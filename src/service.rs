//! Service main loop. Owns `CoreState`, the `XkbState`, the injector, and
//! per-layout dictionaries. Producers (evdev readers, hyprland listener) push
//! to a single mpsc; the loop processes one message at a time so no locking
//! is needed.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::core::{
    CoreState, Dict, HunspellDict, PrevToken, Verdict, WordEntry, classify,
};
use crate::error::Result;
use crate::hyprland::{HyprEvent, HyprIpc};
use crate::input::injector::UinputInjector;
use crate::input::reader::{enumerate_keyboards, spawn_reader};
use crate::input::{KeyEvent, KeyKind};
use crate::xkb::{XkbState, decode_through_layout};

#[derive(Debug)]
pub enum CoreMsg {
    Key(KeyEvent),
    Hypr(HyprEvent),
    /// Triggered by the control socket. Swap layout on whatever is selected.
    SwapSelection,
}

/// evdev keycodes that hard-terminate a word regardless of layout.
fn is_boundary_keycode(kc: u32) -> bool {
    // KEY_SPACE=57, KEY_TAB=15, KEY_ENTER=28, KEY_KPENTER=96
    matches!(kc, 57 | 15 | 28 | 96)
}

fn is_punct_char(s: &str) -> bool {
    matches!(s, "." | "," | ";" | ":" | "!" | "?" | "'" | "\"")
}

/// Map Hyprland's `activelayout` payload (e.g. "English (US)" or "Russian")
/// back to a layout *index* in our xkb keymap. The names are the canonical
/// xkb names from `evdev.lst`.
fn layout_index(xkb: &XkbState, hyprland_name: &str) -> Option<u32> {
    let trimmed = hyprland_name.trim();
    let xkb_code = match trimmed {
        s if s.eq_ignore_ascii_case("English (US)") => "us",
        s if s.eq_ignore_ascii_case("English") => "us",
        s if s.eq_ignore_ascii_case("Russian") => "ru",
        s if s.eq_ignore_ascii_case("Russian (RU)") => "ru",
        _ => return None,
    };
    xkb.layouts()
        .iter()
        .position(|l| l == xkb_code)
        .map(|i| i as u32)
}

/// Boot the service.
pub async fn run(cfg: Config) -> Result<()> {
    Service::build(cfg).await?.run().await
}

pub struct Service {
    cfg: Config,
    state: CoreState,
    xkb: XkbState,
    injector: UinputInjector,
    dicts: HashMap<String, HunspellDict>,
    empty_dict: HunspellDict,
    layouts_csv: String,
    variants_csv: String,
    rx: mpsc::Receiver<CoreMsg>,
    _tx_keep_alive: mpsc::Sender<CoreMsg>,
}

impl Service {
    pub async fn build(cfg: Config) -> Result<Self> {
        let layouts_csv = match HyprIpc::get_layouts().await {
            Ok(s) if !s.is_empty() => s,
            _ => cfg.general.default_layout.clone(),
        };
        let variants_csv = HyprIpc::get_variants().await.unwrap_or_default();
        info!(%layouts_csv, %variants_csv, "xkb layouts");

        let xkb = XkbState::new(&layouts_csv, &variants_csv)?;
        let state = CoreState::new();
        let injector = UinputInjector::build()?;

        let mut dicts = HashMap::new();
        let mut en = HunspellDict::load_or_warn(&cfg.dictionaries.en);
        en.add_words(cfg.dictionaries.extra_words.iter().map(String::as_str));
        dicts.insert("us".to_string(), en);

        let mut ru = HunspellDict::load_or_warn(&cfg.dictionaries.ru);
        ru.add_words(cfg.dictionaries.extra_words.iter().map(String::as_str));
        dicts.insert("ru".to_string(), ru);

        let (tx, rx) = mpsc::channel::<CoreMsg>(256);

        // Hyprland event listener.
        let (htx, mut hrx) = mpsc::channel::<HyprEvent>(64);
        HyprIpc::spawn_event_listener(htx);
        let tx_h = tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = hrx.recv().await {
                if tx_h.send(CoreMsg::Hypr(ev)).await.is_err() {
                    return;
                }
            }
        });

        // Keyboard readers.
        let kbds = enumerate_keyboards(&cfg.devices.ignore)?;
        info!(count = kbds.len(), "keyboard devices");
        let (kex, mut kerx) = mpsc::channel::<KeyEvent>(256);
        for (path, name) in kbds {
            spawn_reader(path, name, kex.clone());
        }
        drop(kex);
        let tx_k = tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = kerx.recv().await {
                if tx_k.send(CoreMsg::Key(ev)).await.is_err() {
                    return;
                }
            }
        });

        // Control socket — feeds CoreMsg::SwapSelection from `swtchr swap`.
        // A bind failure is non-fatal: log and keep serving auto-detection.
        if let Err(e) = crate::control::spawn_listener(tx.clone()) {
            warn!(error = %e, "control socket disabled");
        }

        Ok(Self {
            cfg,
            state,
            xkb,
            injector,
            dicts,
            empty_dict: HunspellDict::empty(),
            layouts_csv,
            variants_csv,
            rx,
            _tx_keep_alive: tx,
        })
    }

    pub async fn run(mut self) -> Result<()> {
        while let Some(msg) = self.rx.recv().await {
            if let Err(e) = self.handle_msg(msg).await {
                warn!(error = %e, "message handler errored");
            }
        }
        Ok(())
    }

    async fn handle_msg(&mut self, msg: CoreMsg) -> Result<()> {
        match msg {
            CoreMsg::Key(ev) => self.handle_key(ev).await,
            CoreMsg::Hypr(HyprEvent::ActiveLayout {
                keyboard,
                layout_name,
            }) => {
                debug!(%keyboard, %layout_name, "hyprland activelayout");
                if let Some(idx) = layout_index(&self.xkb, &layout_name) {
                    self.xkb.set_active_layout(idx);
                    self.state.layout_idx = idx;
                    info!(idx, %layout_name, "synced xkb to hyprland layout");
                } else {
                    warn!(
                        %layout_name,
                        layouts = ?self.xkb.layouts(),
                        "could not map layout name to index"
                    );
                }
                Ok(())
            }
            CoreMsg::Hypr(HyprEvent::ActiveWindow) => {
                self.state.buffer.clear();
                self.state.prev_token = None;
                Ok(())
            }
            CoreMsg::SwapSelection => self.swap_selection().await,
        }
    }

    fn dict_for_layout(&self, idx: u32) -> &dyn Dict {
        let name = self
            .xkb
            .layouts()
            .get(idx as usize)
            .map(String::as_str)
            .unwrap_or("");
        self.dicts
            .get(name)
            .map(|d| d as &dyn Dict)
            .unwrap_or(&self.empty_dict)
    }

    async fn handle_key(&mut self, ev: KeyEvent) -> Result<()> {
        let now = Instant::now();
        // Idle-reset: if the previous event was long enough ago, the user has
        // moved on (paste, focus change, mouse selection) — drop any partial
        // word and any retro-fix candidate so we don't act on stale state.
        if let Some(prev) = self.state.last_event_at {
            let idle = Duration::from_millis(self.cfg.general.idle_reset_ms);
            if now.duration_since(prev) > idle {
                if !self.state.buffer.is_empty() {
                    debug!("idle reset: clearing buffer");
                    self.state.buffer.clear();
                }
                self.state.prev_token = None;
            }
        }
        self.state.last_event_at = Some(now);

        let pressed = matches!(ev.kind, KeyKind::Press);
        let is_mod = self.state.mods.update(ev.keycode, pressed);

        if matches!(ev.kind, KeyKind::Repeat) {
            let _ = self.xkb.process(ev.keycode, KeyKind::Repeat);
            return Ok(());
        }

        let decoded = self.xkb.process(ev.keycode, ev.kind);

        if !pressed {
            return Ok(());
        }
        self.state.seq = self.state.seq.wrapping_add(1);

        // Hotkeys (Ctrl/Alt/Super + X) reset buffer; we never act mid-hotkey.
        if self.state.mods.any_command_modifier() {
            if !is_mod {
                self.state.buffer.clear();
                self.state.prev_token = None;
            }
            return Ok(());
        }

        // Modifier presses don't enter the buffer.
        if is_mod || decoded.is_modifier {
            return Ok(());
        }

        // Navigation / Esc: flush buffer and any retro-fix candidate. Cursor
        // motion means the upcoming text is unrelated to what we just typed.
        if decoded.is_navigation {
            self.state.buffer.clear();
            self.state.prev_token = None;
            return Ok(());
        }

        // Word boundary by keycode.
        if is_boundary_keycode(ev.keycode) {
            return self.on_word_boundary(ev.keycode).await;
        }

        // Backspace: pop one entry.
        if ev.keycode == 14 {
            self.state.buffer.pop();
            return Ok(());
        }

        // Printable utf8.
        if let Some(s) = decoded.utf8.as_deref() {
            // Punctuation usually ends a word — but on the EN/RU pair several
            // keys produce punctuation in EN and a Cyrillic letter in RU
            // (`;`→ж, `,`→б, `.`→ю, `'`→э). If the buffer's swap-form *plus*
            // this key would complete a known word in the other layout, the
            // user is mid-word; push the char instead of firing a boundary.
            if is_punct_char(s) {
                if self.punct_completes_other_lang_word(ev.keycode)
                    && let Some(ch) = s.chars().next()
                    && !ch.is_control()
                {
                    self.state.buffer.push(WordEntry {
                        ch,
                        keycode: ev.keycode,
                        shift: self.state.mods.shift(),
                    });
                    return Ok(());
                }
                return self.on_word_boundary(ev.keycode).await;
            }
            if let Some(ch) = s.chars().next()
                && !ch.is_control()
            {
                self.state.buffer.push(WordEntry {
                    ch,
                    keycode: ev.keycode,
                    shift: self.state.mods.shift(),
                });
            }
        }
        Ok(())
    }

    /// Try the hypothesis "this punctuation key is actually a letter in the
    /// other layout, finishing a word the user is typing." Returns true only
    /// when the swapped form is a complete word in the other dictionary —
    /// otherwise we treat the key as real punctuation.
    fn punct_completes_other_lang_word(&self, kc: u32) -> bool {
        if self.state.buffer.is_empty() {
            return false;
        }
        let cur_idx = self.xkb.active_index();
        let n_layouts = self.xkb.layouts().len() as u32;
        if n_layouts < 2 {
            return false;
        }
        let other_idx = (cur_idx + 1) % n_layouts;
        let mut positional: Vec<(u32, bool)> = self
            .state
            .buffer
            .entries
            .iter()
            .map(|e| (e.keycode, e.shift))
            .collect();
        positional.push((kc, self.state.mods.shift()));
        let Ok(swapped) = decode_through_layout(
            &self.layouts_csv,
            &self.variants_csv,
            other_idx,
            &positional,
        ) else {
            return false;
        };
        self.dict_for_layout(other_idx).lookup(&swapped)
    }

    /// Word-boundary handling: runs the detector and converts on MisLayout.
    async fn on_word_boundary(&mut self, boundary_kc: u32) -> Result<()> {
        if !self.state.buffer.is_empty() {
            self.auto_evaluate(boundary_kc).await?;
        }
        Ok(())
    }

    async fn auto_evaluate(&mut self, boundary_kc: u32) -> Result<()> {
        let entries = self.state.buffer.entries.clone();
        let cur_idx = self.xkb.active_index();
        let n_layouts = self.xkb.layouts().len() as u32;
        if n_layouts < 2 {
            self.state.buffer.clear();
            return Ok(());
        }
        let other_idx = (cur_idx + 1) % n_layouts;

        let current_text: String = entries.iter().map(|e| e.ch).collect();
        let positional: Vec<(u32, bool)> = entries.iter().map(|e| (e.keycode, e.shift)).collect();
        let swapped_text =
            decode_through_layout(&self.layouts_csv, &self.variants_csv, other_idx, &positional)?;

        // Owned strings sidestep simultaneous-borrow issues with self.dicts.
        let cur_lang = self
            .xkb
            .layouts()
            .get(cur_idx as usize)
            .cloned()
            .unwrap_or_default();
        let other_lang = self
            .xkb
            .layouts()
            .get(other_idx as usize)
            .cloned()
            .unwrap_or_default();
        let cur_dict = self.dict_for_layout(cur_idx);
        let other_dict = self.dict_for_layout(other_idx);
        let verdict = classify(
            &current_text,
            &swapped_text,
            cur_dict,
            other_dict,
            &cur_lang,
            &other_lang,
            self.cfg.detector.min_word_len,
            self.cfg.detector.min_score_delta,
        );

        debug!(
            word = %current_text,
            swap = %swapped_text,
            cur = %cur_lang,
            other = %other_lang,
            ?verdict,
            "detector"
        );

        let was_short = current_text.chars().count() < self.cfg.detector.min_word_len;
        match verdict {
            Verdict::MisLayout => {
                self.auto_replay(entries, boundary_kc).await?;
            }
            Verdict::Unknown if was_short => {
                // Stash for a possible retro-fix when the next word fires
                // MisLayout. We only stash short Unknowns because they're the
                // ones the dict path deliberately abstained on; longer
                // Unknowns are likely intentional gibberish.
                self.state.prev_token = Some(PrevToken {
                    entries,
                    boundary_kc,
                    finished_at: Instant::now(),
                    layout_idx_when_typed: cur_idx,
                });
                self.state.buffer.clear();
            }
            _ => {
                // Ok, or Unknown on a non-short word — neither qualifies as
                // a retro-fix candidate.
                self.state.prev_token = None;
                self.state.buffer.clear();
            }
        }
        Ok(())
    }

    /// Convert a word to the other layout: BS through the typed text + its
    /// boundary, switch layout, replay through the new layout, re-emit the
    /// boundary key. If a recent short token sat in the same wrong layout
    /// (`prev_token`), include it in the same backspace sweep so it gets
    /// fixed too — addresses the "preposition stays in the wrong layout"
    /// case where users start a phrase with a 1–2 char Russian preposition.
    async fn auto_replay(&mut self, entries: Vec<WordEntry>, boundary_kc: u32) -> Result<()> {
        let cur_idx = self.xkb.active_index();
        let n_layouts = self.xkb.layouts().len() as u32;
        let target_idx = if n_layouts > 0 {
            (cur_idx + 1) % n_layouts
        } else {
            0
        };

        let prev = self.state.prev_token.take().filter(|p| {
            let age = Instant::now().saturating_duration_since(p.finished_at);
            age < Duration::from_millis(self.cfg.detector.retro_window_ms)
                && p.layout_idx_when_typed == cur_idx
        });

        let n = entries.len();
        let prev_count = prev.as_ref().map(|p| p.entries.len() + 1).unwrap_or(0);
        let total_bs = prev_count + n + 1;

        self.state.replaying = true;

        let res: Result<()> = async {
            self.injector.backspaces(total_bs).await?;
            HyprIpc::switch_layout_to(target_idx).await?;
            sleep(Duration::from_millis(40)).await;

            if let Some(p) = &prev {
                self.injector.replay_entries(&p.entries).await?;
                let pb = u16::try_from(p.boundary_kc).map_err(|_| {
                    crate::error::Error::Evdev(format!(
                        "prev boundary kc {} out of range",
                        p.boundary_kc
                    ))
                })?;
                self.injector.press_release(pb).await?;
            }

            self.injector.replay_entries(&entries).await?;
            // Settle before re-emitting the boundary key. The focused
            // client needs time to render the replayed letters; without
            // enough margin the trailing space lands too early and
            // gets dropped (commonly seen with fcitx5 in the loop).
            sleep(Duration::from_millis(50)).await;
            let bcode = u16::try_from(boundary_kc).map_err(|_| {
                crate::error::Error::Evdev(format!("boundary kc {boundary_kc} out of range"))
            })?;
            self.injector.press_release(bcode).await?;
            Ok(())
        }
        .await;

        if let Err(e) = res {
            warn!(error = %e, "auto replay failed mid-flight");
            self.state.replaying = false;
            return Err(e);
        }

        // Pre-sync xkb to the new layout. The activelayout IPC event will
        // confirm this shortly; doing it here avoids decoding subsequent
        // keystrokes against the stale layout.
        if n_layouts > 0 {
            self.xkb.set_active_layout(target_idx);
            self.state.layout_idx = target_idx;
        }

        self.state.replaying = false;
        self.state.buffer.clear();
        let with_prev = prev.is_some();
        info!(n, with_prev, "auto-converted word in switched layout");
        Ok(())
    }

    /// Swap the layout on whatever the user has highlighted right now.
    /// Drops any in-progress word — the selection swap is a deliberate user
    /// action and should override pending auto-detection state.
    pub async fn swap_selection(&mut self) -> Result<()> {
        self.state.buffer.clear();
        self.state.prev_token = None;
        self.state.replaying = true;
        let res = crate::selection::swap_selection(
            &mut self.xkb,
            &mut self.injector,
            &self.layouts_csv,
            &self.variants_csv,
        )
        .await;
        self.state.replaying = false;
        if let Ok(()) = &res {
            self.state.layout_idx = self.xkb.active_index();
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_keycodes() {
        assert!(is_boundary_keycode(57)); // space
        assert!(is_boundary_keycode(28)); // enter
        assert!(is_boundary_keycode(15)); // tab
        assert!(!is_boundary_keycode(30)); // a
    }

    #[test]
    fn punct_strings() {
        assert!(is_punct_char("."));
        assert!(is_punct_char(","));
        assert!(!is_punct_char("a"));
    }

    #[test]
    fn layout_index_matches_us() {
        let xkb = XkbState::new("us,ru", ",").unwrap();
        assert_eq!(layout_index(&xkb, "English (US)"), Some(0));
        assert_eq!(layout_index(&xkb, "Russian"), Some(1));
    }

    #[test]
    fn layout_index_does_not_substring_match() {
        let xkb = XkbState::new("us,ru", ",").unwrap();
        assert_eq!(layout_index(&xkb, "Russian"), Some(1));
    }

    #[test]
    fn layout_index_unknown_returns_none() {
        let xkb = XkbState::new("us,ru", ",").unwrap();
        assert_eq!(layout_index(&xkb, "German"), None);
    }

    #[test]
    fn layout_index_handles_reversed_order() {
        let xkb = XkbState::new("ru,us", ",").unwrap();
        assert_eq!(layout_index(&xkb, "Russian"), Some(0));
        assert_eq!(layout_index(&xkb, "English (US)"), Some(1));
    }
}
