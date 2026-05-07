use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),

    #[error("config file not found: {0}")]
    ConfigMissing(PathBuf),

    #[error("config parse: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("evdev: {0}")]
    Evdev(String),

    #[error("xkb: failed to compile keymap (layout={layout:?}, variant={variant:?})")]
    XkbCompile { layout: String, variant: String },

    #[error("no usable keyboard devices found in /dev/input")]
    NoKeyboards,
}
