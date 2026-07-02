#!/usr/bin/env bash
# bench/regen_plots.sh — regenerate every §9 figure + env.json from the committed CSVs
# (docs/specs/bare-metal-rerun-spec.md §A.8). In-process plotters; no perf, no pinning
# needed (this only reads the CSVs the re-run sessions just wrote).
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
require_bench

echo "regen_plots: rendering plots + env.json from bench/results/*.csv"
"$BENCH" plot "$@"
echo "regen_plots: done"
