//! Minimal Hyprland IPC client.
//!
//! Two sockets, both under `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`:
//! - `.socket.sock`  — request/response. We use it to dispatch `switchxkblayout`
//!   and to query `getoption` / `devices`.
//! - `.socket2.sock` — line-delimited event stream `EVENT>>DATA\n`.

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};

const INSTANCE_ENV: &str = "HYPRLAND_INSTANCE_SIGNATURE";

#[derive(Debug, Clone)]
pub enum HyprEvent {
    /// Layout (xkb group) changed on a specific keyboard. Hyprland's payload
    /// is the human-readable layout name; we forward it raw and let the caller
    /// re-query `hyprctl devices` to map back to an index.
    ActiveLayout {
        keyboard: String,
        layout_name: String,
    },
    /// Active window changed. We use it as a buffer-reset signal.
    ActiveWindow,
}

pub struct HyprIpc;

impl HyprIpc {
    fn runtime_dir() -> Result<PathBuf> {
        let sig = env::var(INSTANCE_ENV).map_err(|_| {
            Error::Evdev(format!(
                "{INSTANCE_ENV} not set — is this running inside a Hyprland session?"
            ))
        })?;
        let xdg = env::var("XDG_RUNTIME_DIR").map_err(|_| {
            Error::Evdev("XDG_RUNTIME_DIR not set".into())
        })?;
        Ok(PathBuf::from(xdg).join("hypr").join(sig))
    }

    fn socket_path() -> Result<PathBuf> {
        Ok(Self::runtime_dir()?.join(".socket.sock"))
    }

    fn event_socket_path() -> Result<PathBuf> {
        Ok(Self::runtime_dir()?.join(".socket2.sock"))
    }

    /// Send a single command on `.socket.sock` and return the response body.
    pub async fn request(cmd: &str) -> Result<String> {
        let path = Self::socket_path()?;
        let mut sock = UnixStream::connect(&path)
            .await
            .map_err(|e| Error::Evdev(format!("connect {path:?}: {e}")))?;
        sock.write_all(cmd.as_bytes())
            .await
            .map_err(|e| Error::Evdev(format!("write: {e}")))?;
        sock.shutdown()
            .await
            .map_err(|e| Error::Evdev(format!("shutdown: {e}")))?;
        let mut buf = String::new();
        sock.read_to_string(&mut buf)
            .await
            .map_err(|e| Error::Evdev(format!("read: {e}")))?;
        Ok(buf)
    }

    /// `hyprctl switchxkblayout current next`-equivalent. Cycles to the next
    /// layout configured in Hyprland's input section.
    /// Pin every keyboard to layout group `idx`. Absolute index instead of
    /// `next`/`prev` because `next` cycles each keyboard independently — with
    /// `grp:win_space_toggle` or fcitx5 in the mix, keyboards drift out of
    /// sync, and a relative cycle can leave swtchr-virtual in the wrong
    /// layout for the replay (especially on RU→EN).
    pub async fn switch_layout_to(idx: u32) -> Result<()> {
        let cmd = format!("switchxkblayout all {idx}");
        let resp = Self::request(&cmd).await?;
        debug!(?resp, %cmd, "switchxkblayout response");
        if !resp.starts_with("ok") {
            warn!(?resp, %cmd, "switchxkblayout returned non-ok");
        }
        Ok(())
    }

    /// Query `hyprctl getoption -j input:kb_layout` — returns the comma-
    /// separated list of layouts ("us,ru").
    pub async fn get_layouts() -> Result<String> {
        let raw = Self::request("/getoption input:kb_layout").await?;
        // Plain (non-JSON) form: e.g. "str: us,ru"
        for line in raw.lines() {
            if let Some(rest) = line.strip_prefix("str: ") {
                return Ok(rest.trim().to_string());
            }
        }
        Ok("us".to_string())
    }

    pub async fn get_variants() -> Result<String> {
        let raw = Self::request("/getoption input:kb_variant").await?;
        for line in raw.lines() {
            if let Some(rest) = line.strip_prefix("str: ") {
                return Ok(rest.trim().to_string());
            }
        }
        Ok(String::new())
    }

    /// Subscribe to the event stream and forward filtered events.
    /// Reconnects on EOF / error with backoff.
    pub fn spawn_event_listener(tx: mpsc::Sender<HyprEvent>) {
        tokio::spawn(async move {
            let mut backoff = Duration::from_millis(200);
            loop {
                if let Err(e) = run_listener(tx.clone()).await {
                    warn!(error = %e, "hyprland event listener errored");
                }
                if tx.is_closed() {
                    return;
                }
                sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        });
    }
}

async fn run_listener(tx: mpsc::Sender<HyprEvent>) -> Result<()> {
    let path = HyprIpc::event_socket_path()?;
    let sock = UnixStream::connect(&path)
        .await
        .map_err(|e| Error::Evdev(format!("connect {path:?}: {e}")))?;
    info!(?path, "subscribed to hyprland events");
    let mut reader = BufReader::new(sock);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| Error::Evdev(format!("read_line: {e}")))?;
        if n == 0 {
            return Err(Error::Evdev("event socket closed".into()));
        }
        let trimmed = line.trim_end_matches('\n');
        let Some((ev, data)) = trimmed.split_once(">>") else {
            continue;
        };
        let parsed = match ev {
            "activelayout" => {
                let mut parts = data.splitn(2, ',');
                let keyboard = parts.next().unwrap_or("").to_string();
                let layout_name = parts.next().unwrap_or("").to_string();
                Some(HyprEvent::ActiveLayout {
                    keyboard,
                    layout_name,
                })
            }
            "activewindow" | "activewindowv2" => Some(HyprEvent::ActiveWindow),
            _ => None,
        };
        if let Some(p) = parsed
            && tx.send(p).await.is_err()
        {
            return Ok(());
        }
    }
}
