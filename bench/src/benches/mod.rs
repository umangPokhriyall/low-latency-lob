//! Benchmark bodies. Each owns its own monomorphized `match impl { .. }`
//! dispatch so the concrete book type is named at the call site (no `dyn`).

pub mod branch_exp;
pub mod cache_exp;
pub mod e2e;
pub mod flat_memory;
pub mod profile;
pub mod read;
pub mod ring;
pub mod seqlock;
pub mod service;
pub mod sustained;
pub mod throughput;
