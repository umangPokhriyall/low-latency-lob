//! Pinning, warmup, and the canonical impl set. The actual monomorphized
//! dispatch (`match name { "btree" => run::<BTreeBook>(..) }`) lives in each
//! benchmark so the concrete type is named at the call site and `apply` inlines
//! — there is **no `dyn OrderBook`** anywhere in a measured loop (phase1 §2.4).

/// The four impls under test after Phase 5 (`"flat"` is the `FlatBook` addition).
pub const IMPLS: [&str; 4] = ["btree", "sorted", "rev", "flat"];

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

/// Read a pinned-core assignment from an environment variable (the metal-run
/// plumbing: `WRITER_CORE` for the seqlock writer, `PRODUCER_CORE` for the ring
/// producer — see `bench/metal_run.sh` / `docs/specs/bare-metal-rerun-spec.md` §A.8).
/// Returns `None` when unset or unparseable so the caller keeps its own default; an
/// explicit `--core` flag still overrides this (flag beats env).
#[must_use]
pub fn env_core(var: &str) -> Option<usize> {
    std::env::var(var).ok()?.trim().parse().ok()
}

/// Parse a comma/space-separated core list (`"6,12,18"`) into logical-core ids,
/// ignoring blank and non-numeric entries. Used for the explicit reader/consumer
/// core list so the contention benches can spread readers ACROSS CCDs (max
/// cross-CCD coherence traffic for `perf c2c`) rather than sit contiguous to the
/// writer. Empty input yields an empty vec (caller falls back to `core+1+r`).
#[must_use]
pub fn parse_core_list(s: &str) -> Vec<usize> {
    s.split([',', ' '])
        .filter_map(|t| {
            let t = t.trim();
            if t.is_empty() { None } else { t.parse().ok() }
        })
        .collect()
}

/// Resolve the explicit reader/consumer core list for a contention bench, honoring
/// (in precedence order) the `--reader-cores a,b,c` flag, then the `READER_CORES`
/// environment variable. Returns an empty vec when neither is present, in which case
/// the bench uses its contiguous `core+1+r` default (unchanged laptop behavior).
#[must_use]
pub fn reader_cores(args: &[String]) -> Vec<usize> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--reader-cores" {
            if let Some(v) = it.next() {
                return parse_core_list(v);
            }
        }
    }
    std::env::var("READER_CORES").ok().map(|s| parse_core_list(&s)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impl_registry_is_the_four_books() {
        assert_eq!(IMPLS, ["btree", "sorted", "rev", "flat"]);
        assert_eq!(for_impl("btree"), Some("btree"));
        assert_eq!(for_impl("rev"), Some("rev"));
        assert_eq!(for_impl("flat"), Some("flat")); // Phase 5 addition
        assert_eq!(for_impl("nope"), None);
    }

    #[test]
    fn warmup_runs_exactly_n_times() {
        let mut n = 0u64;
        warmup(1000, || n += 1);
        assert_eq!(n, 1000);
    }

    #[test]
    fn parse_core_list_handles_commas_spaces_and_junk() {
        assert_eq!(parse_core_list("6,12,18"), vec![6, 12, 18]);
        assert_eq!(parse_core_list(" 6 , 12 ,18 "), vec![6, 12, 18]);
        assert_eq!(parse_core_list("6 12 18"), vec![6, 12, 18]);
        assert_eq!(parse_core_list(""), Vec::<usize>::new());
        assert_eq!(parse_core_list("6,,x,18"), vec![6, 18]); // blanks + non-numeric skipped
    }

    #[test]
    fn reader_cores_flag_beats_env_and_defaults_empty() {
        let args = ["--reader-cores".to_string(), "1,2,3".to_string()];
        assert_eq!(reader_cores(&args), vec![1, 2, 3]);
        // No flag and (in the test process) no READER_CORES set -> empty.
        assert!(reader_cores(&[]).is_empty() || std::env::var("READER_CORES").is_ok());
    }
}
