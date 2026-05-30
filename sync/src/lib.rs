//! `sync` — lock-free concurrency primitives (seqlock snapshot cell in Phase 6,
//! SPMC cache-line-aligned ring buffer in Phase 7).
//!
//! This is the ONLY crate permitted to use `unsafe`. Every `unsafe` block must
//! carry a `// SAFETY:` justification. `unsafe fn` bodies do not get an implicit
//! unsafe scope — see the `unsafe_op_in_unsafe_fn` lint below.
#![deny(unsafe_op_in_unsafe_fn)]
