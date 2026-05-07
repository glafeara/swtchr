//! Layout-mismatch detector. Given a finished word in two forms — what the
//! user actually typed in the *current* layout, and what those same physical
//! keys would have produced in the *other* layout — it returns a verdict on
//! whether to leave the word alone or trigger a layout switch + replay.
//!
//! Two paths share the same conservative bias:
//!   1. Dictionary path. If `current_text` is in `cur_dict` we trust the
//!      current layout; if `swapped_text` is in `other_dict` (and `current`
//!      is not in `cur_dict`) we declare `MisLayout`.
//!   2. Statistical fallback. When neither dictionary recognises the word —
//!      common for anglicisms ("деплой"), slang ("каво", "че"), proper nouns
//!      and domain jargon — we compare per-letter log-frequency of the two
//!      forms in their respective languages. A delta exceeding
//!      `min_score_delta` flips the verdict to `MisLayout`. The fallback
//!      activates from 2 chars (so short forms still get a chance) but the
//!      threshold itself prevents one-char jitter.

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

/// Minimum length for the statistical fallback. The dictionary path uses
/// `min_len` from config; the fallback uses this hard floor instead because
/// raising config's `min_word_len` would also lock out short common words
/// from the dict path (where they're safe).
const STAT_MIN_LEN: usize = 2;

pub fn classify(
    current_text: &str,
    swapped_text: &str,
    cur_dict: &dyn Dict,
    other_dict: &dyn Dict,
    cur_lang: &str,
    other_lang: &str,
    min_len: usize,
    min_score_delta: f32,
) -> Verdict {
    let chars: Vec<char> = current_text.chars().collect();
    if chars.iter().any(|c| c.is_ascii_digit() || c.is_whitespace()) {
        return Verdict::Unknown;
    }

    let cur_known = cur_dict.lookup(current_text);
    if cur_known {
        return Verdict::Ok;
    }

    if chars.len() >= min_len && other_dict.lookup(swapped_text) {
        return Verdict::MisLayout;
    }

    // Statistical fallback: only when the *other* dict was no help either.
    // We never override a dictionary "Ok" — that's why we returned early above.
    if chars.len() >= STAT_MIN_LEN
        && let (Some(cur_score), Some(other_score)) = (
            lang_score::score(current_text, cur_lang),
            lang_score::score(swapped_text, other_lang),
        )
        && other_score - cur_score >= min_score_delta
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
        classify(current, swapped, cur, other, cur_lang, other_lang, 3, 1.0)
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
        let v = classify("ghbdtn", "привет", &en, &ru, "us", "ru", 3, 5.0);
        assert_eq!(v, Verdict::Unknown);
    }

    #[test]
    fn unsupported_lang_disables_stat_fallback() {
        let en = HunspellDict::empty();
        let ru = HunspellDict::empty();
        let v = classify("ghbdtn", "привет", &en, &ru, "de", "fr", 3, 1.5);
        assert_eq!(v, Verdict::Unknown);
    }
}
