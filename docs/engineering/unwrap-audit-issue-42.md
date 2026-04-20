# `unwrap()` / `expect()` audit — issue #42

Scope: `crates/noether-engine/src/executor/runner.rs`,
`crates/noether-engine/src/executor/nix.rs`, and
`crates/noether-engine/src/index/**`.

Each row is a single call site in the in-scope files. Test-module
unwraps (`#[cfg(test)]`) are not listed — `clippy::unwrap_used` is
suppressed for test code via a module-level `#![cfg_attr(test,
allow(clippy::unwrap_used))]`, which is considered acceptable for
assertion-style tests.

| File | Line (approx) | Call | Classification | Notes |
|---|---|---|---|---|
| `executor/runner.rs` | ~120 | `.find(...).unwrap_or(&"items")` | Safe (not unwrap) | `unwrap_or` — infallible fallback. |
| `executor/runner.rs` | ~152 | `obj.get(name).cloned().unwrap_or_else(...)` | Safe (not unwrap) | `unwrap_or_else` — infallible fallback. |
| `executor/runner.rs` | ~190 | `h.join().expect("parallel branch panicked")` | **Converted** | Now match on `h.join()` and emit `ExecutionError::StageFailed` with a `parallel:<name>` synthetic stage id when a branch thread panics. |
| `executor/runner.rs` | ~235 | `obj.get(...).cloned().unwrap_or(Value::Null)` | Safe (not unwrap) | `unwrap_or` — infallible fallback. |
| `executor/runner.rs` | ~265 | `last_err.unwrap_or(ExecutionError::RetryExhausted { .. })` | Safe (not unwrap) | `unwrap_or` — infallible fallback. |
| `executor/runner.rs` | ~300 | `h.join().expect("Let binding panicked")` | **Converted** | Same treatment as the Parallel branch — `let:<name>` synthetic stage id. |
| `executor/runner.rs` | ~382 | `serde_json::to_vec(value).unwrap_or_default()` | Safe (not unwrap) | `unwrap_or_default` — `serde_json::to_vec` on an in-memory `Value` is effectively total; an empty-bytes hash on a hypothetical failure is acceptable for a trace-only hash. |
| `executor/runner.rs` | ~424 | `.unwrap_or("remote reported ok=false without error message")` | Safe (not unwrap) | `unwrap_or` — infallible fallback. |
| `executor/nix.rs` | ~160 | `std::env::var_os("PATH")?` | Safe (not unwrap) | Returns `Option` via `?`. |
| `executor/nix.rs` | ~182 | `std::env::var("HOME").unwrap_or_else(...)` | Safe (not unwrap) | `unwrap_or_else` — falls back to `/tmp`. |
| `executor/nix.rs` | ~341 | `serde_json::to_string(input).unwrap_or_default()` | Safe (not unwrap) | `unwrap_or_default` — stdin passes an empty string on the (practically impossible) serialise failure; the child then reports a parse error. |
| `executor/nix.rs` | ~355 | `script.to_str().unwrap_or("/dev/null")` | Safe (not unwrap) | `unwrap_or` — invalid-UTF-8 script paths fall through and fail cleanly at `Command::new`. |
| `executor/nix.rs` | ~567 | `script.to_str().unwrap_or("/dev/null")` | Safe (not unwrap) | Same as above. |
| `executor/nix.rs` | ~637 | `trimmed.strip_prefix("# requires:").unwrap().trim()` | **Converted** | Rewritten with `let Some(reqs_raw) = trimmed.strip_prefix(...)` so the unwrap disappears altogether. |
| `index/cache.rs` | ~184 | `.expect("just inserted")` | **Converted** | Replaced with a `match`ed lookup that returns `EmbeddingError::Provider`. A companion length check on `embed_batch` responses catches the real failure mode — a short-read from a misbehaving remote provider — before it gets to this call site. |
| `index/cache.rs` | ~90 | `flush()` in `Drop` | Safe (not unwrap) | `Drop` impl does not unwrap — it ignores `std::fs::write` failures on purpose; noisy drop-time errors would worsen, not improve, the crash path. |
| `index/embedding.rs` | — | (none in production code) | — | Only test-module unwraps. |
| `index/search.rs` | — | (none in production code) | — | Only test-module unwraps. |
| `index/text.rs` | — | (none in production code) | — | No unwraps. |
| `index/mod.rs` | — | (none in production code) | — | Only test-module unwraps; floating-point `partial_cmp(...).unwrap_or(Equal)` pattern used in sort comparators is `unwrap_or`, not `unwrap`. |

## Modules with `#![warn(clippy::unwrap_used)]` applied

All in-scope modules were cleaned end-to-end and now carry the warn
attribute at module scope:

- `executor/runner.rs`
- `executor/nix.rs`
- `index/mod.rs`
- `index/cache.rs`
- `index/embedding.rs`
- `index/search.rs`
- `index/text.rs`

## Out-of-scope observations

A broader audit (outside issue #42) would want to look at:

- `executor/runtime.rs`, `executor/budget.rs`, `executor/stages/*` —
  several `unwrap()` sites tied to stage-stdlib helpers. These run in
  the same CLI `noether run` code path but were not in the issue scope.
- `planner.rs` / `checker.rs` — a handful of invariant-enforced
  `expect` calls on graph traversal. Panics here imply a malformed
  post-resolver graph; worth their own follow-up.
- `noether-cli` / `noether-grid-*` / `noether-scheduler` — explicitly
  out-of-scope per the issue. Their panics need a separate tracking
  issue.
