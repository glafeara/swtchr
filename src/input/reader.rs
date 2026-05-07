//! Per-device evdev reader. Streams `KeyEvent`s to the service via mpsc.
//!
//! Devices are filtered by name: anything matching our virtual-device sysname
//! is skipped to avoid feedback loops with our own injector.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use evdev::{AttributeSet, Device, EventSummary, KeyCode};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::input::{DeviceId, KeyEvent, KeyKind};

/// Sysname we assign to our injection device — readers skip events from it.
pub const VIRTUAL_DEVICE_NAME: &str = "swtchr-virtual";

/// Walk /dev/input and return paths of devices that look like keyboards
/// (have at least the alphanumeric key range), excluding our own virtual one
/// and any names listed in `ignore`.
pub fn enumerate_keyboards(ignore: &[String]) -> Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    for (path, dev) in evdev::enumerate() {
        let name = dev.name().unwrap_or("").to_string();
        if name == VIRTUAL_DEVICE_NAME {
            continue;
        }
        if ignore.iter().any(|n| n == &name) {
            debug!(device = %name, "skipping ignored device");
            continue;
        }
        if !looks_like_keyboard(&dev) {
            continue;
        }
        out.push((path, name));
    }
    if out.is_empty() {
        return Err(Error::NoKeyboards);
    }
    Ok(out)
}

fn looks_like_keyboard(dev: &Device) -> bool {
    let Some(keys) = dev.supported_keys() else {
        return false;
    };
    // Heuristic: require at least the lowercase-A key. Mice and touchpads
    // expose BTN_* but not letters.
    keys.contains(KeyCode::KEY_A)
        && keys.contains(KeyCode::KEY_Z)
        && keys.contains(KeyCode::KEY_SPACE)
}

/// Spawn a reader task for a single device. Sends `KeyEvent` per real keypress.
/// Repeat events (value=2) are forwarded as `KeyKind::Repeat`; the consumer
/// decides whether to drop them.
pub fn spawn_reader(path: PathBuf, name: String, tx: mpsc::Sender<KeyEvent>) {
    tokio::spawn(async move {
        if let Err(e) = run_reader(path.clone(), name.clone(), tx).await {
            warn!(?path, %name, error = %e, "reader task exited");
        }
    });
}

async fn run_reader(path: PathBuf, name: String, tx: mpsc::Sender<KeyEvent>) -> Result<()> {
    let device = Device::open(&path).map_err(|e| Error::Evdev(format!("open {path:?}: {e}")))?;
    info!(?path, %name, "opened keyboard device");
    let device_id = DeviceId(name.clone());

    let mut stream = device
        .into_event_stream()
        .map_err(|e| Error::Evdev(format!("into_event_stream: {e}")))?;

    loop {
        let event = match stream.next_event().await {
            Ok(e) => e,
            Err(e) => {
                warn!(?path, %name, error = %e, "stream ended");
                return Ok(());
            }
        };

        let summary = event.destructure();
        let EventSummary::Key(_, code, value) = summary else {
            continue;
        };
        let kind = match value {
            0 => KeyKind::Release,
            1 => KeyKind::Press,
            2 => KeyKind::Repeat,
            _ => continue,
        };

        let ke = KeyEvent {
            kind,
            keycode: code.code() as u32,
            timestamp: event.timestamp(),
            device: device_id.clone(),
        };

        if tx.send(ke).await.is_err() {
            return Ok(());
        }
    }
}

/// Best-effort: returns true if the user has read access to `path`.
pub fn can_read(path: &Path) -> bool {
    std::fs::OpenOptions::new().read(true).open(path).is_ok()
}

// Suppress unused-import lint until enumerate's body uses AttributeSet directly.
#[allow(dead_code)]
fn _link_attribute_set(_: &AttributeSet<KeyCode>) {}

// Avoid unused-import warning if SystemTime is referenced only conditionally.
#[allow(dead_code)]
fn _link_systemtime(_: SystemTime) {}
