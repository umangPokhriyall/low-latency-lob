//! `feed` — the deterministic event source: replay corpus iterator + synthetic
//! load-profile generator (steady / burst / flash-crash). Filled in Phase 3.
#![forbid(unsafe_code)]

pub mod corpus;
pub mod rng;
pub mod synthetic;

pub use corpus::{Corpus, CorpusError, HEADER_SIZE, MAGIC, RECORD_SIZE, VERSION};
pub use rng::SplitMix64;
pub use synthetic::{generate, GENERATOR_VERSION, GenConfig, Profile};
