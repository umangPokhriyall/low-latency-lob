#!/usr/bin/env bash
# bench/run_phase9_amd.sh — Phase 9 native pipeline-utilization profiling, vendor-aware
# (docs/specs/bare-metal-rerun-spec.md §4, §5.1, §A.8 "Re-run Session 3").
#
# THE PROFILER. Wraps the UNTIMED `bench profile` hot loop in `perf stat` for each of
# the four impls, capturing the vendor's pipeline-utilization metric group plus
# IPC / branch-miss / LLC-miss events into bench/results/perf/perf_<impl>.txt.
#
# Vendor detect (spec §4, §A.6): `grep -qi amd /proc/cpuinfo`.
#   - AMD Zen4: Intel's `-M TopdownL1,TopdownL2` DO NOT EXIST and error/mislead. The
#     pipeline group is HUMAN-SELECTED on the box from `perf list metricgroups`
#     (§A.6 comprehension gate) and handed in via $PERF_METRIC_GROUP — never guessed
#     here. This is AMD Zen4 pipeline-utilization analysis, the architectural
#     counterpart to Intel TMA, NOT Intel TMA relabeled.
#   - Intel: default `-M TopdownL1,TopdownL2` unless $PERF_METRIC_GROUP overrides.
#
# $PERF_METRIC_GROUP  (required on AMD)  — the `-M` pipeline-utilization group(s).
# $PERF_EVENTS        (optional)         — the `-e` raw event list. Defaults to the
#                     kernel's GENERIC aliases (instructions,cycles,branches,
#                     branch-misses,cache-references,cache-misses), which resolve on
#                     both vendors and yield IPC / branch-miss / LLC-miss portably.
#                     Override with exact Zen4 PMCs from §A.6 (ex_ret_ops,
#                     ex_ret_brn_misp/ex_ret_brn, ls_not_halted_cyc, amd_l3/...) when
#                     the human has confirmed them against `perf list` — do NOT
#                     hardcode raw AMD events blind (spec §4a).
#
# Env: PRODUCER_CORE (pin core, default 0); PROFILE_DEPTH/PROFILE_LOCALITY/PROFILE_ITERS.
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
require_bench

PRODUCER_CORE="${PRODUCER_CORE:-0}"
PROFILE_DEPTH="${PROFILE_DEPTH:-2048}"
PROFILE_LOCALITY="${PROFILE_LOCALITY:-uniform}"
PROFILE_ITERS="${PROFILE_ITERS:-200000000}"
PERF_EVENTS="${PERF_EVENTS:-instructions,cycles,branches,branch-misses,cache-references,cache-misses}"

if ! have perf; then
  echo "run_phase9_amd: perf not found — this is the PMU pass and cannot run off a perf-enabled host." >&2
  echo "  (non-perf paths run without perf; the metal box installs linux-tools per spec §A.3.)" >&2
  exit 1
fi

# --- vendor branch: pick the pipeline-utilization metric group ---
if is_amd; then
  : "${PERF_METRIC_GROUP:?AMD Zen4: set PERF_METRIC_GROUP from the §A.6 human gate (perf list metricgroups) — Intel TopdownL1/L2 do NOT exist on AMD}"
  echo "run_phase9_amd: vendor=AMD  metric-group='$PERF_METRIC_GROUP' (human-selected, §A.6)  events='$PERF_EVENTS'"
else
  PERF_METRIC_GROUP="${PERF_METRIC_GROUP:-TopdownL1,TopdownL2}"
  echo "run_phase9_amd: vendor=Intel  metric-group='$PERF_METRIC_GROUP'  events='$PERF_EVENTS'"
fi

mkdir -p "$PERF_DIR"

# Profile each impl's untimed apply hot loop. perf wraps ONLY this untimed target
# (never the timed sweeps — spec §4b). --core pins inside the single-core numactl mask.
for impl in btree sorted rev flat; do
  out="$PERF_DIR/perf_${impl}.txt"
  echo "run_phase9_amd: profiling impl=$impl -> $out"
  numa "$PRODUCER_CORE" -- \
    perf stat -M "$PERF_METRIC_GROUP" -e "$PERF_EVENTS" -o "$out" \
      -- "$BENCH" profile --impl "$impl" --op apply \
         --depth "$PROFILE_DEPTH" --locality "$PROFILE_LOCALITY" \
         --iters "$PROFILE_ITERS" --core "$PRODUCER_CORE"
done

echo "run_phase9_amd: done — raw counters in $PERF_DIR/perf_<impl>.txt"
echo "  NOTE (spec §4): re-confirm 'SortedVec is memory-bound, not speculation-bound'"
echo "  against the AMD BACKEND-BOUND bucket (expect high backend-bound, low bad-spec);"
echo "  cross-reference the branch-miss delta with branch_experiment.csv. HUMAN gate §A.6/§A.10."
