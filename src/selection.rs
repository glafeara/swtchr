//! On-demand layout swap for selected text.
//!
//! Flow:
//!   1. read the Wayland primary selection via `wl-paste -p` (text the user
//!      has highlighted right now);
//!   2. detect the source layout from the dominant script (cyrillic → ru,
//!      latin → us);
//!   3. reverse-map every char to (keycode, shift) in the source layout;
//!   4. switch Hyprland to the target layout;
//!   5. send Backspace — in every text input I know, this deletes the active
//!      selection in one shot;
//!   6. replay the keycodes — same physical positions, new layout, so the
//!      output is the swapped string.
//!
//! Requires `wl-paste` from `wl-clipboard`. We use the *primary* selection
//! (highlight-only) rather than CLIPBOARD so the user doesn't have to Ctrl+C
//! first and we don't clobber their clipboard.

use std::time::Duration;

use tokio::process::Command;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::core::WordEntry;
use crate::error::{Error, Result};
use crate::hyprland::HyprIpc;
use crate::input::injector::UinputInjector;
use crate::xkb::{XkbState, build_char_map};

/// Read the Wayland primary selection. Empty string if nothing is highlighted.
pub async fn read_primary_selection() -> Result<String> {
    let out = Command::new("wl-paste")
        .args(["-p", "-n"])
        .output()
        .await
        .map_err(|e| Error::Evdev(format!("wl-paste exec: {e}")))?;
    if !out.status.success() {
        // wl-paste exits non-zero when the selection is empty — treat as "".
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim_end_matches('\n')
        .to_string())
}

/// Pick the layout the text was typed in, by counting the dominant script.
/// Returns `None` when neither script wins — there's no swap to make.
pub fn detect_source_layout(text: &str, layouts: &[String]) -> Option<u32> {
    let mut cy = 0usize;
    let mut la = 0usize;
    for c in text.chars() {
        let u = c as u32;
        if (0x0400..=0x04FF).contains(&u) || (0x0500..=0x052F).contains(&u) {
            cy += 1;
        } else if c.is_ascii_alphabetic() {
            la += 1;
        }
    }
    let target = if cy > la {
        "ru"
    } else if la > cy {
        "us"
    } else {
        return None;
    };
    layouts.iter().position(|l| l == target).map(|i| i as u32)
}

/// Reverse-map each char of `text` to a positional `WordEntry` under
/// `source_layout_idx`. Chars without a keycode in that layout are dropped
/// with a warn — there's nothing useful to inject for them.
pub fn text_to_entries(
    text: &str,
    layouts_csv: &str,
    variants_csv: &str,
    source_layout_idx: u32,
) -> Result<Vec<WordEntry>> {
    let map = build_char_map(layouts_csv, variants_csv, source_layout_idx)?;
    let mut out = Vec::with_capacity(text.chars().count());
    for ch in text.chars() {
        if let Some(&(keycode, shift)) = map.get(&ch) {
            out.push(WordEntry { ch, keycode, shift });
        } else {
            warn!(?ch, source_layout_idx, "no keycode in source layout — dropped");
        }
    }
    Ok(out)
}

/// Swap layout on whatever is currently selected. No-op when the selection is
/// empty, the source script is ambiguous, or only one layout is configured.
pub async fn swap_selection(
    xkb: &mut XkbState,
    injector: &mut UinputInjector,
    layouts_csv: &str,
    variants_csv: &str,
) -> Result<()> {
    let text = read_primary_selection().await?;
    if text.is_empty() {
        debug!("swap_selection: empty selection");
        return Ok(());
    }
    let n_layouts = xkb.layouts().len() as u32;
    if n_layouts < 2 {
        debug!("swap_selection: fewer than 2 layouts configured");
        return Ok(());
    }
    let Some(source_idx) = detect_source_layout(&text, xkb.layouts()) else {
        debug!(text = %text, "swap_selection: ambiguous script — abstaining");
        return Ok(());
    };
    let target_idx = (source_idx + 1) % n_layouts;

    let entries = text_to_entries(&text, layouts_csv, variants_csv, source_idx)?;
    if entries.is_empty() {
        debug!("swap_selection: nothing to inject after reverse-mapping");
        return Ok(());
    }

    HyprIpc::switch_layout_to(target_idx).await?;
    // Same settle delay as auto_replay — Hyprland needs a moment to broadcast
    // the layout change before replayed keys are decoded under it.
    sleep(Duration::from_millis(40)).await;

    // Backspace deletes the active selection in every text input I've tested
    // (terminals, GTK, Qt, browsers, Electron). Costs one wasted BS if the
    // selection was already gone — acceptable.
    injector.backspaces(1).await?;
    sleep(Duration::from_millis(20)).await;

    injector.replay_entries(&entries).await?;
    xkb.set_active_layout(target_idx);

    info!(
        chars = entries.len(),
        source_idx, target_idx, "swapped selection"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layouts() -> Vec<String> {
        vec!["us".into(), "ru".into()]
    }

    #[test]
    fn detects_ru_from_cyrillic() {
        assert_eq!(detect_source_layout("привет", &layouts()), Some(1));
    }

    #[test]
    fn detects_us_from_latin() {
        assert_eq!(detect_source_layout("hello world", &layouts()), Some(0));
    }

    #[test]
    fn ambiguous_returns_none() {
        assert_eq!(detect_source_layout("123 !!!", &layouts()), None);
        assert_eq!(detect_source_layout("", &layouts()), None);
        // Equal counts — abstain.
        assert_eq!(detect_source_layout("ab фы", &layouts()), None);
    }

    #[test]
    fn mixed_picks_dominant_script() {
        assert_eq!(detect_source_layout("hello мир привет", &layouts()), Some(1));
        assert_eq!(detect_source_layout("hello world мир", &layouts()), Some(0));
    }

    #[test]
    fn missing_target_layout_returns_none() {
        let only_us = vec!["us".to_string()];
        assert_eq!(detect_source_layout("привет", &only_us), None);
    }

    #[test]
    fn text_to_entries_round_trips_via_us_decode() {
        // Source "руддщ" under RU → keycodes that, under US, type "hello".
        let entries = text_to_entries("руддщ", "us,ru", ",", 1).unwrap();
        assert_eq!(entries.len(), 5);
        let positions: Vec<(u32, bool)> = entries.iter().map(|e| (e.keycode, e.shift)).collect();
        let out = crate::xkb::decode_through_layout("us,ru", ",", 0, &positions).unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn unmappable_chars_dropped() {
        // Cyrillic char in *US* source layout has no keycode — should be skipped.
        let entries = text_to_entries("hi я", "us,ru", ",", 0).unwrap();
        // "h", "i", " " or no space (space=kc57 may or may not produce utf8
        // " " in this iteration range — ranged 1..=83 includes 57). The
        // assertion we *do* make: 'я' is dropped, the latin chars survive.
        assert!(entries.iter().any(|e| e.ch == 'h'));
        assert!(entries.iter().any(|e| e.ch == 'i'));
        assert!(!entries.iter().any(|e| e.ch == 'я'));
    }
}
