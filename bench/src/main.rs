//! `bench` — the measurement harness: a low-overhead TSC clock, an `HdrHistogram`
//! recorder, pinning/warmup discipline, and the depth-sweep / CO-correct studies
//! that produce the committed numbers under `bench/results/`. (Phase 4.)
//!
//! Subcommands: `service | read | sustained | throughput | plot | all`. Phase 4
//! is built one session per benchmark; only the implemented ones run today.
#![forbid(unsafe_code)]

mod benches;
mod clock;
mod harness;
mod plot;
mod recorder;
mod workload;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map_or("help", String::as_str);
    let rest: &[String] = if args.len() > 2 { &args[2..] } else { &[] };

    match cmd {
        // Benchmark 1 — service-time depth sweep (the crossover). Session 1.
        "service" => benches::service::run(rest),
        // Benchmark 2 — read-path cost vs depth. Session 2.
        "read" => benches::read::run(rest),
        // Benchmark 4 — end-to-end replay throughput. Session 2.
        "throughput" => benches::throughput::run(rest),
        // Benchmark 3 — CO-correct sustained-feed response time. Session 3.
        "sustained" => benches::sustained::run(rest),
        // FlatBook allocated span (ticks + bytes) per config — the memory tradeoff.
        "flatmem" => benches::flat_memory::run(rest),
        // Benchmark 5 — seqlock read latency under write contention (Phase 6).
        "seqlock" => benches::seqlock::run(rest),
        // Benchmark 6 — SPMC ring throughput + false-sharing evidence (Phase 7).
        "ring" => benches::ring::run(rest),
        // Benchmark 7 — end-to-end production-to-consumption latency (Phase 8).
        "e2e" => benches::e2e::run(rest),
        // Phase 9 — isolated untimed apply/search hot loop (external perf target).
        "profile" => benches::profile::run(rest),
        // Phase 9 — branch-misprediction 2×2 signature (branchy vs branchless).
        "branch-exp" => benches::branch_exp::run(rest),
        // Phase 9 — cache-hierarchy footprint signature (L1/L2/LLC crossings).
        "cache-exp" => benches::cache_exp::run(rest),
        // Render every §9 figure + env.json strictly from the committed CSVs.
        "plot" => plot::run(rest),
        // `all` chains every benchmark then renders the plots + env.json last.
        "all" => {
            benches::service::run(rest);
            benches::read::run(rest);
            benches::throughput::run(rest);
            benches::sustained::run(rest);
            benches::flat_memory::run(rest);
            benches::seqlock::run(rest);
            benches::ring::run(rest);
            benches::e2e::run(rest);
            plot::run(rest);
        }
        _ => {
            eprintln!(
                "usage: bench <service|read|sustained|throughput|flatmem|seqlock|ring|e2e|\
                 profile|branch-exp|cache-exp|plot|all> \
                 [--samples N] [--warmup N] [--core N] [--speed N] [--no-real] [--out DIR]\n\
                 profile: --impl btree|sorted|rev|flat --op apply|search --depth D \
                 --locality concentrated|uniform --iters N [--core C]"
            );
            std::process::exit(2);
        }
    }
}
