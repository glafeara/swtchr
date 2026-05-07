//! Layout-mismatch detector. Given a finished word in two forms — what the
//! user actually typed in the *current* layout, and what those same physical
//! keys would have produced in the *other* layout — it returns a verdict on
//! whether to leave the word alone or trigger a layout switch + replay.
//!
//! Two paths share a 2-char hard floor:
//!   1. Dictionary path. If `current_text` is in `cur_dict` we trust the
//!      current layout; if `swapped_text` is in `other_dict` (and `current`
//!      is not in `cur_dict`) we declare `MisLayout`. Fires from 2 chars —
//!      a hit in the *other* dict is strong enough evidence to act on short
//!      common function words ("ты", "на", "то") regardless of the user's
//!      `min_word_len` setting.
//!      Special case for short tokens: a current-side match that exists only
//!      as an all-uppercase ASCII acronym ("NS", "VS") is treated as a weak
//!      hit and yields to a natural-word match on the swap side. Without
//!      this, lowercase Russian function words typed in EN never swap
//!      because the EN dict's abbreviations shadow them.
//!   2. Statistical fallback. When neither dictionary recognises the word —
//!      common for anglicisms ("деплой"), slang ("каво", "че"), proper nouns
//!      and domain jargon — we compare per-letter log-frequency of the two
//!      forms in their respective languages. A delta exceeding
//!      `min_score_delta` flips the verdict to `MisLayout`. The threshold
//!      itself prevents one-char jitter so we use the same 2-char floor.

use crate::core::dict::Dict;
use crate::core::lang_score;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Word looks correct (or we're not confident enough to act).
    Ok,
    /// Word should be retyped in the *other* layout.
    MisLayout,
    /// Word too short, contains non-alpha, or detector is otherwise abstaining.
    Unknown,
}

/// Absolute minimum length for any swap action. Below this, the detector
/// abstains regardless of evidence quality — single-char tokens are too
/// ambiguous (every Cyrillic letter has a Latin counterpart that could be a
/// legitimate solo character somewhere).
const MIN_ACTION_LEN: usize = 2;

/// Token length at or below which the acronym-shadowing tie-break applies.
/// Hunspell's `en_US-large` stores hundreds of short ASCII acronyms ("NS",
/// "VS", "DS") that, after case-insensitive lowercasing, match common
/// lowercase Russian function words swap-decoded to Latin ("ты", "мы", "вы").
/// For tokens this short, we treat an acronym-only current-side match as
/// "weak" and let a natural-word match on the swap side win.
const SHORT_TIEBREAK_LEN: usize = 3;

pub fn classify(
    current_text: &str,
    swapped_text: &str,
    cur_dict: &dyn Dict,
    other_dict: &dyn Dict,
    cur_lang: &str,
    other_lang: &str,
    min_score_delta: f32,
) -> Verdict {
    let chars: Vec<char> = current_text.chars().collect();
    if chars.iter().any(|c| c.is_ascii_digit() || c.is_whitespace()) {
        return Verdict::Unknown;
    }

    let cur_known = cur_dict.lookup(current_text);

    // Short-token tiebreak: if the current-side match exists only as an
    // all-uppercase acronym (cur_natural=false) and the swap form is a real
    // natural word in the other dict, the user almost certainly typed in the
    // wrong layout. Without this, "ns" matches the EN abbreviation "NS" and
    // hides "ты" forever.
    if cur_known
        && chars.len() >= MIN_ACTION_LEN
        && chars.len() <= SHORT_TIEBREAK_LEN
        && !cur_dict.lookup_natural(current_text)
        && other_dict.lookup_natural(swapped_text)
    {
        return Verdict::MisLayout;
    }

    if cur_known {
        return Verdict::Ok;
    }

    if chars.len() < MIN_ACTION_LEN {
        return Verdict::Unknown;
    }

    if other_dict.lookup(swapped_text) {
        return Verdict::MisLayout;
    }

    // Statistical fallback: only when the *other* dict was no help either.
    // We never override a dictionary "Ok" — that's why we returned early above.
    if let (Some(cur_score), Some(other_score)) = (
        lang_score::score(current_text, cur_lang),
        lang_score::score(swapped_text, other_lang),
    ) && other_score - cur_score >= min_score_delta
    {
        return Verdict::MisLayout;
    }

    Verdict::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::dict::HunspellDict;

    fn dict(words: &[&str]) -> HunspellDict {
        let mut d = HunspellDict::empty();
        d.add_words(words.iter().copied());
        d
    }

    fn classify_default(
        current: &str,
        swapped: &str,
        cur: &dyn Dict,
        other: &dyn Dict,
        cur_lang: &str,
        other_lang: &str,
    ) -> Verdict {
        classify(current, swapped, cur, other, cur_lang, other_lang, 1.0)
    }

    #[test]
    fn ok_when_word_in_current_dict() {
        let en = dict(&["hello", "world"]);
        let ru = dict(&["привет", "мир"]);
        let v = classify_default("hello", "руддщ", &en, &ru, "us", "ru");
        assert_eq!(v, Verdict::Ok);
    }

    #[test]
    fn mislayout_when_swap_in_other_dict() {
        let en = dict(&["hello"]);
        let ru = dict(&["привет"]);
        let v = classify_default("ghbdtn", "привет", &en, &ru, "us", "ru");
        assert_eq!(v, Verdict::MisLayout);
    }

    #[test]
    fn unknown_when_too_short_and_neither_lang_dominates() {
        let en = dict(&["a"]);
        let ru = dict(&["я"]);
        // "a" is in en dict but `cur_known` short-circuits to Ok — so this is
        // really testing that classify still says Ok for in-dict shortwords.
        let v = classify_default("a", "ф", &en, &ru, "us", "ru");
        assert_eq!(v, Verdict::Ok);
    }

    #[test]
    fn unknown_when_neither_dict_knows_no_score_signal() {
        // Two equally implausible strings — fallback should not fire.
        let en = HunspellDict::empty();
        let ru = HunspellDict::empty();
        // "abc" / "фис" — both have OK letter freqs in their langs; delta small.
        let v = classify_default("abc", "фис", &en, &ru, "us", "ru");
        assert_eq!(v, Verdict::Unknown);
    }

    #[test]
    fn unknown_when_contains_digits() {
        let en = dict(&["hello"]);
        let ru = dict(&["привет"]);
        let v = classify_default("ab12cd", "фи12ыа", &en, &ru, "us", "ru");
        assert_eq!(v, Verdict::Unknown);
    }

    #[test]
    fn ambiguous_both_known_returns_ok() {
        let en = dict(&["test"]);
        let ru = dict(&["test", "тест"]);
        let v = classify_default("test", "тест", &en, &ru, "us", "ru");
        assert_eq!(v, Verdict::Ok);
    }

    #[test]
    fn empty_other_dict_does_not_force_mislayout() {
        // "ghbdtn" actually scores fairly high under EN unigram freqs (its
        // letters are individually common), so the stat fallback abstains.
        // The dict path is the right tool for "привет" — and it works since
        // the word is in any real ru dict.
        let en = dict(&["hello"]);
        let ru = HunspellDict::empty();
        let v = classify_default("ghbdtn", "привет", &en, &ru, "us", "ru");
        assert_eq!(v, Verdict::Unknown);
    }

    #[test]
    fn case_insensitive_lookup() {
        let en = dict(&["hello"]);
        let ru = dict(&["привет"]);
        assert_eq!(
            classify_default("Hello", "Руддщ", &en, &ru, "us", "ru"),
            Verdict::Ok
        );
        assert_eq!(
            classify_default("GHBDTN", "ПРИВЕТ", &en, &ru, "us", "ru"),
            Verdict::MisLayout
        );
    }

    #[test]
    fn anglicism_deploy_caught_by_stat_fallback() {
        // Neither dict knows "деплой" nor its Latin swap "ltgkjq".
        let en = dict(&["the", "and", "deploy"]); // "deploy" present in EN dict but typed form is "ltgkjq", not "deploy"
        let ru = dict(&["привет"]);
        let v = classify_default("ltgkjq", "деплой", &en, &ru, "us", "ru");
        assert_eq!(v, Verdict::MisLayout);
    }

    #[test]
    fn slang_kavo_caught_by_stat_fallback() {
        let en = HunspellDict::empty();
        let ru = dict(&["кого"]); // standard form is "кого"; "каво" is colloquial and absent
        let v = classify_default("rfdj", "каво", &en, &ru, "us", "ru");
        assert_eq!(v, Verdict::MisLayout);
    }

    #[test]
    fn slang_che_short_caught_by_stat_fallback() {
        // 2 chars: below dict-path min_len (3) but at the stat-path floor.
        let en = HunspellDict::empty();
        let ru = HunspellDict::empty();
        let v = classify_default("xt", "че", &en, &ru, "us", "ru");
        assert_eq!(v, Verdict::MisLayout);
    }

    #[test]
    fn one_char_never_acted_on() {
        let en = HunspellDict::empty();
        let ru = HunspellDict::empty();
        let v = classify_default("d", "в", &en, &ru, "us", "ru");
        assert_eq!(v, Verdict::Unknown);
    }

    #[test]
    fn high_threshold_blocks_borderline_cases() {
        let en = HunspellDict::empty();
        let ru = HunspellDict::empty();
        // Threshold of 5.0 is unreachable for any real word pair.
        let v = classify("ghbdtn", "привет", &en, &ru, "us", "ru", 5.0);
        assert_eq!(v, Verdict::Unknown);
    }

    #[test]
    fn unsupported_lang_disables_stat_fallback() {
        let en = HunspellDict::empty();
        let ru = HunspellDict::empty();
        let v = classify("ghbdtn", "привет", &en, &ru, "de", "fr", 1.5);
        assert_eq!(v, Verdict::Unknown);
    }

    #[test]
    fn two_char_dict_match_fires_mislayout() {
        // "ns" → "ты" — the original short-preposition case from the user
        // report. Used to require min_len=3 → never fired. Now: dict hit on
        // the swap is enough at 2 chars.
        let en = dict(&["the", "and"]);
        let ru = dict(&["ты", "привет"]);
        let v = classify_default("ns", "ты", &en, &ru, "us", "ru");
        assert_eq!(v, Verdict::MisLayout);
    }

    #[test]
    fn two_char_dict_match_other_direction() {
        // "yf" → "на" (Russian "on"). User typed "yf" in EN, intended "на".
        let en = dict(&["the", "and"]);
        let ru = dict(&["на", "привет"]);
        let v = classify_default("yf", "на", &en, &ru, "us", "ru");
        assert_eq!(v, Verdict::MisLayout);
    }

    /// Build a HunspellDict via a temp .dic file so the loader's
    /// natural/acronym classification runs on the input.
    fn dict_raw(words: &[&str]) -> HunspellDict {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        for w in words {
            writeln!(tmp, "{w}").unwrap();
        }
        tmp.flush().unwrap();
        HunspellDict::load(tmp.path()).unwrap()
    }

    #[test]
    fn acronym_only_match_yields_to_natural_swap() {
        // EN dict has "NS" only as an uppercase abbreviation; RU dict has
        // "ты" as a normal word. Lowercase user input "ns" matches "NS"
        // case-insensitively but isn't a natural English word — the swap
        // should win.
        let en = dict_raw(&["the", "NS", "VS"]);
        let ru = dict_raw(&["ты", "мы"]);
        assert_eq!(
            classify_default("ns", "ты", &en, &ru, "us", "ru"),
            Verdict::MisLayout
        );
        assert_eq!(
            classify_default("vs", "мы", &en, &ru, "us", "ru"),
            Verdict::MisLayout
        );
    }

    #[test]
    fn natural_short_word_keeps_ok_even_with_acronym_swap_hit() {
        // "is" is a natural EN word (lowercase entry). Even if the swap "шы"
        // happened to match a Russian acronym (it doesn't, but to test the
        // tiebreak doesn't over-fire), Ok must win.
        let en = dict_raw(&["the", "is"]);
        let ru = dict_raw(&["шы"]); // contrived; "шы" loaded as natural here
        assert_eq!(
            classify_default("is", "шы", &en, &ru, "us", "ru"),
            Verdict::Ok
        );
    }

    #[test]
    fn acronym_only_match_without_natural_swap_stays_ok() {
        // "as" matches EN acronym "AS" only, swap "фы" isn't a Russian word.
        // No tiebreak signal → fall back to Ok (don't manufacture a swap).
        let en = dict_raw(&["the", "AS"]);
        let ru = dict_raw(&["привет"]);
        assert_eq!(
            classify_default("as", "фы", &en, &ru, "us", "ru"),
            Verdict::Ok
        );
    }
}
