//! `sync` — lock-free concurrency primitives (seqlock snapshot cell in Phase 6,
//! SPMC cache-line-aligned broadcast ring in Phase 7).
//!
//! Both primitives turned out **sound with no `unsafe` at all**: payloads are
//! atomic words accessed `Relaxed`, and ordering is carried by a version/stamp
//! counter (`Acquire`/`Release` + fences), so torn reads are detected and discarded
//! rather than being undefined behaviour. The `UnsafeCell` + raw-copy shortcut that a
//! generic seqlock or a Vyukov broadcast slot would use is a data race under Rust's
//! memory model — rejected in both phases. With the unsafe budget unspent, the crate
//! is tightened to `#![forbid(unsafe_code)]`: the whole workspace is now
//! compiler-enforced unsafe-free (the Phase 7 zero-`unsafe` capstone).
#![forbid(unsafe_code)]

pub mod ring;
pub mod seqlock;
pub use ring::{Consumer, Producer, Recv, RingHandle, SpmcRing};
pub use seqlock::{SeqLock, TopOfBook};
