//! Benchmark bodies. Each owns its own monomorphized `match impl { .. }`
//! dispatch so the concrete book type is named at the call site (no `dyn`).

pub mod read;
pub mod service;
pub mod sustained;
pub mod throughput;
