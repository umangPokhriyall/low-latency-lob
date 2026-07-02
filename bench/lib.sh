#!/usr/bin/env bash
# bench/lib.sh — shared plumbing for the bare-metal re-run wrappers
# (docs/specs/bare-metal-rerun-spec.md §A.0, §A.7, §A.8).
#
# Sourced by run_phase4.sh / run_contention.sh / stress_seqlock / stress_ring /
# run_phase9_amd.sh / regen_plots.sh. It provides:
#   - BENCH        : path to the release `bench` binary (override via $BENCH).
#   - MEMBIND_NODE : NUMA node for `numactl --membind` (default 0; override via env).
#   - numa()       : run a command under `numactl --physcpubind=<cores> --membind=<node>`,
#                    degrading gracefully to a bare run when numactl is unavailable
#                    (so the non-perf paths are testable locally off the metal box).
#   - is_amd()     : vendor detect for the profiler (`grep -qi amd /proc/cpuinfo`).
#
# NOTE: nothing here touches book/feed/sync (frozen). This is harness-only plumbing.

set -euo pipefail

# Repo root = the parent of this script's directory (bench/..).
BENCH_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$BENCH_LIB_DIR/.." && pwd)"

# The release binary the metal run builds in §A.4 (`cargo build --release`).
BENCH="${BENCH:-$REPO_ROOT/target/release/bench}"

# NUMA memory-binding node for --membind (single-socket NPS2 → node 0 holds CCD0).
MEMBIND_NODE="${MEMBIND_NODE:-0}"

# Where the perf/c2c artifacts land (created lazily by the callers).
PERF_DIR="${PERF_DIR:-$REPO_ROOT/bench/results/perf}"

have() { command -v "$1" >/dev/null 2>&1; }

# numa <cpu-list> -- <cmd...>
# Prefix a process with `numactl --physcpubind=<cpu-list> --membind=$MEMBIND_NODE`.
# The cpu-list is a comma-separated set of the physical cores the process may use
# (writer/producer + any reader/consumer cores); the bench then pins each thread to
# a specific core inside that mask. If numactl is missing (e.g. local laptop check),
# we warn once and run the command unpinned so the wrappers stay testable.
numa() {
  local cpus="$1"; shift
  [ "${1:-}" = "--" ] && shift
  if have numactl; then
    numactl --physcpubind="$cpus" --membind="$MEMBIND_NODE" -- "$@"
  else
    echo "bench/lib.sh: numactl not found; running unpinned (physcpubind=$cpus membind=$MEMBIND_NODE skipped)" >&2
    "$@"
  fi
}

# Vendor detect for the AMD-vs-Intel profiler branch (spec §4, §A.6).
is_amd() { grep -qi amd /proc/cpuinfo; }

# Fail early with a clear message if the release binary is absent.
require_bench() {
  if [ ! -x "$BENCH" ]; then
    echo "bench/lib.sh: release binary not found at $BENCH" >&2
    echo "  build it first:  (cd $REPO_ROOT && cargo build --release -p bench)" >&2
    exit 1
  fi
}
