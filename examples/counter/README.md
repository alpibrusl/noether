# Counter Example

A minimal interactive counter app that demonstrates the full Noether browser UI pipeline:

```
Rust stage (VNode output) → noether build --target browser → index.html
```

## Files

| File | Description |
|---|---|
| `counter-stage.json` | Stage spec: `Record { count: Number } → VNode` |
| `graph.json` | Single-stage Lagrange graph |
| `README.md` | This file |

## Prerequisites

```bash
cargo install wasm-pack    # install the WASM toolchain
rustup target add wasm32-unknown-unknown
```

## Setup

Register the counter stage in your local Noether store:

```bash
noether stage add examples/counter/counter-stage.json
```

The CLI will print the stage ID and sign it with your local author key.

## Build

```bash
noether build examples/counter/graph.json --target browser -o dist/counter/
```

This produces:

```
dist/counter/
  index.html       ← open in any browser
  noether_bg.wasm  ← compiled stage graph
  noether.js       ← wasm-bindgen glue
```

## Run

Open `dist/counter/index.html` in a browser. Because the app loads a `.wasm` file via `fetch`, you
need to serve it over HTTP (file:// doesn't work for WASM modules):

```bash
# Python
python3 -m http.server 8080 --directory dist/counter/

# Node / npx
npx serve dist/counter/
```

Then visit [http://localhost:8080](http://localhost:8080).

## How it works

The counter stage takes `{ count: N }` as input and returns a VNode tree:

```json
{
  "tag": "div",
  "props": { "class": "counter" },
  "children": [
    { "tag": "h1", "children": [{ "$text": "Count: 0" }] },
    { "tag": "div", "props": { "class": "counter-buttons" }, "children": [
      { "tag": "button", "props": { "onClick": { "$event": "decrement" } }, "children": [{ "$text": "-" }] },
      { "tag": "button", "props": { "onClick": { "$event": "increment" } }, "children": [{ "$text": "+" }] },
      { "tag": "button", "props": { "onClick": { "$event": "reset" } }, "children": [{ "$text": "Reset" }] }
    ]}
  ]
}
```

The NoetherRuntime (embedded in `index.html`):
1. Initialises the WASM module
2. Creates an atom: `{ count: 0 }`
3. On each render: calls `execute({ count })` → gets VNode JSON → diffs + patches the DOM
4. On button click: the `$event: "increment"` / `"decrement"` / `"reset"` events call registered
   handlers that mutate the `count` atom and trigger a re-render

The UI event handlers are registered in the generated `index.html`:

```javascript
runtime.defineAtom('count', 0);
runtime.defineEvent('increment', (atoms) => ({ count: atoms.count + 1 }));
runtime.defineEvent('decrement', (atoms) => ({ count: atoms.count - 1 }));
runtime.defineEvent('reset',     (_atoms) => ({ count: 0 }));
```

## Extending

To add more state or interactions:

1. Change the stage's input `Record` type to include additional fields (e.g., `step: Number`)
2. Update the graph to pipe data through multiple stages
3. Run `noether build` again — the browser app regenerates automatically
