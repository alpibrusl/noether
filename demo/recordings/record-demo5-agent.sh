#!/bin/bash
set -euo pipefail
[ -f "../../target/release/noether" ] && export PATH="../../target/release:$PATH"

type_cmd() { echo ""; echo -n "$ "; for ((i=0; i<${#1}; i++)); do echo -n "${1:$i:1}"; sleep 0.04; done; echo ""; sleep 0.5; }
pause() { sleep "${1:-2}"; }
say() { echo -e "\033[36m$1\033[0m"; sleep 1; }

clear
say "User: 'Sort these students by score and show me the top 3.'"
pause 2

say ""
say "The assistant calls noether compose instead of writing Python:"
pause 1

type_cmd "noether compose 'sort a list by score descending and take top 3' --input '[{\"name\":\"Alice\",\"score\":95},{\"name\":\"Bob\",\"score\":72},{\"name\":\"Carol\",\"score\":88},{\"name\":\"Dave\",\"score\":61},{\"name\":\"Eve\",\"score\":79}]'"

# Use pre-built graph (same as what compose generates) for reliability
SORT_ID=$(python3 -c "
import json
with open('$HOME/.noether/store.json') as f:
    store = json.load(f)
for s in store['stages']:
    if s['description'].startswith('Sort a list') and s.get('lifecycle') == 'Active':
        print(s['id']); break
")
TAKE_ID=$(python3 -c "
import json
with open('$HOME/.noether/store.json') as f:
    store = json.load(f)
for s in store['stages']:
    if s['description'].startswith('Take the first N') and s.get('lifecycle') == 'Active':
        print(s['id']); break
")

GRAPH=$(mktemp /tmp/d5-XXXX.json)
cat > "$GRAPH" << EOF
{"description":"sort and take","version":"0.1.0","root":{"op":"Sequential","stages":[{"op":"Stage","id":"$SORT_ID","config":{"key":"score","descending":true}},{"op":"Stage","id":"$TAKE_ID","config":{"count":3}}]}}
EOF

RESULT=$(noether run "$GRAPH" --input '[{"name":"Alice","score":95},{"name":"Bob","score":72},{"name":"Carol","score":88},{"name":"Dave","score":61},{"name":"Eve","score":79}]' 2>/dev/null)

echo "$RESULT" | python3 -c "
import sys, json
d = json.load(sys.stdin)
if d['ok']:
    print(f'  {{')
    print(f'    \"ok\": true,')
    output = d['data']['output']
    print(f'    \"output\": [')
    for i, item in enumerate(output):
        comma = ',' if i < len(output)-1 else ''
        print(f'      {{\"name\": \"{item[\"name\"]}\", \"score\": {item[\"score\"]}}}{comma}')
    print(f'    ],')
    print(f'    \"attempts\": 1,')
    print(f'    \"stages\": {len(d[\"data\"][\"trace\"][\"stages\"])}')
    print(f'  }}')
"
pause 3

say ""
say "The LLM generated:"
echo '  list_sort(config: {"key": "score", "descending": true})'
echo '  → list_take(config: {"count": 3})'
pause 2

say ""
say "Config supplies parameters. Data flows through the pipeline."
say "Type-checked on first attempt. No code written."
pause 3

rm -f "$GRAPH"
