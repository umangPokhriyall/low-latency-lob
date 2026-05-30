//! `engine` — the assembled, pinned end-to-end hot path: replay -> apply to frozen
//! book -> publish seqlock snapshot -> push derived event to the SPMC ring. Filled in Phase 8.
#![forbid(unsafe_code)]

fn main() {}
