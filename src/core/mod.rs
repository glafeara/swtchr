pub mod detector;
pub mod dict;
pub mod lang_score;
pub mod state;

pub use detector::{Verdict, classify};
pub use dict::{Dict, HunspellDict};
pub use state::{CoreState, ModState, PrevToken, WordBuffer, WordEntry};
