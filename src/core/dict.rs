//! Hunspell `.dic` loader.
//!
//! The format is well documented but trivial in the parts we care about:
//! - first line is a count (we skip if it parses as integer);
//! - each subsequent line is `word/FLAGS` or just `word`;
//! - blank lines and BOM-prefixed first line are tolerated;
//! - we strip the morphological flags after the slash and lowercase the
//!   result. We do not expand affixes — for layout detection, base forms +
//!   common forms are good enough.
//!
//! Two lookup modes are exposed:
//! - `lookup`: case-insensitive, hits anything in the file.
//! - `lookup_natural`: hits only entries that had at least one non-uppercase
//!   character in the source (or are non-ASCII). This excludes pure ASCII
//!   acronyms ("NS", "VS", "OK") while keeping mixed/lowercase entries
//!   ("hello", "Hello", "iPhone") and all Cyrillic words. The detector uses
//!   it to break ties on short tokens — without it, common Russian function
//!   words like "ты" or "вы" (typed as Latin "ns"/"ds" by mistake) get
//!   shadowed by EN acronyms and never trigger swap.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use tracing::{info, warn};

use crate::error::Result;

pub trait Dict: Send + Sync {
    fn lookup(&self, word: &str) -> bool;
    /// Like `lookup`, but excludes matches that exist only as all-uppercase
    /// ASCII entries. Default impl mirrors `lookup` for non-Hunspell dicts
    /// (test mocks, empty dict) since they don't track case provenance.
    fn lookup_natural(&self, word: &str) -> bool {
        self.lookup(word)
    }
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Default)]
pub struct HunspellDict {
    /// All lowercased forms (case-insensitive lookup).
    words: HashSet<String>,
    /// Subset of `words` whose original form contained at least one
    /// non-uppercase character (i.e., is not a pure ASCII acronym). A short
    /// lowercase user input that matches only here is more likely a real
    /// natural-language word than an abbreviation.
    natural: HashSet<String>,
}

impl HunspellDict {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut words = HashSet::new();
        let mut natural = HashSet::new();
        for (i, line) in reader.lines().enumerate() {
            let line = line?;
            let line = strip_bom(&line);
            // First line: a count? Skip if so.
            if i == 0 && line.trim().parse::<usize>().is_ok() {
                continue;
            }
            if let Some(word) = parse_dic_line(line) {
                let lc = word.to_lowercase();
                words.insert(lc.clone());
                if !is_ascii_acronym(word) {
                    natural.insert(lc);
                }
            }
        }
        info!(
            count = words.len(),
            natural = natural.len(),
            ?path,
            "loaded hunspell dict"
        );
        Ok(Self { words, natural })
    }

    /// Load if path exists; otherwise return empty and log a warning.
    pub fn load_or_warn(path: &Path) -> Self {
        match Self::load(path) {
            Ok(d) => d,
            Err(e) => {
                warn!(?path, error = %e, "could not load dict; auto-detection will be weak");
                Self::empty()
            }
        }
    }

    /// User-supplied additions are always treated as natural words — they're
    /// explicit allowlist entries, not scraped from a dict file.
    pub fn add_words<'a, I: IntoIterator<Item = &'a str>>(&mut self, ws: I) {
        for w in ws {
            let lc = w.to_lowercase();
            self.words.insert(lc.clone());
            self.natural.insert(lc);
        }
    }
}

impl Dict for HunspellDict {
    fn lookup(&self, word: &str) -> bool {
        self.words.contains(&word.to_lowercase())
    }
    fn lookup_natural(&self, word: &str) -> bool {
        self.natural.contains(&word.to_lowercase())
    }
    fn len(&self) -> usize {
        self.words.len()
    }
}

/// True for non-empty strings whose every char is ASCII uppercase. Cyrillic
/// uppercase ("ПРИВЕТ") and mixed/lowercase strings are NOT acronyms here.
fn is_ascii_acronym(w: &str) -> bool {
    !w.is_empty() && w.chars().all(|c| c.is_ascii_uppercase())
}

fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{FEFF}').unwrap_or(s)
}

fn parse_dic_line(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // word may be followed by '/' + flags, or whitespace + morph data
    let upto_slash = line.split('/').next().unwrap_or("");
    let word = upto_slash.split_whitespace().next().unwrap_or("");
    if word.is_empty() {
        return None;
    }
    Some(word)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_dic(s: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(s.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn parses_minimal_dic() {
        let f = write_dic("3\nhello\nworld/MS\nrust\n");
        let d = HunspellDict::load(f.path()).unwrap();
        assert!(d.lookup("hello"));
        assert!(d.lookup("Hello"));
        assert!(d.lookup("world"));
        assert!(d.lookup("rust"));
        assert!(!d.lookup("nope"));
        assert_eq!(d.len(), 3);
    }

    #[test]
    fn handles_bom_and_blank_lines() {
        let f = write_dic("\u{FEFF}2\n\nфыва\nпривет/A\n");
        let d = HunspellDict::load(f.path()).unwrap();
        assert!(d.lookup("привет"));
        assert!(d.lookup("Привет"));
        assert!(d.lookup("фыва"));
    }

    #[test]
    fn ignores_comments_and_count_only_first_line() {
        let f = write_dic("100\n# a comment\nfoo\n42\nbar\n");
        let d = HunspellDict::load(f.path()).unwrap();
        assert!(d.lookup("foo"));
        // Plain numbers further in are also "words" per Hunspell, but we treat
        // the very first numeric line as a count. Subsequent numerics are
        // accepted as-is — that's faithful to the format.
        assert!(d.lookup("42"));
        assert!(d.lookup("bar"));
    }

    #[test]
    fn empty_dict_lookups_false() {
        let d = HunspellDict::empty();
        assert!(!d.lookup("anything"));
        assert!(d.is_empty());
    }

    #[test]
    fn add_words_works() {
        let mut d = HunspellDict::empty();
        d.add_words(["systemd", "Tmux"]);
        assert!(d.lookup("systemd"));
        assert!(d.lookup("tmux"));
    }

    #[test]
    fn missing_file_returns_empty_via_or_warn() {
        let d = HunspellDict::load_or_warn(Path::new("/nonexistent/nope.dic"));
        assert!(d.is_empty());
    }
}
