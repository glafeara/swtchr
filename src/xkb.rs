//! Decode evdev keycodes into UTF-8 using libxkbcommon.
//!
//! Note the +8 offset: evdev keycodes are 0..247 while xkb expects 8..255.
//! This matches what Hyprland (and X11 before it) feeds into xkb.
//!
//! `XkbState` understands a multi-layout keymap (e.g. `us,ru`) and exposes
//! `set_active_layout(idx)` so we can mirror Hyprland's per-keyboard active
//! layout without rebuilding the keymap on every switch.

use std::collections::HashMap;

use xkbcommon::xkb;

use crate::error::{Error, Result};
use crate::input::KeyKind;

pub struct XkbState {
    keymap: xkb::Keymap,
    state: xkb::State,
    layouts: Vec<String>,
    variants: Vec<String>,
    active: u32,
}

#[derive(Debug, Default, Clone)]
pub struct Decoded {
    pub utf8: Option<String>,
    pub keysym: u32,
    pub is_modifier: bool,
    pub is_navigation: bool,
}

impl XkbState {
    pub fn new(layouts: &str, variants: &str) -> Result<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "",
            "",
            layouts,
            variants,
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .ok_or_else(|| Error::XkbCompile {
            layout: layouts.into(),
            variant: variants.into(),
        })?;
        let state = xkb::State::new(&keymap);
        Ok(Self {
            keymap,
            state,
            layouts: layouts
                .split(',')
                .map(str::trim)
                .map(str::to_string)
                .collect(),
            variants: variants
                .split(',')
                .map(str::trim)
                .map(str::to_string)
                .collect(),
            active: 0,
        })
    }

    pub fn layouts(&self) -> &[String] {
        &self.layouts
    }

    pub fn variants(&self) -> &[String] {
        &self.variants
    }

    pub fn active_index(&self) -> u32 {
        self.active
    }

    /// Lock the active layout to `idx`. Resets transient modifier state — call
    /// this on Hyprland `activelayout` events, not in the keystroke hot path.
    pub fn set_active_layout(&mut self, idx: u32) {
        self.active = idx;
        self.state = xkb::State::new(&self.keymap);
        let _ = self.state.update_mask(0, 0, 0, 0, 0, idx);
    }

    pub fn process(&mut self, evdev_keycode: u32, kind: KeyKind) -> Decoded {
        let xkb_kc: xkb::Keycode = (evdev_keycode + 8).into();

        if matches!(kind, KeyKind::Repeat) {
            let utf8 = self.state.key_get_utf8(xkb_kc);
            let keysym = self.state.key_get_one_sym(xkb_kc);
            return Decoded {
                utf8: (!utf8.is_empty()).then_some(utf8),
                keysym: keysym.raw(),
                is_modifier: is_modifier_keysym(keysym.raw()),
                is_navigation: is_navigation_keysym(keysym.raw()),
            };
        }

        let direction = match kind {
            KeyKind::Press => xkb::KeyDirection::Down,
            KeyKind::Release => xkb::KeyDirection::Up,
            KeyKind::Repeat => unreachable!(),
        };

        let utf8 = if matches!(kind, KeyKind::Press) {
            self.state.key_get_utf8(xkb_kc)
        } else {
            String::new()
        };
        let keysym = self.state.key_get_one_sym(xkb_kc);
        let _changed = self.state.update_key(xkb_kc, direction);

        Decoded {
            utf8: (!utf8.is_empty()).then_some(utf8),
            keysym: keysym.raw(),
            is_modifier: is_modifier_keysym(keysym.raw()),
            is_navigation: is_navigation_keysym(keysym.raw()),
        }
    }
}

fn is_modifier_keysym(sym: u32) -> bool {
    use xkbcommon::xkb::keysyms as ks;
    matches!(
        sym,
        ks::KEY_Shift_L
            | ks::KEY_Shift_R
            | ks::KEY_Control_L
            | ks::KEY_Control_R
            | ks::KEY_Alt_L
            | ks::KEY_Alt_R
            | ks::KEY_Meta_L
            | ks::KEY_Meta_R
            | ks::KEY_Super_L
            | ks::KEY_Super_R
            | ks::KEY_Hyper_L
            | ks::KEY_Hyper_R
            | ks::KEY_Caps_Lock
            | ks::KEY_Shift_Lock
            | ks::KEY_Num_Lock
            | ks::KEY_ISO_Level3_Shift
            | ks::KEY_ISO_Level5_Shift
    )
}

fn is_navigation_keysym(sym: u32) -> bool {
    use xkbcommon::xkb::keysyms as ks;
    matches!(
        sym,
        ks::KEY_Left
            | ks::KEY_Right
            | ks::KEY_Up
            | ks::KEY_Down
            | ks::KEY_Home
            | ks::KEY_End
            | ks::KEY_Page_Up
            | ks::KEY_Page_Down
            | ks::KEY_Escape
    )
}

/// Compute what a sequence of `(keycode, shift)` would produce in a *specific*
/// layout, regardless of the caller's current state. Used by the detector to
/// answer "what would these positional keys look like in the OTHER layout?"
pub fn decode_through_layout(
    layouts: &str,
    variants: &str,
    layout_idx: u32,
    entries: &[(u32, bool)],
) -> Result<String> {
    let mut state = XkbState::new(layouts, variants)?;
    state.set_active_layout(layout_idx);

    const SHIFT_KC: u32 = 42; // KEY_LEFTSHIFT (evdev)
    let mut shift_held = false;
    let mut out = String::new();

    for &(kc, shift) in entries {
        if shift && !shift_held {
            state.process(SHIFT_KC, KeyKind::Press);
            shift_held = true;
        } else if !shift && shift_held {
            state.process(SHIFT_KC, KeyKind::Release);
            shift_held = false;
        }
        let d = state.process(kc, KeyKind::Press);
        if let Some(s) = d.utf8 {
            out.push_str(&s);
        }
        state.process(kc, KeyKind::Release);
    }
    if shift_held {
        state.process(SHIFT_KC, KeyKind::Release);
    }
    Ok(out)
}

/// Reverse map: unicode char → (evdev keycode, shift) for `layout_idx`. Used
/// by selection-swap to turn an arbitrary string back into positional keys we
/// can replay through uinput.
///
/// Walks the main keycode block (1..=83 covers letters/digits/punctuation in
/// evdev) twice — first unshifted, then with Shift latched in the mask.
/// `or_insert` biases toward the unshifted form when both reach the same char.
/// AltGr layers (ISO_Level3_Shift) are not enumerated; en/ru don't need them.
///
/// We drive the state via `update_mask` directly rather than `update_key` +
/// `set_active_layout` — mixing the two leaves the "Shift held" bit in an
/// inconsistent state on some xkbcommon versions.
pub fn build_char_map(
    layouts: &str,
    variants: &str,
    layout_idx: u32,
) -> Result<HashMap<char, (u32, bool)>> {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_names(
        &context,
        "",
        "",
        layouts,
        variants,
        None,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .ok_or_else(|| Error::XkbCompile {
        layout: layouts.into(),
        variant: variants.into(),
    })?;

    let shift_idx = keymap.mod_get_index(xkb::MOD_NAME_SHIFT);
    let shift_mask: xkb::ModMask = 1 << shift_idx;
    let mut map = HashMap::new();

    for &shifted in &[false, true] {
        let mut state = xkb::State::new(&keymap);
        let depressed = if shifted { shift_mask } else { 0 };
        state.update_mask(depressed, 0, 0, 0, 0, layout_idx);
        for kc in 1..=83u32 {
            let xkb_kc: xkb::Keycode = (kc + 8).into();
            let s = state.key_get_utf8(xkb_kc);
            if s.is_empty() {
                continue;
            }
            if let Some(ch) = s.chars().next()
                && !ch.is_control()
            {
                map.entry(ch).or_insert((kc, shifted));
            }
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// evdev keycode for KEY_A is 30; +8 = 38, mapping to "a" in US layout.
    #[test]
    fn decodes_a_in_us_layout() {
        let mut s = XkbState::new("us", "").unwrap();
        let d = s.process(30, KeyKind::Press);
        assert_eq!(d.utf8.as_deref(), Some("a"));
        assert!(!d.is_modifier);
        assert!(!d.is_navigation);
    }

    #[test]
    fn shift_changes_case() {
        let mut s = XkbState::new("us", "").unwrap();
        let d_shift = s.process(42, KeyKind::Press); // KEY_LEFTSHIFT
        assert!(d_shift.is_modifier);
        assert!(d_shift.utf8.is_none());
        let d_a = s.process(30, KeyKind::Press);
        assert_eq!(d_a.utf8.as_deref(), Some("A"));
    }

    #[test]
    fn ru_layout_produces_cyrillic() {
        let mut s = XkbState::new("ru", "").unwrap();
        let d = s.process(30, KeyKind::Press);
        assert_eq!(d.utf8.as_deref(), Some("ф"));
    }

    #[test]
    fn arrow_classified_as_navigation() {
        let mut s = XkbState::new("us", "").unwrap();
        let d = s.process(105, KeyKind::Press); // KEY_LEFT
        assert!(d.is_navigation);
    }

    #[test]
    fn multi_layout_switching() {
        let mut s = XkbState::new("us,ru", ",").unwrap();
        assert_eq!(s.layouts(), &["us".to_string(), "ru".to_string()]);
        assert_eq!(s.active_index(), 0);
        assert_eq!(s.process(30, KeyKind::Press).utf8.as_deref(), Some("a"));
        s.process(30, KeyKind::Release);
        s.set_active_layout(1);
        assert_eq!(s.active_index(), 1);
        assert_eq!(s.process(30, KeyKind::Press).utf8.as_deref(), Some("ф"));
    }

    /// "ghbdtn" in US layout maps positionally to "привет" in RU layout.
    /// keycodes: g=34, h=35, b=48, d=32, t=20, n=49.
    #[test]
    fn decode_ghbdtn_through_ru_yields_privet() {
        let entries = [
            (34, false),
            (35, false),
            (48, false),
            (32, false),
            (20, false),
            (49, false),
        ];
        let out = decode_through_layout("us,ru", ",", 1, &entries).unwrap();
        assert_eq!(out, "привет");
    }

    #[test]
    fn char_map_us_basic_letters() {
        let m = build_char_map("us,ru", ",", 0).unwrap();
        assert_eq!(m.get(&'a'), Some(&(30, false)));
        assert_eq!(m.get(&'A'), Some(&(30, true)));
        assert_eq!(m.get(&'h'), Some(&(35, false)));
    }

    #[test]
    fn char_map_ru_letters() {
        let m = build_char_map("us,ru", ",", 1).unwrap();
        // р is on US 'h' (kc 35), у is on US 'e' (kc 18), д is on US 'l' (kc 38).
        assert_eq!(m.get(&'р'), Some(&(35, false)));
        assert_eq!(m.get(&'у'), Some(&(18, false)));
        assert_eq!(m.get(&'д'), Some(&(38, false)));
    }

    /// Round-trip: take "руддщ", reverse-map under RU, decode positions through
    /// US — should yield "hello". Same physical keys, swapped layout.
    #[test]
    fn round_trip_ru_to_us() {
        let m = build_char_map("us,ru", ",", 1).unwrap();
        let entries: Vec<(u32, bool)> =
            "руддщ".chars().map(|c| *m.get(&c).unwrap()).collect();
        let out = decode_through_layout("us,ru", ",", 0, &entries).unwrap();
        assert_eq!(out, "hello");
    }

    /// And the inverse: "ghbdtn" in US.
    #[test]
    fn decode_ghbdtn_through_us_yields_ghbdtn() {
        let entries = [
            (34, false),
            (35, false),
            (48, false),
            (32, false),
            (20, false),
            (49, false),
        ];
        let out = decode_through_layout("us,ru", ",", 0, &entries).unwrap();
        assert_eq!(out, "ghbdtn");
    }
}
