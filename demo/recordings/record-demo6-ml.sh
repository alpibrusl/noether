#!/bin/bash
set -euo pipefail
[ -f "../../target/release/noether" ] && export PATH="../../target/release:$PATH"

type_cmd() { echo ""; echo -n "$ "; for ((i=0; i<${#1}; i++)); do echo -n "${1:$i:1}"; sleep 0.04; done; echo ""; sleep 0.5; }
pause() { sleep "${1:-2}"; }
say() { echo -e "\033[36m$1\033[0m"; sleep 1; }

# Get stage IDs
JSON_READ=$(python3 -c "
import json
with open('$HOME/.noether/store.json') as f:
    s = json.load(f)
for x in s['stages']:
    if 'Read and parse a JSON file' in x.get('description','') and x.get('lifecycle')=='Active':
        print(x['id']); break
")
TRAIN_ID=$(python3 -c "
import json
with open('$HOME/.noether/store.json') as f:
    s = json.load(f)
for x in s['stages']:
    if 'Train a scikit-learn model' in x.get('description','') and x.get('lifecycle')=='Active':
        print(x['id']); break
")
PREDICT_ID=$(python3 -c "
import json
with open('$HOME/.noether/store.json') as f:
    s = json.load(f)
for x in s['stages']:
    if 'Load a trained sklearn model and add' in x.get('description','') and x.get('lifecycle')=='Active':
        print(x['id']); break
")
EVAL_ID=$(python3 -c "
import json
with open('$HOME/.noether/store.json') as f:
    s = json.load(f)
for x in s['stages']:
    if 'Evaluate model predictions' in x.get('description','') and x.get('lifecycle')=='Active':
        print(x['id']); break
")

clear
say "Noether ML Pipeline: Train → Predict → Evaluate"
say "Dataset: Iris flowers (15 samples, 3 species)"
pause 2

say ""
say "Step 1: Read data + train RandomForest"
echo ""
echo '  Pipeline: json_read → sklearn_train(config: {
    target: "species",
    model: "RandomForestClassifier",
    params: {n_estimators: 10}
  })'
pause 2

GRAPH=$(mktemp /tmp/ml-XXXX.json)
cat > "$GRAPH" << EOF
{"description":"train","version":"0.1.0","root":{"op":"Sequential","stages":[
  {"op":"Stage","id":"$JSON_READ"},
  {"op":"Stage","id":"$TRAIN_ID","config":{"target":"species","model":"RandomForestClassifier","params":{"n_estimators":10,"random_state":42},"save_path":"/tmp/iris_rf.pkl"}}
]}}
EOF

type_cmd 'noether run train.json --input '"'"'{"path": "/tmp/iris_train.json"}'"'"''
RESULT=$(noether run "$GRAPH" --input '{"path":"/tmp/iris_train.json"}' 2>/dev/null)
echo "$RESULT" | python3 -c "
import sys, json
d = json.load(sys.stdin)['data']['output']
print(f'  Model:    {d[\"model_type\"]}')
print(f'  Features: {d[\"feature_names\"]}')
print(f'  Samples:  {d[\"train_samples\"]}')
print(f'  Saved:    {d[\"model_path\"]}')
"
rm -f "$GRAPH"
pause 3

say ""
say "Step 2: Predict + Evaluate on same data"
echo ""
echo '  Pipeline: json_read → sklearn_predict(config: {model_path})
           → sklearn_evaluate(config: {target, predicted})'
pause 2

GRAPH=$(mktemp /tmp/ml-XXXX.json)
cat > "$GRAPH" << EOF
{"description":"eval","version":"0.1.0","root":{"op":"Sequential","stages":[
  {"op":"Stage","id":"$JSON_READ"},
  {"op":"Stage","id":"$PREDICT_ID","config":{"model_path":"/tmp/iris_rf.pkl"}},
  {"op":"Stage","id":"$EVAL_ID","config":{"target":"species","predicted":"prediction"}}
]}}
EOF

type_cmd 'noether run evaluate.json --input '"'"'{"path": "/tmp/iris_train.json"}'"'"''
RESULT=$(noether run "$GRAPH" --input '{"path":"/tmp/iris_train.json"}' 2>/dev/null)
echo "$RESULT" | python3 -c "
import sys, json
d = json.load(sys.stdin)['data']['output']
print(f'  Accuracy:  {d[\"accuracy\"]}')
print(f'  Precision: {d[\"precision\"]}')
print(f'  Recall:    {d[\"recall\"]}')
print(f'  F1:        {d[\"f1\"]}')
print(f'  Samples:   {d[\"samples\"]}')
"
rm -f "$GRAPH"
pause 3

say ""
say "100% accuracy. Two pipelines, 5 stages, zero Python written."
say "Config provides parameters. Data flows through the pipeline."
pause 3
