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
        "btree" => RGBColor(200, 30, 30),  // red
        "sorted" => RGBColor(30, 80, 200), // blue
        "rev" => RGBColor(30, 150, 60),    // green
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
