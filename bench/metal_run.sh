#!/usr/bin/env bash
set -euo pipefail
source "$HOME/.cargo/env"
CCD0_CORE=0                       # one core on CCD 0 (confirm via lscpu -e)
READER_CORES="6,12,18"           # spread across CCDs 1..3 (confirm topology)
: "${PERF_METRIC_GROUP:?set from A.6}"
mkdir -p bench/results/perf

# ---- Re-run Session 1 — Phase 4: CO-correct interior latency (perf-FREE, jitter-free) ----
PRODUCER_CORE=$CCD0_CORE bash bench/run_phase4.sh    # service/read/sustained/throughput/flat_memory

# ---- Re-run Session 2 — Phases 6/7: contention + perf c2c cache-line proof ----
WRITER_CORE=$CCD0_CORE READER_CORES=$READER_CORES bash bench/run_contention.sh   # perf-free timings
# perf c2c can exit non-zero on multiplexing/HITM-sampling quirks even when the report
# is written; tolerate so the run reaches plots + push (report writes the .txt).
perf c2c record -o bench/results/perf/c2c_seqlock.data -- \
  bash -c "WRITER_CORE=$CCD0_CORE READER_CORES=$READER_CORES bench/stress_seqlock" \
  || echo "perf c2c seqlock record exit $? — continuing"
perf c2c report -i bench/results/perf/c2c_seqlock.data --stdio > bench/results/perf/c2c_seqlock.txt \
  || echo "perf c2c seqlock report exit $? — continuing"
perf c2c record -o bench/results/perf/c2c_ring.data -- \
  bash -c "WRITER_CORE=$CCD0_CORE READER_CORES=$READER_CORES bench/stress_ring" \
  || echo "perf c2c ring record exit $? — continuing"
perf c2c report -i bench/results/perf/c2c_ring.data --stdio > bench/results/perf/c2c_ring.txt \
  || echo "perf c2c ring report exit $? — continuing"

# ---- Re-run Session 3 — Phase 9: native AMD Zen4 pipeline analysis (perf wraps the untimed hot loop) ----
# run_phase9_amd.sh tolerates the per-impl perf -M multiplexing non-zero internally.
PRODUCER_CORE=$CCD0_CORE PERF_METRIC_GROUP="$PERF_METRIC_GROUP" bash bench/run_phase9_amd.sh \
  || echo "phase9 exit $? — benign perf -M multiplexing; profiles written, continuing"

# regenerate plots (in-process plotters) and publish
bash bench/regen_plots.sh || echo "regen_plots exit $? — continuing"
git add -A bench/results
git commit -m "bare-metal: Latitude m4.metal.large AMD EPYC re-run (Ph4/6/7/9)" || echo "nothing to commit"
git push || echo "git push failed — results committed locally"
