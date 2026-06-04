//! `FlatBook` memory footprint (§5.4) — the source for the memory-vs-speed
//! tradeoff in the final verdict.
//!
//! `FlatBook`'s cost is memory proportional to the price **span**, not the
//! occupied-level count. This records its allocated span (ticks and bytes) for
//! every benchmark configuration so the tradeoff is sourced from a committed CSV
//! rather than asserted: the service-sweep depth ladder (the array is sized to
//! each depth's band) and each replayed corpus (the span the real/synthetic feed
//! drives the array to). Span is read through the public
//! `FlatBook::allocated_span_ticks`; bytes = `2 * span * size_of::<Qty>()` (two
//! parallel per-side arrays). This is not a timed benchmark — it measures memory.

use crate::workload::build_at_depth;
use book::{FlatBook, OrderBook, Px, Qty};
use feed::corpus::Corpus;
use std::path::{Path, PathBuf};

/// Mid price the service ladder is built around (matches `service.rs`/`read.rs`).
const MID: Px = Px(1_000_000);

/// The service-sweep depth ladder (matches `service.rs`).
const DEPTHS: [usize; 12] = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048];

/// The corpora replayed by the throughput/sustained benchmarks (same paths).
const CORPORA: [(&str, &str); 4] = [
    ("steady", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/steady-s1-100k.mdf")),
    ("burst", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/burst-s1-100k.mdf")),
    ("flashcrash", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/flashcrash-s1-100k.mdf")),
    ("btcusdt-sample", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/btcusdt-sample.mdf")),
];

/// One CSV row: a config, its key (depth or corpus tag), span in ticks, and the
/// total allocated bytes across both per-side arrays.
#[derive(Debug)]
struct Row {
    config: &'static str,
    key: String,
    span_ticks: usize,
    total_bytes: usize,
}

/// Bytes held by the two parallel `Vec<Qty>` arrays of width `span` ticks.
fn bytes_for(span: usize) -> usize {
    2 * span * std::mem::size_of::<Qty>()
}

fn parse_out(args: &[String]) -> PathBuf {
    let mut out = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/results"));
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--out" {
            if let Some(d) = it.next() {
                out = PathBuf::from(d);
            }
        }
    }
    out
}

/// Entry point for `bench flatmem [--out DIR]`. Writes `flat_memory.csv`.
pub fn run(args: &[String]) {
    let out_dir = parse_out(args);
    std::fs::create_dir_all(&out_dir).expect("create results dir");

    let mut rows: Vec<Row> = Vec::new();

    // Service-sweep configs: the array is sized to each depth's band (memory grows
    // with span, not depth — the band here is `mid ± STEP*depth`).
    for &depth in &DEPTHS {
        let book = build_at_depth::<FlatBook>(MID, depth);
        let span = book.allocated_span_ticks();
        rows.push(Row { config: "service_depth", key: depth.to_string(), span_ticks: span, total_bytes: bytes_for(span) });
    }

    // Corpus configs: the span the full replay drives the array to.
    for &(tag, path) in &CORPORA {
        match Corpus::load(Path::new(path)) {
            Ok(corpus) => {
                let mut book = FlatBook::default();
                for ev in corpus.events() {
                    book.apply(ev);
                }
                let span = book.allocated_span_ticks();
                rows.push(Row { config: "corpus", key: tag.to_string(), span_ticks: span, total_bytes: bytes_for(span) });
            }
            Err(e) => eprintln!("warn: skip corpus {tag} ({path}): {e}"),
        }
    }

    write_csv(&out_dir.join("flat_memory.csv"), &rows);
    print_headline(&rows);
}

fn write_csv(path: &Path, rows: &[Row]) {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str("config,key,span_ticks,total_bytes\n");
    for r in rows {
        let _ = writeln!(s, "{},{},{},{}", r.config, r.key, r.span_ticks, r.total_bytes);
    }
    std::fs::write(path, s).expect("write flat_memory.csv");
    eprintln!("wrote {}", path.display());
}

#[allow(clippy::cast_precision_loss)]
fn print_headline(rows: &[Row]) {
    eprintln!("\n==== FLATBOOK MEMORY (allocated span) ====");
    for r in rows {
        eprintln!(
            "  {:<13} {:<16} span={:>9} ticks  {:>10} bytes ({:.2} MiB)",
            r.config,
            r.key,
            r.span_ticks,
            r.total_bytes,
            r.total_bytes as f64 / (1024.0 * 1024.0),
        );
    }
}
