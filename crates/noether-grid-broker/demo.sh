#!/usr/bin/env bash
# noether-grid live demo
#
# Spawns a broker + 2 workers locally on free ports, enrols them, fires
# 5 sample jobs, and opens the dashboard. Suitable for screen recording.
#
# Usage:
#   ./demo.sh                # uses mock LLM credentials (no real spend)
#   ANTHROPIC_API_KEY=... ./demo.sh   # real Anthropic seat as one worker
#
# Exit:  Ctrl-C tears down everything.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BROKER_PORT=18088
WORKER_A_PORT=18089
WORKER_B_PORT=18090
LOG_DIR="$(mktemp -d)"
PIDS=()

cleanup() {
  echo
  echo "── Tearing down ─────────────────────────────────"
  for pid in "${PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
  echo "  logs preserved in $LOG_DIR"
}
trap cleanup EXIT INT TERM

echo "── Building noether-grid binaries ──────────────────"
( cd "$ROOT" && cargo build --release \
    -p noether-grid-broker -p noether-grid-worker 2>&1 \
    | tail -3 ) || { echo "build failed"; exit 1; }

BIN_BROKER="$ROOT/target/release/noether-grid-broker"
BIN_WORKER="$ROOT/target/release/noether-grid-worker"

echo
echo "── Spawning broker on :$BROKER_PORT ────────────────"
NOETHER_GRID_BIND="127.0.0.1:$BROKER_PORT" \
  RUST_LOG="noether_grid_broker=info" \
  "$BIN_BROKER" > "$LOG_DIR/broker.log" 2>&1 &
PIDS+=("$!")

# Wait for broker readiness
for _ in $(seq 1 30); do
  if curl -sf "http://127.0.0.1:$BROKER_PORT/health" > /dev/null; then break; fi
  sleep 0.2
done

echo "── Spawning worker A on :$WORKER_A_PORT ────────────"
ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-mock-anthropic-key}" \
  NOETHER_GRID_ANTHROPIC_BUDGET_CENTS=20000 \
  NOETHER_GRID_BROKER="http://127.0.0.1:$BROKER_PORT" \
  NOETHER_GRID_WORKER_BIND="127.0.0.1:$WORKER_A_PORT" \
  NOETHER_GRID_WORKER_URL="http://127.0.0.1:$WORKER_A_PORT" \
  "$BIN_WORKER" > "$LOG_DIR/worker-a.log" 2>&1 &
PIDS+=("$!")

echo "── Spawning worker B on :$WORKER_B_PORT ────────────"
OPENAI_API_KEY="${OPENAI_API_KEY:-mock-openai-key}" \
  NOETHER_GRID_OPENAI_BUDGET_CENTS=15000 \
  NOETHER_GRID_BROKER="http://127.0.0.1:$BROKER_PORT" \
  NOETHER_GRID_WORKER_BIND="127.0.0.1:$WORKER_B_PORT" \
  NOETHER_GRID_WORKER_URL="http://127.0.0.1:$WORKER_B_PORT" \
  "$BIN_WORKER" > "$LOG_DIR/worker-b.log" 2>&1 &
PIDS+=("$!")

# Wait for workers to enrol
for _ in $(seq 1 30); do
  count=$(curl -sf "http://127.0.0.1:$BROKER_PORT/workers" \
    | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null \
    || echo 0)
  [ "$count" -ge 2 ] && break
  sleep 0.5
done

echo
echo "── Pool registered ─────────────────────────────────"
curl -sf "http://127.0.0.1:$BROKER_PORT/workers" | python3 -m json.tool || true
echo

echo "── Submitting 5 demo jobs ──────────────────────────"
for i in 1 2 3 4 5; do
  resp=$(curl -sf -X POST "http://127.0.0.1:$BROKER_PORT/jobs" \
    -H 'Content-Type: application/json' \
    -d "{
          \"graph\": {\"description\": \"demo-job-$i\", \"version\": \"0.1.0\",
                      \"root\": {\"op\": \"Const\", \"value\": \"demo-result-$i\"}},
          \"input\": null
        }")
  job_id=$(echo "$resp" | python3 -c 'import json,sys; print(json.load(sys.stdin)["job_id"])')
  echo "  job $i → $job_id"
done

echo
echo "════════════════════════════════════════════════════"
echo "  Dashboard:  http://127.0.0.1:$BROKER_PORT"
echo "  /workers:   http://127.0.0.1:$BROKER_PORT/workers"
echo "  /metrics:   http://127.0.0.1:$BROKER_PORT/metrics"
echo "  Logs:       $LOG_DIR"
echo "════════════════════════════════════════════════════"
echo
echo "Press Ctrl-C to tear down."
wait
