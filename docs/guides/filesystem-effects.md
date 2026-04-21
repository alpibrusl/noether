# Filesystem-scoped Effects

`Effect::FsRead(path)` and `Effect::FsWrite(path)` let a stage declare the specific host paths it reads from or writes to. `IsolationPolicy::from_effects` scans for them and generates the matching bind mounts automatically — no caller plumbing.

Before these variants landed, the `EffectSet` vocabulary had nothing between "the stage doesn't touch the filesystem at all" and "the stage is `Process`-effectful, good luck". Consumers with a richer trust model (agentspec's `filesystem: scoped`, agent-coding runtimes that operate on a specific project directory) had to bypass the effect surface and build `IsolationPolicy` by hand. These variants close that gap.

## Declaring the effects

In a stage spec:

```json
{
  "name": "read_config_file",
  "signature": {
    "input":  "Null",
    "output": { "Record": { "config": "Text" } },
    "effects": [
      { "effect": "FsRead", "path": "/etc/agent/config.toml" }
    ]
  },
  …
}
```

or in Rust:

```rust
use noether_core::effects::{Effect, EffectSet};
use std::path::PathBuf;

let effects = EffectSet::new([
    Effect::FsRead  { path: PathBuf::from("/etc/agent/config.toml") },
    Effect::FsWrite { path: PathBuf::from("/tmp/agent-output") },
]);
```

Each effect carries an absolute host path. Use separate entries for each path — the `EffectSet` is a `BTreeSet<Effect>` and distinct paths are distinct elements.

## How it drives the policy

`IsolationPolicy::from_effects` walks the set and emits binds:

| Effect | Bind emitted |
|---|---|
| `Effect::FsRead { path: p }` | `RoBind { host: p, sandbox: p }` |
| `Effect::FsWrite { path: p }` | `RwBind { host: p, sandbox: p }` |

Paths appear at the same location inside the sandbox — the convention is 1:1 mapping so stage code doesn't need to know it's running under bwrap.

`/nix/store` stays unconditionally bound read-only regardless of declared effects. Mount order `rw → ro → work_host` from [sandbox-isolation](./sandbox-isolation.md#caller-managed-filesystem-trust) is preserved, so a narrower RO effect can shadow a broader RW parent:

```rust
let effects = EffectSet::new([
    Effect::FsWrite { path: PathBuf::from("/home/user/project") },
    Effect::FsRead  { path: PathBuf::from("/home/user/project/.ssh") },
]);
```

The sandbox emits `--bind /home/user/project /home/user/project` then `--ro-bind /home/user/project/.ssh /home/user/project/.ssh`. bwrap's later-wins-on-overlap rule keeps `.ssh` read-only inside a writable project dir.

## What stays caller-managed

`from_effects` doesn't automatically populate `work_host` — the scratch dir default is still `None` → sandbox-private tmpfs. Callers who need host-visible scratch opt in via `IsolationPolicy::with_work_host(path)`. That decision lives with the caller, not the effect vocabulary.

## The trust framing

The `FsRead` / `FsWrite` rustdoc spells out what the pattern around #39 made explicit for `RwBind`: **the crate cannot validate whether a declared path is sensible to share**. `FsWrite(/home/user)` is syntactically identical to `FsWrite(/tmp/project-output)` — both produce an RW bind. One is a terrible idea; one is the whole point of an agent-coding tool. The policy decision lives with the stage author.

What the effect system *does* give you is a structured trust surface:

- `EffectPolicy::restrict([EffectKind::FsRead])` — allow reads but no writes (via `--allow-effects fs-read`).
- `EffectPolicy::restrict([EffectKind::FsRead, EffectKind::FsWrite])` — allow both.
- No `--allow-effects` flag including `fs-write` — a stage declaring `FsWrite` is rejected at preflight, before any subprocess spawns.

Combined with the sandbox, that's enough for a caller to say "I'll delegate to `noether-sandbox` only for stages whose effect surface stays within these kinds" without having to read every stage's implementation.

## Relationship to `rw_binds` on `IsolationPolicy`

Two layers:

- **Effect surface** (`Effect::FsRead` / `FsWrite`) — declared in the stage signature, part of the content hash, visible to policy preflight.
- **Policy surface** (`RoBind` / `RwBind` on `IsolationPolicy`) — the concrete mount instructions handed to bwrap.

`from_effects` bridges them. Callers who need host paths that aren't declared in any stage's effects (e.g. adding a shared cache dir across multiple runs) can still extend the policy directly via `policy.rw_binds.push(...)` — the effect vocabulary drives the default, the policy API lets you go further if you know what you're doing.

## When NOT to use these variants

- **One-off scratch data inside `/work`.** That's what the sandbox-private tmpfs is for. Declaring `FsWrite(/work)` would be redundant (the policy's `--dir /work` already creates the writable tmpfs).
- **Dynamic paths computed at run time.** `Effect::FsWrite(path)` is a signature-level claim — `path` is fixed when the stage is authored. A stage that writes to "wherever the user configures" needs a different surface; `Effect::FsWrite(/)` is not the right answer.
- **Secret material.** If the stage needs access to `~/.ssh/id_rsa`, declare it and accept the trust widening, but probably pass the key as input data instead of binding the file. The effect surface is for what the stage *needs* at the filesystem layer; data should flow through the typed input.

## See also

- [guides/sandbox-isolation](./sandbox-isolation.md) — how `rw_binds` / `ro_binds` / `work_host` render to bwrap argv
- [architecture/type-system](../architecture/type-system.md) — where effects fit in the signature
- [agents/express-a-property](../agents/express-a-property.md) — the dense agent version of this narrative
