//! Virtual keyboard built on /dev/uinput. Used to send Backspace and replay
//! keycodes after a layout switch.
//!
//! The device name is `swtchr-virtual` and our reader filters events from
//! devices with that exact name to avoid feedback loops.

use std::time::Duration;

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};
use tokio::time::sleep;
use tracing::{debug, warn};

use crate::core::WordEntry;
use crate::error::{Error, Result};
use crate::input::reader::VIRTUAL_DEVICE_NAME;

/// Tiny pause between batched emissions. Without this, very fast streams of
/// synthetic events occasionally race the focused client's input handling.
const INTER_EVENT_PAUSE: Duration = Duration::from_micros(800);

pub struct UinputInjector {
    device: VirtualDevice,
}

impl UinputInjector {
    pub fn build() -> Result<Self> {
        // Register every key in 0..255. Cheap and keeps replay layout-agnostic.
        let mut keys = AttributeSet::<KeyCode>::new();
        for kc in 1..=255u16 {
            keys.insert(KeyCode::new(kc));
        }

        let device = VirtualDevice::builder()
            .map_err(|e| Error::Evdev(format!("uinput builder: {e}")))?
            .name(VIRTUAL_DEVICE_NAME)
            .with_keys(&keys)
            .map_err(|e| Error::Evdev(format!("uinput with_keys: {e}")))?
            .build()
            .map_err(|e| Error::Evdev(format!("uinput build: {e}")))?;

        debug!(name = VIRTUAL_DEVICE_NAME, "uinput device created");
        Ok(Self { device })
    }

    fn emit(&mut self, events: &[InputEvent]) -> Result<()> {
        self.device
            .emit(events)
            .map_err(|e| Error::Evdev(format!("emit: {e}")))
    }

    fn key(code: u16, value: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, code, value)
    }

    pub async fn press_release(&mut self, keycode: u16) -> Result<()> {
        // Press and release in separate SYN_REPORT frames. Some IM/text
        // clients (notably fcitx5 + GTK text inputs) drop a key event when
        // press and release land in the same frame.
        self.emit(&[Self::key(keycode, 1)])?;
        sleep(INTER_EVENT_PAUSE).await;
        self.emit(&[Self::key(keycode, 0)])?;
        sleep(INTER_EVENT_PAUSE).await;
        Ok(())
    }

    pub async fn backspaces(&mut self, n: usize) -> Result<()> {
        // KEY_BACKSPACE = 14
        for _ in 0..n {
            self.press_release(14).await?;
        }
        Ok(())
    }

    /// Replay a previously-typed sequence, applying shift around entries that
    /// need it. Caller must have already switched the focused-keyboard layout
    /// before invoking this.
    pub async fn replay_entries(&mut self, entries: &[WordEntry]) -> Result<()> {
        // KEY_LEFTSHIFT = 42, KEY_RIGHTSHIFT = 54
        const SHIFT: u16 = 42;
        const RSHIFT: u16 = 54;

        // Defensive: clear any shift the user happens to be holding when
        // replay starts. wlroots aggregates modifier state per seat, so this
        // doesn't always cancel the real keyboard's hold, but it's cheap and
        // covers the case where the trigger is a non-modifier path.
        self.emit(&[Self::key(SHIFT, 0), Self::key(RSHIFT, 0)])?;
        sleep(INTER_EVENT_PAUSE).await;

        let mut shift_held = false;

        for e in entries {
            let kc = u16::try_from(e.keycode).map_err(|_| {
                Error::Evdev(format!("keycode {} out of range for uinput", e.keycode))
            })?;
            if e.shift && !shift_held {
                self.emit(&[Self::key(SHIFT, 1)])?;
                shift_held = true;
            } else if !e.shift && shift_held {
                self.emit(&[Self::key(SHIFT, 0)])?;
                shift_held = false;
            }
            self.emit(&[Self::key(kc, 1)])?;
            sleep(INTER_EVENT_PAUSE).await;
            self.emit(&[Self::key(kc, 0)])?;
            sleep(INTER_EVENT_PAUSE).await;
        }
        if shift_held {
            self.emit(&[Self::key(SHIFT, 0)])?;
        }
        Ok(())
    }
}

impl Drop for UinputInjector {
    fn drop(&mut self) {
        // Releasing the VirtualDevice destroys the uinput node; nothing to do.
        if let Err(e) = self.device.emit(&[]) {
            warn!(error = %e, "final uinput sync failed");
        }
    }
}
