#!/bin/bash
set -euo pipefail
[ -f "../../target/release/noether" ] && export PATH="../../target/release:$PATH"

type_cmd() { echo ""; echo -n "$ "; for ((i=0; i<${#1}; i++)); do echo -n "${1:$i:1}"; sleep 0.04; done; echo ""; sleep 0.5; }
pause() { sleep "${1:-2}"; }
say() { echo -e "\033[36m$1\033[0m"; sleep 1; }

# Get stage IDs
REV_ID=$(python3 -c "
import json
with open('$HOME/.noether/store.json') as f:
    store = json.load(f)
for s in store['stages']:
    if 'Read a CSV file with name,revenue,region' in s.get('description','') and s.get('lifecycle') == 'Active':
        print(s['id']); break
")
DEALS_ID=$(python3 -c "
import json
with open('$HOME/.noether/store.json') as f:
    store = json.load(f)
for s in store['stages']:
    if 'count the number of deals per region' in s.get('description','') and s.get('lifecycle') == 'Active':
        print(s['id']); break
")
REPORT_ID=$(python3 -c "
import json
with open('$HOME/.noether/store.json') as f:
    store = json.load(f)
for s in store['stages']:
    if 'HTML sales report' in s.get('description','') and s.get('lifecycle') == 'Active':
        print(s['id']); break
")

clear
say "An agent needs a sales report from a CSV file."
say "Noether runs two aggregations in parallel and generates HTML."
pause 2

say ""
say "The CSV file:"
echo ""
cat /tmp/sales.csv | head -5
echo "  ... (8 rows, 3 regions)"
pause 2

say ""
say "The pipeline: parallel(revenue + deals + title) → HTML report"
echo ""
echo '  {
    "op": "Sequential",
    "stages": [
      { "op": "Parallel", "branches": {
          "revenue_by_region": { "Stage": "csv_group_revenue" },
          "deals_by_region":   { "Stage": "csv_group_deals"   },
          "title":             { "Const": "Q4 2025 Sales Report" }
      }},
      { "Stage": "html_sales_report" }
    ]
  }'
pause 3

GRAPH=$(mktemp /tmp/d1-XXXX.json)
cat > "$GRAPH" << EOF
{"description":"report","version":"0.1.0","root":{"op":"Sequential","stages":[{"op":"Parallel","branches":{"revenue_by_region":{"op":"Stage","id":"$REV_ID","config":null},"deals_by_region":{"op":"Stage","id":"$DEALS_ID","config":null},"title":{"op":"Const","value":"Q4 2025 Sales Report"}}},{"op":"Stage","id":"$REPORT_ID","config":null}]}}
EOF

say ""
say "Execute:"
type_cmd 'noether run sales-report.json --input '"'"'{"path":"/tmp/sales.csv"}'"'"''

RESULT=$(noether run "$GRAPH" --input '{"path":"/tmp/sales.csv"}' 2>/dev/null)
echo "$RESULT" | python3 -c "
import sys, json
d = json.load(sys.stdin)
if d['ok']:
    html = d['data']['output']
    # Save to file
    with open('/tmp/sales_report.html', 'w') as f:
        f.write(html)
    trace = d['data']['trace']
    print(f'  ✓ HTML report generated ({len(html)} chars)')
    print(f'    Time:   {trace[\"duration_ms\"]}ms')
    print(f'    Stages: {len(trace[\"stages\"])} executed')
    print(f'    Saved:  /tmp/sales_report.html')
    # Show summary from the HTML
    import re
    amounts = re.findall(r'\\$[\d,]+', html)
    if amounts:
        print(f'')
        print(f'  Report contains:')
        for a in amounts[:4]:
            print(f'    {a}')
else:
    print(f'  Error: {d[\"error\"][\"message\"][:80]}')
"
pause 4

say ""
say "Two parallel aggregations → one HTML report."
say "Summary cards, data table, bar charts. No pandas, no matplotlib."
pause 3

rm -f "$GRAPH"
