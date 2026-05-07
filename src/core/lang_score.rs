//! Per-letter log-frequency scorer used as a fallback when neither dictionary
//! recognises a word. The detector compares the score a word would receive in
//! its currently-typed language against the score the swap-form receives in
//! the other language; a large positive delta means the swap is much more
//! plausible as native text than what the user actually typed.
//!
//! We use unigram (single-letter) log-frequencies rather than bigrams. They
//! are smaller, deterministic, and — because EN and RU use disjoint alphabets
//! in our two-layout setup — already provide a strong signal: a word in the
//! "wrong" alphabet decays each summand toward the smoothing floor, while a
//! plausible word in the right alphabet sums high-frequency letters.
//!
//! Frequencies are taken from standard published letter-frequency tables and
//! treated as percentages of the corpus. The score is the *mean* `ln(freq)`
//! per alphabetic char, so words of different lengths are directly comparable.
//! Non-alphabetic characters are skipped entirely. Letters not in the table
//! contribute the smoothing constant `UNSEEN_LP`.

const UNSEEN_LP: f32 = -4.6; // ln(0.01) — strong but not catastrophic penalty.

/// English letter frequencies (percent of corpus). Source: standard
/// published tables; exact decimals don't matter, only relative ordering.
const EN_FREQ: &[(char, f32)] = &[
    ('e', 12.70),
    ('t', 9.05),
    ('a', 8.17),
    ('o', 7.51),
    ('i', 6.97),
    ('n', 6.75),
    ('s', 6.33),
    ('h', 6.09),
    ('r', 5.99),
    ('d', 4.25),
    ('l', 4.03),
    ('c', 2.78),
    ('u', 2.76),
    ('m', 2.41),
    ('w', 2.36),
    ('f', 2.23),
    ('g', 2.02),
    ('y', 1.97),
    ('p', 1.93),
    ('b', 1.49),
    ('v', 0.98),
    ('k', 0.77),
    ('j', 0.15),
    ('x', 0.15),
    ('q', 0.10),
    ('z', 0.07),
];

/// Russian letter frequencies (percent of corpus). 'ё' is normalised to 'е'
/// at lookup time so its low standalone frequency doesn't penalise a word
/// that legitimately uses it.
const RU_FREQ: &[(char, f32)] = &[
    ('о', 10.97),
    ('е', 8.45),
    ('а', 8.01),
    ('и', 7.35),
    ('н', 6.70),
    ('т', 6.26),
    ('с', 5.47),
    ('р', 4.73),
    ('в', 4.54),
    ('л', 4.40),
    ('к', 3.49),
    ('м', 3.21),
    ('д', 2.98),
    ('п', 2.81),
    ('у', 2.62),
    ('я', 2.01),
    ('ы', 1.90),
    ('ь', 1.74),
    ('г', 1.70),
    ('з', 1.65),
    ('б', 1.59),
    ('ч', 1.44),
    ('й', 1.21),
    ('х', 0.97),
    ('ж', 0.94),
    ('ш', 0.73),
    ('ю', 0.64),
    ('ц', 0.48),
    ('щ', 0.36),
    ('э', 0.32),
    ('ф', 0.26),
    ('ъ', 0.04),
];

fn lookup(table: &[(char, f32)], c: char) -> Option<f32> {
    table.iter().find(|(ch, _)| *ch == c).map(|(_, f)| f.ln())
}

fn mean_log_freq(word: &str, table: &[(char, f32)], normalize_yo: bool) -> Option<f32> {
    let mut sum = 0.0_f32;
    let mut n = 0_usize;
    for c in word.chars().flat_map(char::to_lowercase) {
        if !c.is_alphabetic() {
            continue;
        }
        let c = if normalize_yo && c == 'ё' { 'е' } else { c };
        sum += lookup(table, c).unwrap_or(UNSEEN_LP);
        n += 1;
    }
    if n == 0 { None } else { Some(sum / n as f32) }
}

pub fn score_en(word: &str) -> Option<f32> {
    mean_log_freq(word, EN_FREQ, false)
}

pub fn score_ru(word: &str) -> Option<f32> {
    mean_log_freq(word, RU_FREQ, true)
}

/// Score a word against an xkb layout code. Returns `None` for unsupported
/// layouts (caller should treat that as "no statistical signal available").
pub fn score(word: &str, xkb_layout: &str) -> Option<f32> {
    match xkb_layout {
        "us" | "gb" | "en" => score_en(word),
        "ru" => score_ru(word),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn english_word_scores_higher_than_swap_in_russian() {
        // "hello" typed correctly in EN; "руддщ" is its key-positional swap.
        let en = score_en("hello").unwrap();
        let ru = score_ru("руддщ").unwrap();
        assert!(en > ru, "en={en} ru={ru}");
    }

    #[test]
    fn russian_word_scores_higher_than_latin_swap() {
        // user typed "ckjdj" intending "слово". Picked because it has rare
        // Latin letters (k, j) so unigram delta is unambiguous; "ghbdtn"-style
        // strings happen to score well in EN even when they're nonsense.
        let typed_en = score_en("ckjdj").unwrap();
        let intended_ru = score_ru("слово").unwrap();
        assert!(intended_ru - typed_en > 1.5, "ru={intended_ru} en={typed_en}");
    }

    #[test]
    fn anglicism_deploy_triggers_stat_signal() {
        // "деплой" not in standard ru_RU.dic, but reads as Russian.
        let typed_en = score_en("ltgkjq").unwrap();
        let intended_ru = score_ru("деплой").unwrap();
        assert!(intended_ru - typed_en > 1.0);
    }

    #[test]
    fn slang_kavo_triggers_stat_signal() {
        let typed_en = score_en("rfdj").unwrap();
        let intended_ru = score_ru("каво").unwrap();
        assert!(intended_ru - typed_en > 1.0);
    }

    #[test]
    fn short_preposition_v_has_signal() {
        // 1-char "в" vs Latin "d" — the detector caller will gate on length,
        // but the score itself should still favour Russian.
        let v_en = score_en("d").unwrap();
        let v_ru = score_ru("в").unwrap();
        assert!(v_ru > v_en);
    }

    #[test]
    fn returns_none_for_unsupported_layout() {
        assert!(score("hello", "de").is_none());
    }

    #[test]
    fn returns_none_for_no_alpha_chars() {
        assert!(score_en("123").is_none());
        assert!(score_ru("...").is_none());
    }

    #[test]
    fn mean_is_length_invariant() {
        // Repeating a high-freq letter doesn't inflate the per-char mean.
        let one = score_en("e").unwrap();
        let many = score_en("eeeee").unwrap();
        assert!(approx(one, many, 1e-4));
    }

    #[test]
    fn yo_normalised_to_e() {
        // 'ё' on its own is rare; normalised it should match 'е'.
        let yo = score_ru("ё").unwrap();
        let ye = score_ru("е").unwrap();
        assert!(approx(yo, ye, 1e-4));
    }
}
