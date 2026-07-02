#!/usr/bin/env bash
# bench/run_contention.sh — Phases 6/7 contention, perf-FREE timings
# (docs/specs/bare-metal-rerun-spec.md §5.1, §A.8 "Re-run Session 2", first half).
#
# Writer/producer pinned to one CCD-0 core ($WRITER_CORE); readers/consumers spread
# ACROSS CCDs ($READER_CORES) to maximize cross-CCD coherence traffic. The numactl
# --physcpubind mask spans writer + all reader cores so the bench can pin each thread
# to its assigned core. perf-free (the perf c2c cache-line proof is a SEPARATE pass —
# see stress_seqlock / stress_ring wrapped by metal_run.sh).
#
# Env: WRITER_CORE (writer/producer core), READER_CORES (comma list, spread ACROSS CCDs).
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
require_bench

WRITER_CORE="${WRITER_CORE:-0}"
READER_CORES="${READER_CORES:-1}"
# physcpubind mask = writer core + every reader core.
CPUSET="$WRITER_CORE,$READER_CORES"
echo "run_contention: writer core $WRITER_CORE, reader cores $READER_CORES (mask $CPUSET, membind node $MEMBIND_NODE), perf-free"

# seqlock: writer core via --core / WRITER_CORE, readers via --reader-cores / READER_CORES.
WRITER_CORE="$WRITER_CORE" READER_CORES="$READER_CORES" \
  numa "$CPUSET" -- "$BENCH" seqlock --core "$WRITER_CORE" --reader-cores "$READER_CORES" "$@"

# ring: producer core via --core / PRODUCER_CORE, consumers via --reader-cores / READER_CORES.
PRODUCER_CORE="$WRITER_CORE" READER_CORES="$READER_CORES" \
  numa "$CPUSET" -- "$BENCH" ring --core "$WRITER_CORE" --reader-cores "$READER_CORES" "$@"

echo "run_contention: done"
