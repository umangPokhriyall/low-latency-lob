//! `HdrHistogram` wrapper. Records ns latencies across a 1 ns .. 60 s dynamic
//! range at 3 significant figures, emits the §4.2 percentile set, and exports
//! the full interior distribution as a `.hgrm` text file for the log-y plots.

use hdrhistogram::Histogram;
use std::io::{BufWriter, Write};
use std::path::Path;

#[derive(Debug)]
pub struct Recorder {
    hist: Histogram<u64>,
}

impl Recorder {
    /// 1 ns lowest discernible value, 60 s highest trackable value, 3 sig figs:
    /// enough dynamic range for a sub-ns floor and a multi-second saturation tail.
    #[must_use]
    pub fn new() -> Self {
        Self {
            hist: Histogram::new_with_bounds(1, 60_000_000_000, 3)
                .expect("valid HdrHistogram bounds"),
        }
    }

    /// Record one ns latency. Clamped to ≥1 (the histogram's lowest bucket); a
    /// genuine 0 would be an elision artifact and is investigated upstream, not here.
    #[inline]
    pub fn record(&mut self, ns: u64) {
        let _ = self.hist.record(ns.max(1));
    }

    /// Value (ns) at quantile `q` in `0.0..=1.0`.
    #[must_use]
    pub fn p(&self, q: f64) -> u64 {
        self.hist.value_at_quantile(q)
    }

    #[must_use]
    pub fn mean(&self) -> f64 {
        self.hist.mean()
    }

    #[must_use]
    pub fn max(&self) -> u64 {
        self.hist.max()
    }

    #[must_use]
    pub fn count(&self) -> u64 {
        self.hist.len()
    }

    /// Export the full percentile distribution in the standard `HdrHistogram`
    /// `.hgrm` text layout (`Value` / `Percentile` / `TotalCount` /
    /// `1/(1-Percentile)`), the input the §9 interior-latency (log-y) plot consumes.
    pub fn export_hgrm(&self, path: &Path) -> std::io::Result<()> {
        let mut w = BufWriter::new(std::fs::File::create(path)?);
        writeln!(
            w,
            "{:>15} {:>15} {:>12} {:>15}",
            "Value(ns)", "Percentile", "TotalCount", "1/(1-Percentile)"
        )?;
        writeln!(w)?;
        for v in self.hist.iter_quantiles(5) {
            let q = v.quantile_iterated_to();
            let inv = if q < 1.0 { 1.0 / (1.0 - q) } else { f64::INFINITY };
            writeln!(
                w,
                "{:>15} {:>15.6} {:>12} {:>15.2}",
                v.value_iterated_to(),
                q,
                v.count_since_last_iteration(),
                inv
            )?;
        }
        writeln!(w)?;
        writeln!(
            w,
            "#[Mean    = {:>15.2}, StdDeviation = {:>15.2}]",
            self.hist.mean(),
            self.hist.stdev()
        )?;
        writeln!(
            w,
            "#[Max     = {:>15}, Total count  = {:>15}]",
            self.hist.max(),
            self.hist.len()
        )?;
        w.flush()
    }
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Percentiles are correct on a known uniform distribution 1..=1000.
    /// For a uniform set the q-th percentile value ≈ q·1000.
    #[test]
    fn percentiles_correct_on_known_distribution() {
        let mut rec = Recorder::new();
        for v in 1..=1000u64 {
            rec.record(v);
        }
        assert_eq!(rec.count(), 1000);

        // 3 sig figs => ~0.1% relative bucket width; allow a small tolerance.
        let p50 = rec.p(0.50);
        let p90 = rec.p(0.90);
        let p99 = rec.p(0.99);
        assert!((495..=505).contains(&p50), "p50 = {p50}, expected ~500");
        assert!((895..=905).contains(&p90), "p90 = {p90}, expected ~900");
        assert!((985..=1000).contains(&p99), "p99 = {p99}, expected ~990");

        // Mean of 1..=1000 is 500.5; the histogram mean is within sig-fig error.
        let mean = rec.mean();
        assert!((mean - 500.5).abs() < 2.0, "mean = {mean}, expected ~500.5");
        assert!(rec.max() >= 1000, "max = {}, expected >= 1000", rec.max());
    }

    /// A single recorded value reads back at every quantile (degenerate case).
    #[test]
    fn single_value_reads_back() {
        let mut rec = Recorder::new();
        rec.record(42);
        assert_eq!(rec.count(), 1);
        assert!((41..=43).contains(&rec.p(0.5)));
    }
}
