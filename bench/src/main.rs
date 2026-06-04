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
        // Render every §9 figure + env.json strictly from the committed CSVs.
        "plot" => plot::run(rest),
        // `all` chains every benchmark then renders the plots + env.json last.
        "all" => {
            benches::service::run(rest);
            benches::read::run(rest);
            benches::throughput::run(rest);
            benches::sustained::run(rest);
            plot::run(rest);
        }
        _ => {
            eprintln!(
                "usage: bench <service|read|sustained|throughput|plot|all> \
                 [--samples N] [--warmup N] [--core N] [--out DIR]"
            );
            std::process::exit(2);
        }
    }
}
