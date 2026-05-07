use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Config {
    pub general: General,
    pub hotkeys: Hotkeys,
    pub languages: Languages,
    pub dictionaries: Dictionaries,
    pub detector: Detector,
    pub devices: Devices,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct General {
    pub quiet_ms: u64,
    pub idle_reset_ms: u64,
    pub default_layout: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Hotkeys {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Languages {
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Dictionaries {
    pub en: PathBuf,
    pub ru: PathBuf,
    pub extra_words: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Detector {
    pub min_word_len: usize,
    pub min_score_delta: f32,
    pub retro_window_ms: u64,
    pub blacklist: Vec<String>,
    pub fallback_unicode_input: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Devices {
    pub ignore: Vec<String>,
    pub also_watch_mice: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: General::default(),
            hotkeys: Hotkeys::default(),
            languages: Languages::default(),
            dictionaries: Dictionaries::default(),
            detector: Detector::default(),
            devices: Devices::default(),
        }
    }
}

impl Default for General {
    fn default() -> Self {
        Self {
            quiet_ms: 25,
            idle_reset_ms: 800,
            default_layout: "us,ru".to_string(),
        }
    }
}

impl Default for Languages {
    fn default() -> Self {
        Self {
            enabled: vec!["en".to_string(), "ru".to_string()],
        }
    }
}

impl Default for Dictionaries {
    fn default() -> Self {
        Self {
            en: PathBuf::from("/usr/share/hunspell/en_US-large.dic"),
            ru: default_data_dir().join("dicts").join("ru_top200k.txt"),
            extra_words: Vec::new(),
        }
    }
}

fn default_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("swtchr")
}

impl Default for Detector {
    fn default() -> Self {
        Self {
            min_word_len: 3,
            // Tuned for the unigram log-frequency fallback: ~1.0 catches
            // anglicism/slang cases (deploy/каво/че) without firing on real
            // 3–4 letter English words. Bigram fallback would let this drop.
            min_score_delta: 1.0,
            retro_window_ms: 1500,
            blacklist: Vec::new(),
            fallback_unicode_input: false,
        }
    }
}

impl Default for Devices {
    fn default() -> Self {
        Self {
            ignore: Vec::new(),
            also_watch_mice: true,
        }
    }
}

impl Config {
    pub fn default_path() -> PathBuf {
        let home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        home.join("swtchr").join("config.toml")
    }

    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(Error::ConfigMissing(path.to_path_buf()));
        }
        let text = std::fs::read_to_string(path)?;
        let cfg: Self = toml::from_str(&text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load_or_default(path: &Path) -> Result<Self> {
        match Self::load(path) {
            Ok(c) => Ok(c),
            Err(Error::ConfigMissing(_)) => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.languages.enabled.is_empty() {
            return Err(Error::Config("languages.enabled is empty".into()));
        }
        if self.detector.min_word_len == 0 {
            return Err(Error::Config("detector.min_word_len must be > 0".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn parse_minimal() {
        let s = r#"
            [general]
            idle_reset_ms = 1500
            [languages]
            enabled = ["en", "ru"]
        "#;
        let c: Config = toml::from_str(s).unwrap();
        assert_eq!(c.general.idle_reset_ms, 1500);
        assert_eq!(c.languages.enabled, vec!["en", "ru"]);
    }

    #[test]
    fn missing_file_returns_default_via_load_or_default() {
        let c = Config::load_or_default(Path::new("/nonexistent/swtchr.toml")).unwrap();
        assert_eq!(c.general.idle_reset_ms, 800);
    }
}
