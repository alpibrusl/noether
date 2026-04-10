#!/bin/bash
# Noether ACLI Demo — run this to see Noether in action.
#
# Prerequisites:
#   - Rust toolchain (cargo)
#   - One LLM provider configured (see below)
#
# Provider setup (pick one):
#   export VERTEX_AI_PROJECT=your-project   # Google Cloud + Gemini
#   export OPENAI_API_KEY=sk-...            # OpenAI
#   export ANTHROPIC_API_KEY=sk-ant-...     # Anthropic
#   export MISTRAL_API_KEY=...              # Mistral
#
# Usage:
#   ./demo/run-demo.sh

set -euo pipefail

# ── Setup ────────────────────────────────────────────────────────────────────

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║              Noether ACLI Demo                              ║"
echo "║  Type-safe composition for AI agents                        ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""

# Find noether binary
if [ -f "../target/release/noether" ]; then
  export PATH="../target/release:$PATH"
elif ! command -v noether &>/dev/null; then
  echo "Building noether from source..."
  (cd .. && cargo build --release -p noether-cli 2>&1 | tail -2)
  export PATH="../target/release:$PATH"
fi

echo "Using: $(which noether)"
echo ""

# ── Demo 1: Stage Discovery ─────────────────────────────────────────────────

echo "━━━ Demo 1: Stage Discovery ━━━"
echo ""
echo "  Searching for CSV-related stages..."
echo "  \$ noether stage search \"parse CSV\""
echo ""
noether stage search "parse CSV" 2>/dev/null | python3 -c "
import sys, json
results = json.load(sys.stdin)['data']['results'][:3]
for r in results:
    print(f'  {r[\"score\"]:>6s}  {r[\"signature\"]:50s}  {r[\"description\"][:40]}')
"
echo ""

# ── Demo 2: Type Safety ─────────────────────────────────────────────────────

echo "━━━ Demo 2: Type Safety (dry-run catches errors) ━━━"
echo ""

# Valid graph
echo "  Valid pipeline: csv_parse → list_length → to_text"
echo "  \$ noether run --dry-run valid.json"
VALID=$(cat << 'EOF'
{"description":"demo","version":"0.1.0","root":{"op":"Sequential","stages":[
  {"op":"Stage","id":"72cdbe8850ff9f60c40dc3b4d40da7636c0673ff89c953508d1e782f03ebf023"},
  {"op":"Stage","id":"bb1b2e4dda7a8cb309b255c8c6d89a6befeb5df0aabbe0029b3ee888ac13c8d2"},
  {"op":"Stage","id":"85c780f2ac8543e9e8c25d194615e15b40b5afe1bb77bb02998f76588911f634"}
]}}
EOF
)
TMPG=$(mktemp /tmp/demo-XXXX.json)
echo "$VALID" > "$TMPG"
if noether run --dry-run "$TMPG" 2>/dev/null | grep -q '"ok": true'; then
  echo "  ✓ Type check passed"
else
  echo "  ✗ Unexpected failure"
fi
rm -f "$TMPG"

# Broken graph
echo ""
echo "  Broken pipeline: list_length → csv_parse (Number can't feed a Record)"
echo "  \$ noether run --dry-run broken.json"
BROKEN=$(cat << 'EOF'
{"description":"broken","version":"0.1.0","root":{"op":"Sequential","stages":[
  {"op":"Stage","id":"bb1b2e4dda7a8cb309b255c8c6d89a6befeb5df0aabbe0029b3ee888ac13c8d2"},
  {"op":"Stage","id":"72cdbe8850ff9f60c40dc3b4d40da7636c0673ff89c953508d1e782f03ebf023"}
]}}
EOF
)
TMPG=$(mktemp /tmp/demo-XXXX.json)
echo "$BROKEN" > "$TMPG"
ERR=$(noether run --dry-run "$TMPG" 2>&1 | grep -v '^Embedding\|^Warning\|^Nix' || true)
if echo "$ERR" | grep -q '"ok": false'; then
  echo "  ✓ Type error caught:"
  echo "    $(echo "$ERR" | python3 -c "import sys,json; print(json.load(sys.stdin)['error']['message'][:100])" 2>/dev/null)"
else
  echo "  ✗ Expected type error but got: $(echo "$ERR" | head -1)"
fi
rm -f "$TMPG"
echo ""

# ── Demo 3: Real Execution ──────────────────────────────────────────────────

echo "━━━ Demo 3: Execute a Pipeline ━━━"
echo ""
echo "  Input: CSV with student grades"
echo "  Pipeline: csv_parse → list_length → to_text"
echo ""

TMPG=$(mktemp /tmp/demo-XXXX.json)
echo "$VALID" > "$TMPG"
INPUT='{"text":"name,score,grade\nAlice,95,A\nBob,72,B\nCarol,88,A\nDave,61,C","has_header":true,"delimiter":null}'
RESULT=$(noether run "$TMPG" --input "$INPUT" 2>/dev/null)
OUTPUT=$(echo "$RESULT" | python3 -c "import sys,json; print(json.load(sys.stdin)['data']['output'])" 2>/dev/null)
DURATION=$(echo "$RESULT" | python3 -c "import sys,json; print(json.load(sys.stdin)['data']['trace']['duration_ms'])" 2>/dev/null)
echo "  Result: $OUTPUT students"
echo "  Execution time: ${DURATION}ms"
rm -f "$TMPG"
echo ""

# ── Demo 4: LLM-Powered Composition (optional) ──────────────────────────────

echo "━━━ Demo 4: noether compose (requires LLM provider) ━━━"
echo ""

# Check if any LLM provider is available
HAS_LLM=false
for var in VERTEX_AI_PROJECT OPENAI_API_KEY ANTHROPIC_API_KEY MISTRAL_API_KEY; do
  if [ -n "${!var:-}" ]; then
    HAS_LLM=true
    break
  fi
done

if $HAS_LLM; then
  PROBLEM="parse CSV data and count the number of rows"
  echo "  Problem: \"$PROBLEM\""
  echo "  \$ noether compose --dry-run \"$PROBLEM\""
  echo ""

  COMPOSE_OUT=$(noether compose --dry-run "$PROBLEM" 2>/dev/null || true)
  if echo "$COMPOSE_OUT" | grep -q '"ok": true'; then
    ATTEMPTS=$(echo "$COMPOSE_OUT" | python3 -c "import sys,json; print(json.load(sys.stdin)['data']['attempts'])" 2>/dev/null)
    STEPS=$(echo "$COMPOSE_OUT" | python3 -c "import sys,json; print(json.load(sys.stdin)['data']['plan']['steps'])" 2>/dev/null)
    TYPE_IN=$(echo "$COMPOSE_OUT" | python3 -c "import sys,json; print(json.load(sys.stdin)['data']['type_check']['input'][:50])" 2>/dev/null)
    TYPE_OUT=$(echo "$COMPOSE_OUT" | python3 -c "import sys,json; print(json.load(sys.stdin)['data']['type_check']['output'][:30])" 2>/dev/null)
    echo "  ✓ Composed in $ATTEMPTS attempt(s)"
    echo "    Steps: $STEPS"
    echo "    Types: $TYPE_IN → $TYPE_OUT"
  else
    echo "  ✗ Composition failed (check LLM provider config)"
  fi
else
  echo "  Skipped: no LLM provider configured."
  echo "  Set VERTEX_AI_PROJECT, OPENAI_API_KEY, ANTHROPIC_API_KEY, or MISTRAL_API_KEY"
fi

echo ""
echo "━━━ Done ━━━"
echo ""
echo "Learn more:"
echo "  noether introspect          # full command reference"
echo "  noether stage list          # browse all stages"
echo "  noether stage search \"...\" # semantic search"
