# Examples

All examples live in [`examples/`](https://github.com/alpibrusl/noether/tree/main/examples) in the repository.

## Stage Explorer

**[`examples/stage-explorer/`](https://github.com/alpibrusl/noether/tree/main/examples/stage-explorer)**

A self-contained browser app that lets you search and browse all 77 Noether stdlib stages. Demonstrates `--target browser` (WASM), the `NoetherRuntime` reactive UI runtime, and embedded catalog search — no backend required.

```bash
cd examples/stage-explorer/ui
noether build --target browser graph.json --output /tmp/stage-explorer
cd /tmp/stage-explorer && python3 -m http.server 3000
# Open http://localhost:3000
```

**What it shows:**

- Rust stage that embeds a JSON catalog and renders a VNode tree
- Keyword search over stage descriptions, input/output types
- Hint chips as `onclick` event bindings
- Live text filtering via `oninput`

---

## Todo App

**[`examples/todo/`](https://github.com/alpibrusl/noether/tree/main/examples/todo)**

A minimal browser to-do list app built with `--target browser`. Shows reactive atoms (`items`, `draft`), `add`/`toggle`/`clear` events, and how Rust stages render conditional UI.

```bash
cd examples/todo/ui
noether build --target browser graph.json --output /tmp/todo-app
cd /tmp/todo-app && python3 -m http.server 3001
```

---

## Counter

**[`examples/counter/`](https://github.com/alpibrusl/noether/tree/main/examples/counter)**

The simplest possible browser app — a single counter with `increment`/`decrement`/`reset` events. Good starting point for understanding the atom → stage → VNode → DOM loop.

```bash
cd examples/counter/ui
noether build --target browser graph.json --output /tmp/counter
cd /tmp/counter && python3 -m http.server 3002
```

---

## Building your own

1. Write a Rust stage that accepts `{ query: Text, ... }` and returns a `VNode` (use `serde_json::json!`).
2. Create a `graph.json` with `"root": { "op": "Stage", "id": "<hash>" }` and a `"ui"` block defining atoms and events.
3. Run `noether build --target browser graph.json --output ./dist`.
4. Serve `./dist` statically.

See [CLI Reference → noether build](../cli/commands.md#noether-build) and [Guides → Building Custom Stages](../guides/custom-stages.md).
