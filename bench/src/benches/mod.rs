//! Benchmark bodies. Each owns its own monomorphized `match impl { .. }`
//! dispatch so the concrete book type is named at the call site (no `dyn`).

pub mod flat_memory;
pub mod read;
pub mod seqlock;
pub mod service;
pub mod sustained;
pub mod throughput;
