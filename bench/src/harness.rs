//! Pinning, warmup, and the canonical impl set. The actual monomorphized
//! dispatch (`match name { "btree" => run::<BTreeBook>(..) }`) lives in each
//! benchmark so the concrete type is named at the call site and `apply` inlines
//! — there is **no `dyn OrderBook`** anywhere in a measured loop (phase1 §2.4).

/// The three impls under test in Phase 4. Phase 5 appends `"flat"`.
pub const IMPLS: [&str; 3] = ["btree", "sorted", "rev"];

/// Pin the current thread to logical core `core` so sample-to-sample migration
/// does not pollute ns measurements (§3.4). Returns whether pinning succeeded;
/// the caller records the outcome and the core id rather than assuming it.
#[must_use]
pub fn pin_to_core(core: usize) -> bool {
    let Some(ids) = core_affinity::get_core_ids() else {
        return false;
    };
    let Some(target) = ids.into_iter().find(|c| c.id == core) else {
        return false;
    };
    core_affinity::set_for_current(target)
}

/// Run an untimed warmup pass: invoke `op` `iters` times to warm I-cache,
/// D-cache, and the branch predictor and let the core reach steady frequency
/// before any sample is recorded (§3.4). Warmup output is discarded.
#[inline]
pub fn warmup(iters: u64, mut op: impl FnMut()) {
    for _ in 0..iters {
        op();
    }
}

/// Validate an impl name against [`IMPLS`], returning the canonical `&'static str`.
/// Benchmarks then `match` on it to pick the concrete type to monomorphize over.
#[must_use]
pub fn for_impl(name: &str) -> Option<&'static str> {
    IMPLS.iter().copied().find(|&n| n == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impl_registry_is_the_three_phase4_books() {
        assert_eq!(IMPLS, ["btree", "sorted", "rev"]);
        assert_eq!(for_impl("btree"), Some("btree"));
        assert_eq!(for_impl("rev"), Some("rev"));
        assert_eq!(for_impl("flat"), None); // Phase 5
        assert_eq!(for_impl("nope"), None);
    }

    #[test]
    fn warmup_runs_exactly_n_times() {
        let mut n = 0u64;
        warmup(1000, || n += 1);
        assert_eq!(n, 1000);
    }
}
