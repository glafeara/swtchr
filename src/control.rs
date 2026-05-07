//! Local control socket. A single-line text protocol over
//! `$XDG_RUNTIME_DIR/swtchr.sock` (fallback `/tmp/swtchr-$UID.sock`).
//!
//! Commands (one per connection, terminated by newline or EOF):
//!   - `swap` — swap layout on the current Wayland selection.
//!   - `ping` — health probe; replies `ok`.
//!
//! The reply is a single line: `ok\n` on success, `err: <message>\n` on
//! failure. The socket is created with mode 0600 (owner-only) and the path is
//! per-user, so any process running as the same UID can drive the daemon —
//! good enough for a desktop helper, no auth needed.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};

/// Cap incoming command line length — anything longer is malformed and we'd
/// rather drop the connection than buffer unbounded data.
const MAX_LINE: usize = 64;
/// Hard ceiling on a request: even `swap` should resolve fast. Keeps a stuck
/// or hostile client from holding the listener open.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Swap,
    Ping,
}

impl Command {
    fn parse(line: &str) -> Option<Self> {
        match line.trim() {
            "swap" => Some(Self::Swap),
            "ping" => Some(Self::Ping),
            _ => None,
        }
    }
}

/// Default socket path: `$XDG_RUNTIME_DIR/swtchr.sock`, falling back to
/// `/tmp/swtchr-$UID.sock` if `XDG_RUNTIME_DIR` is unset.
pub fn socket_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("swtchr.sock");
    }
    let uid = unsafe_uid();
    PathBuf::from(format!("/tmp/swtchr-{uid}.sock"))
}

fn unsafe_uid() -> u32 {
    // libc::getuid is the obvious answer, but we forbid `unsafe_code` on the
    // crate. Read /proc/self/loginuid as a portable-enough proxy; failure
    // collapses to 0 (root) which is fine for a fallback path.
    std::fs::read_to_string("/proc/self/loginuid")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// Spawn the listener task. Each accepted connection reads one command line
/// and forwards a `CoreMsg::SwapSelection` (or replies inline for `ping`).
pub fn spawn_listener(tx: mpsc::Sender<crate::service::CoreMsg>) -> Result<()> {
    let path = socket_path();
    if path.exists() {
        // Stale socket from a previous run. Removing is safe: bind would fail
        // otherwise, and two daemons on the same UID is unsupported anyway.
        if let Err(e) = std::fs::remove_file(&path) {
            warn!(?path, error = %e, "could not remove stale socket; bind may fail");
        }
    }
    let listener = UnixListener::bind(&path)
        .map_err(|e| Error::Evdev(format!("control bind {path:?}: {e}")))?;
    set_owner_only(&path)?;
    info!(?path, "control socket listening");

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, tx).await {
                            warn!(error = %e, "control connection failed");
                        }
                    });
                }
                Err(e) => {
                    warn!(error = %e, "control accept failed");
                    // Brief backoff so a persistent fd-exhaustion doesn't spin.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    });
    Ok(())
}

async fn handle_connection(
    stream: UnixStream,
    tx: mpsc::Sender<crate::service::CoreMsg>,
) -> Result<()> {
    let res = timeout(REQUEST_TIMEOUT, async {
        let (rd, mut wr) = stream.into_split();
        let mut rd = BufReader::new(rd).take(MAX_LINE as u64 + 1);
        let mut line = String::new();
        rd.read_line(&mut line)
            .await
            .map_err(|e| Error::Evdev(format!("control read: {e}")))?;
        if line.len() > MAX_LINE {
            let _ = wr.write_all(b"err: line too long\n").await;
            return Ok(());
        }
        let reply: String = match Command::parse(&line) {
            Some(Command::Swap) => {
                if tx.send(crate::service::CoreMsg::SwapSelection).await.is_err() {
                    "err: service stopped\n".into()
                } else {
                    // We dispatch async — the caller doesn't get a real
                    // success/failure for the swap itself, only that the
                    // command was queued.
                    "ok\n".into()
                }
            }
            Some(Command::Ping) => "ok\n".into(),
            None => format!("err: unknown command: {:?}\n", line.trim()),
        };
        let _ = wr.write_all(reply.as_bytes()).await;
        let _ = wr.shutdown().await;
        Ok::<(), Error>(())
    })
    .await;
    match res {
        Ok(inner) => inner,
        Err(_) => {
            debug!("control request timed out");
            Ok(())
        }
    }
}

fn set_owner_only(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)
        .map_err(|e| Error::Evdev(format!("chmod 0600 {path:?}: {e}")))
}

/// Connect to the daemon and send a single command, returning its reply.
/// Used by the `swtchr swap` / `swtchr ping` CLI subcommands.
pub async fn send(cmd: Command) -> Result<String> {
    let path = socket_path();
    let stream = UnixStream::connect(&path)
        .await
        .map_err(|e| Error::Evdev(format!("connect {path:?}: {e}")))?;
    let (rd, mut wr) = stream.into_split();
    let line = match cmd {
        Command::Swap => "swap\n",
        Command::Ping => "ping\n",
    };
    wr.write_all(line.as_bytes())
        .await
        .map_err(|e| Error::Evdev(format!("control write: {e}")))?;
    wr.shutdown()
        .await
        .map_err(|e| Error::Evdev(format!("control shutdown: {e}")))?;
    let mut reader = BufReader::new(rd);
    let mut reply = String::new();
    reader
        .read_line(&mut reply)
        .await
        .map_err(|e| Error::Evdev(format!("control read reply: {e}")))?;
    Ok(reply.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_commands() {
        assert_eq!(Command::parse("swap"), Some(Command::Swap));
        assert_eq!(Command::parse("swap\n"), Some(Command::Swap));
        assert_eq!(Command::parse(" ping "), Some(Command::Ping));
        assert_eq!(Command::parse("nope"), None);
        assert_eq!(Command::parse(""), None);
    }

    #[tokio::test]
    async fn round_trip_ping() {
        // Use a temp socket path so we don't clash with a real daemon.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("swtchr.sock");
        // Bind directly — bypass `socket_path()` for the test.
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, mut rx) = mpsc::channel::<crate::service::CoreMsg>(8);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, tx).await.unwrap();
        });

        let stream = UnixStream::connect(&path).await.unwrap();
        let (rd, mut wr) = stream.into_split();
        wr.write_all(b"ping\n").await.unwrap();
        wr.shutdown().await.unwrap();
        let mut reader = BufReader::new(rd);
        let mut reply = String::new();
        reader.read_line(&mut reply).await.unwrap();
        assert_eq!(reply.trim(), "ok");
        assert!(rx.try_recv().is_err()); // ping does not enqueue
    }

    #[tokio::test]
    async fn swap_command_enqueues_msg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("swtchr.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, mut rx) = mpsc::channel::<crate::service::CoreMsg>(8);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, tx).await.unwrap();
        });

        let stream = UnixStream::connect(&path).await.unwrap();
        let (rd, mut wr) = stream.into_split();
        wr.write_all(b"swap\n").await.unwrap();
        wr.shutdown().await.unwrap();
        let mut reader = BufReader::new(rd);
        let mut reply = String::new();
        reader.read_line(&mut reply).await.unwrap();
        assert_eq!(reply.trim(), "ok");
        let msg = rx.recv().await.unwrap();
        assert!(matches!(msg, crate::service::CoreMsg::SwapSelection));
    }

    #[tokio::test]
    async fn unknown_command_replies_err() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("swtchr.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, _rx) = mpsc::channel::<crate::service::CoreMsg>(8);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, tx).await.unwrap();
        });

        let stream = UnixStream::connect(&path).await.unwrap();
        let (rd, mut wr) = stream.into_split();
        wr.write_all(b"weird\n").await.unwrap();
        wr.shutdown().await.unwrap();
        let mut reader = BufReader::new(rd);
        let mut reply = String::new();
        reader.read_line(&mut reply).await.unwrap();
        assert!(reply.starts_with("err:"));
    }
}
