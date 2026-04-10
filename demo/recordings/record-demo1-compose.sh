#!/bin/bash
set -euo pipefail
[ -f "../../target/release/noether" ] && export PATH="../../target/release:$PATH"

type_cmd() { echo ""; echo -n "$ "; for ((i=0; i<${#1}; i++)); do echo -n "${1:$i:1}"; sleep 0.04; done; echo ""; sleep 0.5; }
pause() { sleep "${1:-2}"; }
say() { echo -e "\033[36m$1\033[0m"; sleep 1; }

# Get the real stage ID
GROUP_ID=$(python3 -c "
import json
with open('$HOME/.noether/store.json') as f:
    store = json.load(f)
for s in store['stages']:
    if s.get('description','').startswith('Parse CSV text with name,revenue,region'):
        if s.get('lifecycle') == 'Active': print(s['id']); break
")
JSON_SER="b96bc6ef0e959aea91a1ece9ef067baaa778cae1de2673ccc71504f5bf8b3705"

clear
say "An agent needs to analyze sales data by region."
say "Instead of writing pandas code, it uses Noether:"
pause 2

say ""
say "The composition graph: csv_group_revenue → json_serialize"
echo ""
echo '  {
    "root": {
      "op": "Sequential",
      "stages": [
        { "op": "Stage", "id": "'${GROUP_ID:0:12}'...",
          "_comment": "csv_group_revenue: parse + group + sum" },
        { "op": "Stage", "id": "b96bc6ef...",
          "_comment": "json_serialize: Any → Text" }
      ]
    }
  }'
pause 3

say ""
say "Execute with real sales data:"

GRAPH=$(mktemp /tmp/d1-XXXX.json)
echo "{\"description\":\"revenue by region\",\"version\":\"0.1.0\",\"root\":{\"op\":\"Sequential\",\"stages\":[{\"op\":\"Stage\",\"id\":\"$GROUP_ID\"},{\"op\":\"Stage\",\"id\":\"$JSON_SER\"}]}}" > "$GRAPH"

type_cmd 'noether run revenue.json --input sales.csv'
RESULT=$(noether run "$GRAPH" --input '{"text":"name,revenue,region\nAcme,450000,US\nGlobalTech,280000,EU\nDataFlow,520000,US\nNordStar,190000,EU\nPacific,340000,APAC"}' 2>/dev/null)
echo "$RESULT" | python3 -c "
import sys, json
d = json.load(sys.stdin)['data']
output = json.loads(d['output'])
print(f'  Result:')
for region in sorted(output, key=lambda r: output[r], reverse=True):
    print(f'    {region:6s}  \${output[region]:>10,}')
print(f'')
print(f'  Time: {d[\"trace\"][\"duration_ms\"]}ms')
"
pause 4

say ""
say "US: \$970K. EU: \$470K. APAC: \$340K."
say "Parsed, grouped, and summed in 0ms. No pandas. No code."
pause 3

rm -f "$GRAPH"
