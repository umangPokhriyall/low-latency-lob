#!/usr/bin/env bash
# bench/run_phase4.sh — Phase 4 CO-correct interior latency, perf-FREE, jitter-free
# (docs/specs/bare-metal-rerun-spec.md §5.1, §A.8 "Re-run Session 1").
#
# The LOB producer is pinned to ONE dedicated physical core on CCD 0 via
# `numactl --physcpubind=$PRODUCER_CORE --membind=$MEMBIND_NODE`; nothing else on
# that CCD, governor=performance. perf never wraps these timed sweeps (perf overhead
# corrupts ns numbers — spec §4b). Regenerates service / read / sustained /
# throughput / flat_memory.
#
# Env: PRODUCER_CORE (required by the metal run; defaults to 0 for a local smoke).
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
require_bench

PRODUCER_CORE="${PRODUCER_CORE:-0}"
echo "run_phase4: producer pinned to core $PRODUCER_CORE (membind node $MEMBIND_NODE), perf-free"

# Each timed sweep runs under the single-core numactl mask and takes --core so the
# bench pins its worker thread to exactly that core inside the mask.
numa "$PRODUCER_CORE" -- "$BENCH" service    --core "$PRODUCER_CORE" "$@"
numa "$PRODUCER_CORE" -- "$BENCH" read       --core "$PRODUCER_CORE" "$@"
numa "$PRODUCER_CORE" -- "$BENCH" sustained  --core "$PRODUCER_CORE" "$@"
numa "$PRODUCER_CORE" -- "$BENCH" throughput --core "$PRODUCER_CORE" "$@"
numa "$PRODUCER_CORE" -- "$BENCH" flatmem "$@"

echo "run_phase4: done"
