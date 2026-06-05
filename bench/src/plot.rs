//! Plot rendering (§9) and the `env.json` provenance manifest (§3.5).
//!
//! `bench plot` reads ONLY the committed CSVs under `results/` — the source of
//! truth — and renders `results/plots/*.svg` with `plotters`. No figure invents
//! data: each is a view of one CSV and its caption names that CSV. Interior
//! latency distributions come from the committed `.hgrm` exports. `env.json`
//! captures the measurement environment so a run is reproducible up to timing
//! noise. CSVs remain citeable regardless of the figures.

use crate::clock::BenchClock;
use crate::harness;
use plotters::prelude::*;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// One impl's plotting identity: registry tag and a stable line colour.
fn impl_color(name: &str) -> RGBColor {
    match name {
        "btree" => RGBColor(200, 30, 30),   // red
        "sorted" => RGBColor(30, 80, 200),  // blue
        "rev" => RGBColor(30, 150, 60),     // green
        "flat" => RGBColor(220, 130, 0),    // orange (Phase 5 FlatBook)
        _ => BLACK,
    }
}

/// The depth ladder, as the log-x axis for the service/read figures.
const DEPTHS: [u32; 12] = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048];

/// A named, coloured polyline.
type Series = (&'static str, RGBColor, Vec<(f64, f64)>);

/// Parse a committed CSV into rows of string cells (header dropped).
fn load_rows(path: &Path) -> Vec<Vec<String>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("warn: cannot read {} — skipping its figures", path.display());
        return Vec::new();
    };
    text.lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split(',').map(str::to_string).collect())
        .collect()
}

/// Render one log-x line chart (y linear or log), citing `source` in the caption.
/// x is always log (the depth/rate ladders span decades); markers sit on points.
fn render(
    out: &Path,
    title: &str,
    source: &str,
    x_desc: &str,
    y_desc: &str,
    y_log: bool,
    series: &[Series],
) -> Result<(), Box<dyn std::error::Error>> {
    if series.iter().all(|s| s.2.is_empty()) {
        eprintln!("warn: no data for {title} — skipped");
        return Ok(());
    }
    let (mut xmin, mut xmax, mut ymin, mut ymax) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for (_, _, pts) in series {
        for &(x, y) in pts {
            xmin = xmin.min(x);
            xmax = xmax.max(x);
            ymin = ymin.min(y);
            ymax = ymax.max(y);
        }
    }
    let xr = ((xmin * 0.8).max(0.5)..xmax * 1.25).log_scale();
    let caption = format!("{title}   (source: {source})");
    let root = SVGBackend::new(out, (940, 560)).into_drawing_area();
    root.fill(&WHITE)?;

    // x is log in both branches; y differs in type, so the body is duplicated.
    if y_log {
        let yr = ((ymin * 0.7).max(0.5)..ymax * 1.5).log_scale();
        let mut chart = ChartBuilder::on(&root)
            .caption(&caption, ("sans-serif", 17))
            .margin(14)
            .x_label_area_size(48)
            .y_label_area_size(70)
            .build_cartesian_2d(xr, yr)?;
        chart.configure_mesh().x_desc(x_desc).y_desc(y_desc).label_style(("sans-serif", 12)).draw()?;
        draw_series(&mut chart, series)?;
        chart.configure_series_labels().background_style(WHITE.mix(0.85)).border_style(BLACK).draw()?;
    } else {
        let yr = 0.0..ymax * 1.15;
        let mut chart = ChartBuilder::on(&root)
            .caption(&caption, ("sans-serif", 17))
            .margin(14)
            .x_label_area_size(48)
            .y_label_area_size(70)
            .build_cartesian_2d(xr, yr)?;
        chart.configure_mesh().x_desc(x_desc).y_desc(y_desc).label_style(("sans-serif", 12)).draw()?;
        draw_series(&mut chart, series)?;
        chart.configure_series_labels().background_style(WHITE.mix(0.85)).border_style(BLACK).draw()?;
    }
    root.present()?;
    eprintln!("wrote {}", out.display());
    Ok(())
}

/// Draw each series as a line plus point markers and register its legend entry.
fn draw_series<DB, X, Y>(
    chart: &mut ChartContext<'_, DB, Cartesian2d<X, Y>>,
    series: &[Series],
) -> Result<(), Box<dyn std::error::Error>>
where
    DB: DrawingBackend,
    X: Ranged<ValueType = f64>,
    Y: Ranged<ValueType = f64>,
    DB::ErrorType: 'static,
{
    for (name, color, pts) in series {
        if pts.is_empty() {
            continue;
        }
        let c = *color;
        chart
            .draw_series(LineSeries::new(pts.iter().copied(), ShapeStyle::from(c).stroke_width(2)))?
            .label(*name)
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 18, y)], c));
        chart.draw_series(pts.iter().map(move |&p| Circle::new(p, 3, c.filled())))?;
    }
    Ok(())
}

/// `update` latency vs depth for one locality+percentile, one line per impl.
fn crossover_figure(rows: &[Vec<String>], out_dir: &Path, locality: &str, pct_col: usize, pct_tag: &str) {
    let series: Vec<Series> = harness::IMPLS
        .iter()
        .map(|&name| {
            let pts = DEPTHS
                .iter()
                .filter_map(|&d| {
                    rows.iter().find(|r| {
                        r.len() > pct_col
                            && r[0] == name
                            && r[1] == locality
                            && r[3] == "update"
                            && r[2] == d.to_string()
                    })
                    .and_then(|r| r[pct_col].parse::<f64>().ok())
                    .map(|y| (f64::from(d), y))
                })
                .collect();
            (name, impl_color(name), pts)
        })
        .collect();
    let out = out_dir.join(format!("crossover_update_{pct_tag}_{locality}.svg"));
    let title = format!("apply update {pct_tag} vs depth — {locality}");
    if let Err(e) = render(&out, &title, "service_sweep.csv", "book depth (levels/side, log)", &format!("{pct_tag} latency (ns, log)"), true, &series) {
        eprintln!("warn: {title}: {e}");
    }
}

/// `best_bid` p50 vs depth — the read tax figure.
fn read_tax_figure(out_dir: &Path, results: &Path) {
    let rows = load_rows(&results.join("read_path.csv"));
    let series: Vec<Series> = harness::IMPLS
        .iter()
        .map(|&name| {
            let pts = DEPTHS
                .iter()
                .filter_map(|&d| {
                    rows.iter()
                        .find(|r| r.len() > 6 && r[0] == name && r[2] == "best_bid" && r[1] == d.to_string())
                        .and_then(|r| r[6].parse::<f64>().ok())
                        .map(|y| (f64::from(d), y))
                })
                .collect();
            (name, impl_color(name), pts)
        })
        .collect();
    let out = out_dir.join("read_best_bid_vs_depth.svg");
    if let Err(e) = render(&out, "best_bid p50 vs depth (read tax)", "read_path.csv", "book depth (levels/side, log)", "p50 latency (ns)", false, &series) {
        eprintln!("warn: read tax figure: {e}");
    }
}

/// CO-correct response p99 vs target rate for one synthetic profile.
fn sustained_figure(out_dir: &Path, rows: &[Vec<String>], profile: &str) {
    let series: Vec<Series> = harness::IMPLS
        .iter()
        .map(|&name| {
            let mut pts: Vec<(f64, f64)> = rows
                .iter()
                .filter(|r| r.len() > 8 && r[0] == name && r[1] == profile && r[2] == "fixed")
                .filter_map(|r| Some((r[3].parse::<f64>().ok()?, r[8].parse::<f64>().ok()?)))
                .collect();
            pts.sort_by(|a, b| a.0.total_cmp(&b.0));
            (name, impl_color(name), pts)
        })
        .collect();
    let out = out_dir.join(format!("sustained_p99_vs_rate_{profile}.svg"));
    let title = format!("CO-correct response p99 vs target rate — {profile}");
    if let Err(e) = render(&out, &title, "sustained.csv", "target rate (events/s, log)", "response p99 (ns, log)", true, &series) {
        eprintln!("warn: {title}: {e}");
    }
}

/// Interior latency distribution (value vs 1/(1-percentile)) from the `.hgrm`
/// exports at a crossover depth, one line per impl.
fn interior_figure(out_dir: &Path, results: &Path, locality: &str, depth: u32) {
    let series: Vec<Series> = harness::IMPLS
        .iter()
        .map(|&name| {
            let path = results.join(format!("service_update_{name}_{locality}_d{depth}.hgrm"));
            let pts = parse_hgrm(&path);
            (name, impl_color(name), pts)
        })
        .collect();
    let out = out_dir.join(format!("interior_update_{locality}_d{depth}.svg"));
    let title = format!("update interior distribution at D*={depth} — {locality}");
    if let Err(e) = render(&out, &title, &format!("service_update_*_{locality}_d{depth}.hgrm"), "percentile  1/(1-q)  (log)", "latency (ns, log)", true, &series) {
        eprintln!("warn: {title}: {e}");
    }
}

/// Seqlock read p99 vs reader count `K`, one line per writer mode — the
/// contention story (Benchmark 5 §6). `seqlock_read.csv` columns are, in order:
/// `readers, writer_mode, samples, clock_overhead_ns, read_p50_ns, read_p99_ns,
/// read_p999_ns, read_max_ns, mean_retries_per_load, write_p50_ns, write_p99_ns`.
fn seqlock_read_figure(out_dir: &Path, rows: &[Vec<String>]) {
    let series = seqlock_series_by_mode(rows, 5); // col 5 = read_p99_ns
    let out = out_dir.join("seqlock_read_p99_vs_readers.svg");
    if let Err(e) = render(&out, "seqlock read p99 vs reader count", "seqlock_read.csv", "reader threads K (log)", "read p99 latency (ns, log)", true, &series) {
        eprintln!("warn: seqlock read figure: {e}");
    }
}

/// Seqlock writer `store` p50 and p99 vs reader count `K` — the writer-independence
/// result (expected FLAT: readers do not tax the wait-free writer). Plotted
/// full-tilt only (worst case); linear y so flatness is read directly.
fn seqlock_write_figure(out_dir: &Path, rows: &[Vec<String>]) {
    let mut p50: Vec<(f64, f64)> = Vec::new();
    let mut p99: Vec<(f64, f64)> = Vec::new();
    for r in rows.iter().filter(|r| r.len() > 10 && r[1] == "full_tilt") {
        if let Ok(k) = r[0].parse::<f64>() {
            if let Ok(v) = r[9].parse::<f64>() {
                p50.push((k, v));
            }
            if let Ok(v) = r[10].parse::<f64>() {
                p99.push((k, v));
            }
        }
    }
    p50.sort_by(|a, b| a.0.total_cmp(&b.0));
    p99.sort_by(|a, b| a.0.total_cmp(&b.0));
    let series: Vec<Series> =
        vec![("store p50", RGBColor(30, 80, 200), p50), ("store p99", RGBColor(200, 30, 30), p99)];
    let out = out_dir.join("seqlock_write_vs_readers.svg");
    if let Err(e) = render(&out, "seqlock writer store latency vs reader count (full_tilt)", "seqlock_read.csv", "reader threads K (log)", "store latency (ns)", false, &series) {
        eprintln!("warn: seqlock write figure: {e}");
    }
}

/// Ring producer push throughput (Mev/s) vs consumer count `K`, one line per
/// producer mode — the **false-sharing** figure (expected FLAT in `full_tilt`:
/// consumers read distinct cache lines, so adding them does not tax the writer).
/// Linear y so flatness is read directly. `ring_bench.csv` columns are, in order:
/// `mode, consumers, capacity, words, samples, clock_overhead_ns, push_p50_ns,
/// push_p99_ns, recv_p50_ns, recv_p99_ns, producer_mev_s, overrun_rate`.
fn ring_throughput_figure(out_dir: &Path, rows: &[Vec<String>]) {
    let series = ring_series_by_mode(rows, 10); // col 10 = producer_mev_s
    let out = out_dir.join("ring_producer_throughput_vs_consumers.svg");
    if let Err(e) = render(&out, "ring producer push throughput vs consumer count (flat = no false sharing)", "ring_bench.csv", "consumer threads K (log)", "producer throughput (Mev/s)", false, &series) {
        eprintln!("warn: ring throughput figure: {e}");
    }
}

/// Ring `push` and `try_recv` p99 latency vs consumer count `K` (`full_tilt` — worst
/// case). Two series from `ring_bench.csv`; log y for the ns-scale tail.
fn ring_latency_figure(out_dir: &Path, rows: &[Vec<String>]) {
    let mut push: Vec<(f64, f64)> = Vec::new();
    let mut recv: Vec<(f64, f64)> = Vec::new();
    for r in rows.iter().filter(|r| r.len() > 9 && r[0] == "full_tilt") {
        if let Ok(k) = r[1].parse::<f64>() {
            if let Ok(v) = r[7].parse::<f64>() {
                push.push((k, v));
            }
            if let Ok(v) = r[9].parse::<f64>() {
                recv.push((k, v));
            }
        }
    }
    push.sort_by(|a, b| a.0.total_cmp(&b.0));
    recv.sort_by(|a, b| a.0.total_cmp(&b.0));
    let series: Vec<Series> =
        vec![("push p99", RGBColor(200, 30, 30), push), ("recv p99", RGBColor(30, 80, 200), recv)];
    let out = out_dir.join("ring_latency_p99_vs_consumers.svg");
    if let Err(e) = render(&out, "ring push/recv p99 latency vs consumer count (full_tilt)", "ring_bench.csv", "consumer threads K (log)", "p99 latency (ns, log)", true, &series) {
        eprintln!("warn: ring latency figure: {e}");
    }
}

/// A stable line colour per consumer-count `K` (the e2e figures plot one line per K).
fn k_color(k: u32) -> RGBColor {
    match k {
        1 => RGBColor(200, 30, 30),  // red
        2 => RGBColor(30, 80, 200),  // blue
        4 => RGBColor(30, 150, 60),  // green
        8 => RGBColor(220, 130, 0),  // orange
        _ => BLACK,
    }
}

/// End-to-end (production→consumption) p99 latency vs target rate, one line per
/// consumer count `K`, for the synthetic fixed-rate schedule — the headline CO-correct
/// distribution and where the tail blows up at saturation. `e2e.csv` columns are, in
/// order: `book, schedule, consumers, target_rate_eps, achieved_rate_eps, samples,
/// clock_overhead_ns, e2e_p50_ns, e2e_p99_ns, e2e_p999_ns, e2e_max_ns, producer_mev_s,
/// overrun_rate, saturated`.
fn e2e_p99_vs_rate_figure(out_dir: &Path, rows: &[Vec<String>]) {
    let series: Vec<Series> = [1u32, 2, 4, 8]
        .into_iter()
        .map(|k| {
            let mut pts: Vec<(f64, f64)> = rows
                .iter()
                .filter(|r| r.len() > 8 && r[1] == "fixed" && r[2] == k.to_string())
                .filter_map(|r| Some((r[3].parse::<f64>().ok()?, r[8].parse::<f64>().ok()?)))
                .collect();
            pts.sort_by(|a, b| a.0.total_cmp(&b.0));
            // Leaked tiny K-label string keeps the `&'static str` Series contract.
            let label: &'static str = Box::leak(format!("K={k}").into_boxed_str());
            (label, k_color(k), pts)
        })
        .collect();
    let out = out_dir.join("e2e_p99_vs_rate.svg");
    if let Err(e) = render(
        &out,
        "end-to-end p99 vs target rate (per K, synthetic fixed-rate)",
        "e2e.csv",
        "target rate (events/s, log)",
        "end-to-end p99 (ns, log)",
        true,
        &series,
    ) {
        eprintln!("warn: e2e p99 figure: {e}");
    }
}

/// Producer push throughput (Mev/s) vs consumer count `K` at the saturated
/// (free-running) synthetic rate — the **true-sharing** curve (expected to DECLINE:
/// every consumer reads the shared `write.v` cursor; this is true sharing, not false
/// sharing — the slots are `align(64)`-isolated). For each K the saturated operating
/// point is the highest swept target rate. Linear y so the decline is read directly.
fn e2e_producer_throughput_figure(out_dir: &Path, rows: &[Vec<String>]) {
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for k in [1u32, 2, 4, 8] {
        // The full-tilt operating point: the row with the max target rate for this K.
        let best = rows
            .iter()
            .filter(|r| r.len() > 11 && r[1] == "fixed" && r[2] == k.to_string())
            .filter_map(|r| Some((r[3].parse::<u64>().ok()?, r[11].parse::<f64>().ok()?)))
            .max_by_key(|&(rate, _)| rate);
        if let Some((_, mev)) = best {
            pts.push((f64::from(k), mev));
        }
    }
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let series: Vec<Series> = vec![("producer Mev/s", RGBColor(200, 30, 30), pts)];
    let out = out_dir.join("e2e_producer_throughput_vs_consumers.svg");
    if let Err(e) = render(
        &out,
        "producer throughput vs consumer count at saturation (true-sharing decline)",
        "e2e.csv",
        "consumer threads K (log)",
        "producer throughput (Mev/s)",
        false,
        &series,
    ) {
        eprintln!("warn: e2e producer-throughput figure: {e}");
    }
}

/// Branch-misprediction 2×2 (§3.1): p50 ns per lookup vs sorted-array depth, four
/// series — branchy/branchless × predictable/random. The branchy/random line sits
/// far above branchy/predictable (the misprediction penalty); both branchless
/// lines are flat and overlap (no data-dependent branch). Source
/// `branch_experiment.csv` (cols: `variant,key_pattern,depth,samples,oh,p50,p99,mean`).
fn branch_figure(out_dir: &Path, results: &Path) {
    let rows = load_rows(&results.join("branch_experiment.csv"));
    let specs: [(&'static str, &str, &str, RGBColor); 4] = [
        ("branchy/random", "branchy", "random", RGBColor(200, 30, 30)),
        ("branchy/predictable", "branchy", "predictable", RGBColor(235, 140, 140)),
        ("branchless/random", "branchless", "random", RGBColor(30, 80, 200)),
        ("branchless/predictable", "branchless", "predictable", RGBColor(120, 165, 235)),
    ];
    let series: Vec<Series> = specs
        .iter()
        .map(|(label, var, pat, color)| {
            let mut pts: Vec<(f64, f64)> = rows
                .iter()
                .filter(|r| r.len() > 5 && r[0] == *var && r[1] == *pat)
                .filter_map(|r| Some((r[2].parse::<f64>().ok()?, r[5].parse::<f64>().ok()?)))
                .collect();
            pts.sort_by(|a, b| a.0.total_cmp(&b.0));
            (*label, *color, pts)
        })
        .collect();
    let out = out_dir.join("branch_misprediction_2x2.svg");
    if let Err(e) = render(
        &out,
        "branch misprediction 2x2: p50 ns/lookup vs depth (branchy slow only on random)",
        "branch_experiment.csv",
        "sorted array depth (levels, log)",
        "p50 latency (ns/lookup, log)",
        true,
        &series,
    ) {
        eprintln!("warn: branch figure: {e}");
    }
}

/// Parse a `/sys` cache `size` string (`"48K"`, `"8192K"`, `"8M"`) to bytes.
fn parse_cache_size(s: &str) -> f64 {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('K') => (&s[..s.len() - 1], 1024.0),
        Some('M') => (&s[..s.len() - 1], 1024.0 * 1024.0),
        Some('G') => (&s[..s.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (s, 1.0),
    };
    num.trim().parse::<f64>().unwrap_or(0.0) * mult
}

/// Host L1d / L2 / LLC boundaries (bytes) for the cache figure's vertical lines.
fn cache_boundaries() -> Vec<(&'static str, f64, RGBColor)> {
    let read = |idx: usize, field: &str| {
        std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu0/cache/index{idx}/{field}"))
            .ok()
            .map(|s| s.trim().to_string())
    };
    let (mut l1d, mut l2, mut llc) = (0.0f64, 0.0f64, 0.0f64);
    for idx in 0..16 {
        let Some(level) = read(idx, "level") else { break };
        let kind = read(idx, "type").unwrap_or_default();
        let size = read(idx, "size").map_or(0.0, |s| parse_cache_size(&s));
        match (level.as_str(), kind.as_str()) {
            ("1", "Data") => l1d = size,
            ("2", _) => l2 = size,
            ("3", _) => llc = size,
            _ => {}
        }
    }
    vec![
        ("L1d", l1d, RGBColor(120, 120, 120)),
        ("L2", l2, RGBColor(120, 120, 120)),
        ("LLC", llc, RGBColor(120, 120, 120)),
    ]
}

/// Cache-footprint latency curve (§3.2): update p50 vs per-side footprint bytes,
/// one line per impl, with L1d/L2/LLC boundary lines annotated. Source
/// `cache_experiment.csv` (cols: `impl,depth,footprint_bytes,level,samples,oh,p50,p99`).
fn cache_figure(out_dir: &Path, results: &Path) {
    let rows = load_rows(&results.join("cache_experiment.csv"));
    let mut series: Vec<Series> = harness::IMPLS
        .iter()
        .map(|&name| {
            let mut pts: Vec<(f64, f64)> = rows
                .iter()
                .filter(|r| r.len() > 6 && r[0] == name)
                .filter_map(|r| Some((r[2].parse::<f64>().ok()?, r[6].parse::<f64>().ok()?)))
                .collect();
            pts.sort_by(|a, b| a.0.total_cmp(&b.0));
            (name, impl_color(name), pts)
        })
        .collect();
    if series.iter().all(|s| s.2.is_empty()) {
        eprintln!("warn: no data for cache figure — skipped");
        return;
    }
    let ymax = series.iter().flat_map(|s| s.2.iter().map(|p| p.1)).fold(1.0f64, f64::max);
    for (label, sz, color) in cache_boundaries() {
        if sz > 0.0 {
            series.push((label, color, vec![(sz, 0.5), (sz, ymax * 1.2)]));
        }
    }
    let out = out_dir.join("cache_footprint_latency.svg");
    if let Err(e) = render(
        &out,
        "apply update p50 vs per-side footprint (vertical lines = L1d/L2/LLC)",
        "cache_experiment.csv",
        "per-side footprint (bytes, log)",
        "update p50 latency (ns, log)",
        true,
        &series,
    ) {
        eprintln!("warn: cache figure: {e}");
    }
}

/// Build one series per producer mode from a numeric column of `ring_bench.csv`
/// (x = consumer count, col 1).
fn ring_series_by_mode(rows: &[Vec<String>], col: usize) -> Vec<Series> {
    [("full_tilt", RGBColor(200, 30, 30)), ("paced", RGBColor(30, 150, 60))]
        .into_iter()
        .map(|(mode, color)| {
            let mut pts: Vec<(f64, f64)> = rows
                .iter()
                .filter(|r| r.len() > col && r[0] == mode)
                .filter_map(|r| Some((r[1].parse::<f64>().ok()?, r[col].parse::<f64>().ok()?)))
                .collect();
            pts.sort_by(|a, b| a.0.total_cmp(&b.0));
            (mode, color, pts)
        })
        .collect()
}

/// Build one series per writer mode from a numeric column of `seqlock_read.csv`.
fn seqlock_series_by_mode(rows: &[Vec<String>], col: usize) -> Vec<Series> {
    [("full_tilt", RGBColor(200, 30, 30)), ("paced", RGBColor(30, 150, 60))]
        .into_iter()
        .map(|(mode, color)| {
            let mut pts: Vec<(f64, f64)> = rows
                .iter()
                .filter(|r| r.len() > col && r[1] == mode)
                .filter_map(|r| Some((r[0].parse::<f64>().ok()?, r[col].parse::<f64>().ok()?)))
                .collect();
            pts.sort_by(|a, b| a.0.total_cmp(&b.0));
            (mode, color, pts)
        })
        .collect()
}

/// Parse an `.hgrm`: rows of `value percentile count 1/(1-pct)`; x=col 3, y=col 0.
/// Skips the header, the trailing `#[..]` summary lines, and the `inf` tail row.
fn parse_hgrm(path: &Path) -> Vec<(f64, f64)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut pts = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = t.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        let (Ok(value), Ok(inv)) = (cols[0].parse::<f64>(), cols[3].parse::<f64>()) else {
            continue;
        };
        if inv.is_finite() && inv >= 1.0 {
            pts.push((inv, value.max(1.0)));
        }
    }
    pts
}

/// Entry point for `bench plot [--out DIR]`. Writes `env.json` first (§3.5), then
/// renders every §9 figure from the committed CSVs/`.hgrm`s.
pub fn run(args: &[String]) {
    let results = parse_out(args);
    let plots = results.join("plots");
    std::fs::create_dir_all(&plots).expect("create plots dir");

    if let Err(e) = write_env_json(&results) {
        eprintln!("warn: env.json: {e}");
    }

    let service = load_rows(&results.join("service_sweep.csv"));
    // Crossover (the headline): update p50 & p99 vs depth, per locality.
    // service_sweep columns: impl,locality,depth,op,samples,oh,mean,p50,p90,p99,p999,max
    for loc in ["concentrated", "uniform"] {
        crossover_figure(&service, &plots, loc, 7, "p50");
        crossover_figure(&service, &plots, loc, 9, "p99");
    }
    // Interior distributions at the per-locality crossover depths (Session 1's
    // committed .hgrm exports: D*=256 concentrated, D*=2 uniform).
    interior_figure(&plots, &results, "concentrated", 256);
    interior_figure(&plots, &results, "uniform", 2);

    read_tax_figure(&plots, &results);

    let sustained = load_rows(&results.join("sustained.csv"));
    for profile in ["steady", "burst", "flashcrash"] {
        sustained_figure(&plots, &sustained, profile);
    }

    // Benchmark 5 (Phase 6): seqlock read p99 vs K, and the writer-independence figure.
    let seqlock = load_rows(&results.join("seqlock_read.csv"));
    seqlock_read_figure(&plots, &seqlock);
    seqlock_write_figure(&plots, &seqlock);

    // Benchmark 6 (Phase 7): ring producer throughput vs K (the false-sharing test,
    // expected flat) and push/recv p99 vs K.
    let ring = load_rows(&results.join("ring_bench.csv"));
    ring_throughput_figure(&plots, &ring);
    ring_latency_figure(&plots, &ring);

    // Benchmark 7 (Phase 8): end-to-end p99 vs rate (per K) and the producer-
    // throughput-vs-K true-sharing curve.
    let e2e = load_rows(&results.join("e2e.csv"));
    e2e_p99_vs_rate_figure(&plots, &e2e);
    e2e_producer_throughput_figure(&plots, &e2e);

    // Phase 9 (microarchitecture teardown): the misprediction 2×2 and the
    // cache-footprint latency curve, each citing its experiment CSV.
    branch_figure(&plots, &results);
    cache_figure(&plots, &results);

    eprintln!("plots in {}", plots.display());
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

// --- env.json (§3.5 provenance) ---------------------------------------------

/// The corpora whose provenance is recorded (path relative to manifest dir).
const CORPORA: [(&str, &str); 4] = [
    ("steady", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/steady-s1-100k.mdf")),
    ("burst", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/burst-s1-100k.mdf")),
    ("flashcrash", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/flashcrash-s1-100k.mdf")),
    ("btcusdt-sample", concat!(env!("CARGO_MANIFEST_DIR"), "/../feed/corpus/btcusdt-sample.mdf")),
];

/// Run a command, returning trimmed stdout or `"unknown"`.
fn cmd(bin: &str, args: &[&str]) -> String {
    std::process::Command::new(bin)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// First matching `key` value from `/proc/cpuinfo`, or `"unknown"`.
fn cpuinfo(key: &str) -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|t| {
            t.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split(':').nth(1))
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn read_trim(path: &str) -> String {
    std::fs::read_to_string(path).map_or_else(|_| "unknown".to_string(), |s| s.trim().to_string())
}

/// Corpus provenance without a hashing dep (§3.5 allows length + edge bytes):
/// length and the first/last 16 bytes in hex uniquely fingerprint a corpus.
fn corpus_provenance(path: &Path) -> Option<(u64, String, String)> {
    let bytes = std::fs::read(path).ok()?;
    let n = bytes.len();
    let head = hex(&bytes[..n.min(16)]);
    let tail = hex(&bytes[n.saturating_sub(16)..]);
    Some((n as u64, head, tail))
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Write `results/env.json`: CPU, cores, governor, kernel, rustc, target-cpu,
/// git commit, pinned core, measured `clock_overhead_ns`, and corpus fingerprints.
fn write_env_json(results: &Path) -> std::io::Result<()> {
    let clock = BenchClock::new();
    let cores = cmd("nproc", &[]);
    let governor = read_trim("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor");
    let kernel = cmd("uname", &["-r"]);
    let rustc = cmd("rustc", &["--version"]);
    let commit = cmd("git", &["rev-parse", "HEAD"]);
    let cpu = cpuinfo("model name");

    let mut s = String::new();
    s.push_str("{\n");
    let _ = writeln!(s, "  \"cpu_model\": \"{}\",", json_escape(&cpu));
    let _ = writeln!(s, "  \"logical_cores\": \"{}\",", json_escape(&cores));
    let _ = writeln!(s, "  \"cpu_governor\": \"{}\",", json_escape(&governor));
    let _ = writeln!(s, "  \"kernel\": \"{}\",", json_escape(&kernel));
    let _ = writeln!(s, "  \"rustc\": \"{}\",", json_escape(&rustc));
    let _ = writeln!(s, "  \"target_cpu\": \"native\",");
    let _ = writeln!(s, "  \"git_commit\": \"{}\",", json_escape(&commit));
    let _ = writeln!(s, "  \"pinned_core\": 0,");
    let _ = writeln!(s, "  \"clock_overhead_ns\": {},", clock.overhead_ns());
    s.push_str("  \"corpora\": [\n");
    let last = CORPORA.len() - 1;
    for (i, (name, path)) in CORPORA.iter().enumerate() {
        let comma = if i == last { "" } else { "," };
        match corpus_provenance(Path::new(path)) {
            Some((len, head, tail)) => {
                let _ = writeln!(
                    s,
                    "    {{ \"name\": \"{name}\", \"bytes\": {len}, \"first16_hex\": \"{head}\", \"last16_hex\": \"{tail}\" }}{comma}"
                );
            }
            None => {
                let _ = writeln!(s, "    {{ \"name\": \"{name}\", \"bytes\": null }}{comma}");
            }
        }
    }
    s.push_str("  ]\n}\n");

    let path = results.join("env.json");
    std::fs::write(&path, s)?;
    eprintln!("wrote {}", path.display());
    Ok(())
}
