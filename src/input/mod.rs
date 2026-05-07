use std::time::{Duration, SystemTime};

pub mod injector;
pub mod reader;

pub use reader::{enumerate_keyboards, spawn_reader};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyKind {
    Press,
    Release,
    Repeat,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId(pub String);

#[derive(Debug, Clone)]
pub struct KeyEvent {
    pub kind: KeyKind,
    /// raw evdev keycode (0..247), without the +8 xkb offset.
    pub keycode: u32,
    pub timestamp: SystemTime,
    pub device: DeviceId,
}

impl KeyEvent {
    pub fn since(&self, other: &KeyEvent) -> Duration {
        self.timestamp
            .duration_since(other.timestamp)
            .unwrap_or(Duration::ZERO)
    }
}
