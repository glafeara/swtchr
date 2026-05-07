use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tokio::sync::mpsc;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use swtchr::config::Config;
use swtchr::control;
use swtchr::input::{KeyKind, enumerate_keyboards, spawn_reader};
use swtchr::service;
use swtchr::xkb::XkbState;

#[derive(Parser, Debug)]
#[command(
    name = "swtchr",
    version,
    about = "Keyboard layout autoswitcher (en↔ru) for Hyprland"
)]
struct Cli {
    /// Override config file path.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Decode-only mode: print decoded UTF-8 from one keyboard, no injection.
    /// Useful for verifying xkb wiring (M1 milestone).
    #[arg(long)]
    decode_only: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Send a swap-selection command to the running daemon.
    Swap,
    /// Health-check the running daemon.
    Ping,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();

    if let Some(cmd) = cli.command {
        let req = match cmd {
            Commands::Swap => control::Command::Swap,
            Commands::Ping => control::Command::Ping,
        };
        let reply = control::send(req).await?;
        println!("{reply}");
        return Ok(());
    }

    let cfg_path = cli.config.unwrap_or_else(Config::default_path);
    let cfg = Config::load_or_default(&cfg_path)?;
    info!(?cfg_path, "loaded config");

    if cli.decode_only {
        return run_decode_only(&cfg).await;
    }

    service::run(cfg).await?;
    Ok(())
}

#[allow(dead_code)]
fn _service_used() {
    let _ = service::run;
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("swtchr=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

async fn run_decode_only(cfg: &Config) -> anyhow::Result<()> {
    let mut xkb = XkbState::new(&cfg.general.default_layout, "")?;
    info!(layout = %cfg.general.default_layout, "xkb state ready");

    let devices = enumerate_keyboards(&cfg.devices.ignore)?;
    info!(count = devices.len(), "found keyboard devices");

    let (tx, mut rx) = mpsc::channel(256);
    for (path, name) in devices {
        spawn_reader(path, name, tx.clone());
    }
    drop(tx);

    while let Some(ev) = rx.recv().await {
        match ev.kind {
            KeyKind::Press => {
                let d = xkb.process(ev.keycode, KeyKind::Press);
                if let Some(s) = d.utf8 {
                    info!(device = %ev.device.0, code = ev.keycode, ch = %s, "press");
                } else if d.is_modifier {
                    info!(device = %ev.device.0, code = ev.keycode, "modifier-press");
                } else if d.is_navigation {
                    info!(device = %ev.device.0, code = ev.keycode, "nav-press");
                } else {
                    info!(device = %ev.device.0, code = ev.keycode, sym = d.keysym, "press(non-printable)");
                }
            }
            KeyKind::Release => {
                let _ = xkb.process(ev.keycode, KeyKind::Release);
            }
            KeyKind::Repeat => {}
        }
    }
    Ok(())
}

// helper kept for legacy decode-only flag; unused at scale
#[allow(dead_code)]
fn parse_layout(s: &str) -> (String, String) {
    let first = s.split(',').next().unwrap_or("us").trim();
    if let Some((l, v)) = first.split_once('(')
        && let Some(var) = v.strip_suffix(')')
    {
        return (l.to_string(), var.to_string());
    }
    (first.to_string(), String::new())
}

#[allow(dead_code)]
fn _unused_warn() {
    warn!("unreachable");
}
