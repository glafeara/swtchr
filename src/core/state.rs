//! In-memory state owned by the service: word buffer, layout index,
//! modifier tracker.

use std::time::Instant;

/// One key the user typed that contributed a character to the current word.
/// Stores the raw evdev keycode (so we can replay positionally after a layout
/// switch) and whether shift was held when it was produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordEntry {
    pub ch: char,
    pub keycode: u32,
    pub shift: bool,
}

#[derive(Debug, Default, Clone)]
pub struct WordBuffer {
    pub entries: Vec<WordEntry>,
}

impl WordBuffer {
    pub fn push(&mut self, e: WordEntry) {
        self.entries.push(e);
    }

    pub fn pop(&mut self) -> Option<WordEntry> {
        self.entries.pop()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn text(&self) -> String {
        self.entries.iter().map(|e| e.ch).collect()
    }
}

/// Modifier tracking, independent of xkb. We watch the standard evdev
/// modifier keycodes ourselves so the service has a fast bool to consult
/// without going through xkb on every event.
#[derive(Debug, Default, Clone, Copy)]
pub struct ModState {
    pub shift_l: bool,
    pub shift_r: bool,
    pub ctrl_l: bool,
    pub ctrl_r: bool,
    pub alt_l: bool,
    pub alt_r: bool,
    pub super_l: bool,
    pub super_r: bool,
}

impl ModState {
    pub const KEY_LEFTCTRL: u32 = 29;
    pub const KEY_LEFTSHIFT: u32 = 42;
    pub const KEY_RIGHTSHIFT: u32 = 54;
    pub const KEY_LEFTALT: u32 = 56;
    pub const KEY_LEFTMETA: u32 = 125;
    pub const KEY_RIGHTCTRL: u32 = 97;
    pub const KEY_RIGHTALT: u32 = 100;
    pub const KEY_RIGHTMETA: u32 = 126;

    /// Update on any key press/release. Returns true if `kc` is a modifier.
    pub fn update(&mut self, kc: u32, pressed: bool) -> bool {
        let slot = match kc {
            Self::KEY_LEFTSHIFT => &mut self.shift_l,
            Self::KEY_RIGHTSHIFT => &mut self.shift_r,
            Self::KEY_LEFTCTRL => &mut self.ctrl_l,
            Self::KEY_RIGHTCTRL => &mut self.ctrl_r,
            Self::KEY_LEFTALT => &mut self.alt_l,
            Self::KEY_RIGHTALT => &mut self.alt_r,
            Self::KEY_LEFTMETA => &mut self.super_l,
            Self::KEY_RIGHTMETA => &mut self.super_r,
            _ => return false,
        };
        *slot = pressed;
        true
    }

    pub fn shift(&self) -> bool {
        self.shift_l || self.shift_r
    }
    pub fn ctrl(&self) -> bool {
        self.ctrl_l || self.ctrl_r
    }
    pub fn alt(&self) -> bool {
        self.alt_l || self.alt_r
    }
    pub fn meta(&self) -> bool {
        self.super_l || self.super_r
    }

    /// Any non-shift modifier held — the kind that means "this is a hotkey,
    /// don't put the resulting key in the word buffer".
    pub fn any_command_modifier(&self) -> bool {
        self.ctrl() || self.alt() || self.meta()
    }
}

/// A finished word that the detector chose not to act on, kept around so a
/// later mislayout-trigger on the *next* word can retroactively fix it too.
///
/// Use case: user types "d ghbdtn " (intending "в привет ") in EN. The "d"
/// finishes as Unknown (1 char, below dict-path floor). The "ghbdtn" finishes
/// as MisLayout. Without this snapshot, only "ghbdtn" is converted and "d"
/// stays orphaned in the wrong layout. With it, we backspace through both
/// tokens and replay them through the new layout.
#[derive(Debug, Clone)]
pub struct PrevToken {
    pub entries: Vec<WordEntry>,
    pub boundary_kc: u32,
    pub finished_at: Instant,
    pub layout_idx_when_typed: u32,
}

#[derive(Debug, Default)]
pub struct CoreState {
    pub mods: ModState,
    pub buffer: WordBuffer,
    pub layout_idx: u32,
    pub last_event_at: Option<Instant>,
    pub replaying: bool,
    pub seq: u64,
    pub prev_token: Option<PrevToken>,
}

impl CoreState {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modstate_tracks_shift() {
        let mut m = ModState::default();
        assert!(!m.shift());
        assert!(m.update(ModState::KEY_LEFTSHIFT, true));
        assert!(m.shift());
        assert!(m.update(ModState::KEY_LEFTSHIFT, false));
        assert!(!m.shift());
    }

    #[test]
    fn modstate_ignores_letters() {
        let mut m = ModState::default();
        assert!(!m.update(30, true)); // KEY_A
        assert!(!m.shift());
    }
}
