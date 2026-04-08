//! `noether build --target browser` — compile a composition graph into a
//! self-contained browser app (HTML + WASM + JS).
//!
//! ## What is produced
//!
//! ```
//! <output_dir>/
//!   index.html          ← the app entry point (open in any browser)
//!   noether_bg.wasm     ← compiled stage graph
//!   noether.js          ← wasm-bindgen JS glue
//! ```
//!
//! ## How it works
//!
//! 1. Type-check the Lagrange graph.
//! 2. Collect all non-stdlib stages from the store.
//! 3. Generate a temporary Rust WASM crate that embeds the graph + bundle.
//! 4. Run `wasm-pack build --target web --release` on the temp crate.
//! 5. Copy the `pkg/` output to the requested output directory.
//! 6. Generate `index.html` with the embedded NoetherRuntime JS.
//!
//! Requires `wasm-pack` in PATH (install with `cargo install wasm-pack`).

use super::build::BuildOptions;
use crate::output::{acli_error, acli_error_hints, acli_ok};
use noether_core::stage::{Stage, StageId};
use noether_core::stdlib::load_stdlib;
use noether_engine::checker::{check_graph, verify_signatures};
use noether_engine::lagrange::{collect_stage_ids, parse_graph};
use noether_store::StageStore;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// Build-time path of this CLI crate. Used to locate the workspace at runtime.
const NOETHER_CLI_DIR: &str = env!("CARGO_MANIFEST_DIR");

pub fn cmd_build_browser(store: &dyn StageStore, opts: BuildOptions<'_>) {
    // ── 1. Parse graph ────────────────────────────────────────────────────────
    let graph_json = match std::fs::read_to_string(opts.graph_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "{}",
                acli_error(&format!("Cannot read '{}': {e}", opts.graph_path))
            );
            std::process::exit(1);
        }
    };
    let graph = match parse_graph(&graph_json) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{}", acli_error(&format!("Invalid graph JSON: {e}")));
            std::process::exit(1);
        }
    };

    // ── 2. Type-check ─────────────────────────────────────────────────────────
    match check_graph(&graph.root, store) {
        Ok(result) => {
            for w in &result.warnings {
                eprintln!("Warning: {w}");
            }
        }
        Err(errors) => {
            let msgs: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
            eprintln!(
                "{}",
                acli_error_hints(&format!("{} type error(s)", msgs.len()), None, Some(msgs),)
            );
            std::process::exit(2);
        }
    }

    // ── 3. Signature pre-flight ───────────────────────────────────────────────
    let stdlib_ids_for_sig: HashSet<StageId> = load_stdlib().into_iter().map(|s| s.id).collect();
    let sig_violations: Vec<_> = verify_signatures(&graph.root, store)
        .into_iter()
        .filter(|v| !stdlib_ids_for_sig.contains(&v.stage_id))
        .collect();
    if !sig_violations.is_empty() {
        let msgs: Vec<String> = sig_violations.iter().map(|v| format!("{v}")).collect();
        eprintln!(
            "{}",
            acli_error_hints(
                &format!("{} signature violation(s)", msgs.len()),
                None,
                Some(msgs),
            )
        );
        std::process::exit(2);
    }

    // ── 4. Collect non-stdlib stages ──────────────────────────────────────────
    let all_ids: Vec<&StageId> = collect_stage_ids(&graph.root);
    let stdlib_ids: HashSet<StageId> = stdlib_ids_for_sig;

    let mut bundle: Vec<Stage> = Vec::new();
    for id in &all_ids {
        if stdlib_ids.contains(*id) {
            continue;
        }
        match store.get(id) {
            Ok(Some(stage)) => bundle.push(stage.clone()),
            Ok(None) => {
                eprintln!(
                    "{}",
                    acli_error(&format!("Stage '{}' not found in store", id.0))
                );
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("{}", acli_error(&format!("Store error loading stage: {e}")));
                std::process::exit(1);
            }
        }
    }

    // ── 5. App metadata ───────────────────────────────────────────────────────
    let output_path = Path::new(opts.output_path);
    let app_name = opts
        .app_name
        .map(String::from)
        .or_else(|| {
            output_path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "noether-app".to_string());

    let description = opts
        .description
        .map(String::from)
        .unwrap_or_else(|| graph.description.clone());

    let app_version = env!("CARGO_PKG_VERSION");

    // ── 6. Workspace-relative crate paths ─────────────────────────────────────
    let workspace_root = std::path::PathBuf::from(NOETHER_CLI_DIR)
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(NOETHER_CLI_DIR).join("../.."));

    let core_path = workspace_root
        .join("crates/noether-core")
        .display()
        .to_string();
    let store_path = workspace_root
        .join("crates/noether-store")
        .display()
        .to_string();
    let engine_path = workspace_root
        .join("crates/noether-engine")
        .display()
        .to_string();

    // ── 7. Write temporary wasm-bindgen project ────────────────────────────────
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let build_dir = std::env::temp_dir().join(format!("noether-browser-build-{ts}"));
    let src_dir = build_dir.join("src");

    if let Err(e) = std::fs::create_dir_all(&src_dir) {
        eprintln!(
            "{}",
            acli_error(&format!("Failed to create build dir: {e}"))
        );
        std::process::exit(1);
    }

    let write_file = |rel: &std::path::Path, contents: &str| {
        if let Err(e) = std::fs::write(rel, contents) {
            eprintln!(
                "{}",
                acli_error(&format!("Failed to write {}: {e}", rel.display()))
            );
            std::process::exit(1);
        }
    };

    write_file(&src_dir.join("graph.json"), &graph_json);
    write_file(
        &src_dir.join("bundle.json"),
        &serde_json::to_string_pretty(&bundle).unwrap_or_else(|_| "[]".into()),
    );
    write_file(
        &build_dir.join("Cargo.toml"),
        &generate_wasm_cargo_toml(
            "noether-browser-stage",
            app_version,
            &core_path,
            &store_path,
            &engine_path,
        ),
    );
    write_file(&src_dir.join("lib.rs"), &generate_wasm_lib_rs(&bundle));

    // ── 8. wasm-pack build ────────────────────────────────────────────────────
    eprintln!(
        "Building {} (browser target, may take a minute on first run)…",
        app_name
    );

    let wasm_pack = which_wasm_pack();
    let wasm_status = Command::new(&wasm_pack)
        .args(["build", "--target", "web", "--release"])
        .current_dir(&build_dir)
        .status();

    match wasm_status {
        Err(e) => {
            eprintln!(
                "{}",
                acli_error(&format!(
                    "Failed to invoke '{}': {e}\nInstall wasm-pack with: cargo install wasm-pack",
                    wasm_pack
                ))
            );
            let _ = std::fs::remove_dir_all(&build_dir);
            std::process::exit(1);
        }
        Ok(s) if !s.success() => {
            eprintln!(
                "{}",
                acli_error("wasm-pack build failed — see compiler output above")
            );
            eprintln!("Build directory preserved: {}", build_dir.display());
            std::process::exit(1);
        }
        Ok(_) => {}
    }

    // ── 9. Copy artifacts to output directory ─────────────────────────────────
    if let Err(e) = std::fs::create_dir_all(output_path) {
        eprintln!(
            "{}",
            acli_error(&format!(
                "Failed to create output dir '{}': {e}",
                output_path.display()
            ))
        );
        std::process::exit(1);
    }

    let pkg_dir = build_dir.join("pkg");
    let mut wasm_filename = String::from("noether_bg.wasm");
    let mut js_filename = String::from("noether.js");

    for entry in std::fs::read_dir(&pkg_dir).unwrap_or_else(|_| {
        eprintln!(
            "{}",
            acli_error("wasm-pack did not produce a pkg/ directory")
        );
        std::process::exit(1);
    }) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with("_bg.wasm") {
            let _ = std::fs::copy(entry.path(), output_path.join("noether_bg.wasm"));
            wasm_filename = "noether_bg.wasm".into();
        } else if name.ends_with(".js") && !name.ends_with("_bg.js") && !name.contains("snippets") {
            let _ = std::fs::copy(entry.path(), output_path.join("noether.js"));
            js_filename = "noether.js".into();
        }
    }

    // ── 10. Generate index.html ────────────────────────────────────────────────
    let html = generate_index_html(
        &app_name,
        &description,
        app_version,
        &wasm_filename,
        &js_filename,
        &graph_json,
        &bundle,
    );
    write_file(&output_path.join("index.html"), &html);

    let _ = std::fs::remove_dir_all(&build_dir);

    println!(
        "{}",
        acli_ok(serde_json::json!({
            "output_dir": output_path.display().to_string(),
            "app_name": app_name,
            "version": app_version,
            "files": ["index.html", wasm_filename, js_filename],
            "stages": {
                "bundled": bundle.len(),
                "stdlib": all_ids.len() - bundle.len(),
                "total": all_ids.len(),
            },
            "description": description,
        }))
    );
}

// ── Helper: locate wasm-pack ──────────────────────────────────────────────────

fn which_wasm_pack() -> String {
    // Common installation locations.
    for candidate in &[
        "wasm-pack",
        "/home/alpibru/.cargo/bin/wasm-pack",
        "/usr/local/bin/wasm-pack",
        "/usr/bin/wasm-pack",
    ] {
        if Command::new(candidate).arg("--version").output().is_ok() {
            return candidate.to_string();
        }
    }
    // Try ~/.cargo/bin via $HOME
    if let Ok(home) = std::env::var("HOME") {
        let path = format!("{home}/.cargo/bin/wasm-pack");
        if Command::new(&path).arg("--version").output().is_ok() {
            return path;
        }
    }
    "wasm-pack".into()
}

// ── Code generation ───────────────────────────────────────────────────────────

fn generate_wasm_cargo_toml(
    name: &str,
    version: &str,
    core: &str,
    store: &str,
    engine: &str,
) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "{version}"
edition = "2021"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
noether-core   = {{ path = "{core}" }}
noether-store  = {{ path = "{store}" }}
noether-engine = {{ path = "{engine}", default-features = false }}
wasm-bindgen   = "0.2"
serde_json     = "1"

[target.'cfg(target_arch = "wasm32")'.dependencies]
getrandom = {{ version = "0.2", features = ["js"] }}
"#
    )
}

fn generate_wasm_lib_rs(bundle: &[Stage]) -> String {
    // Collect Rust stages that have inline implementation code.
    // These get compiled directly into the WASM binary via a generated dispatch function.
    let mut rust_stage_fns = String::new();
    let mut dispatch_arms = String::new();

    for stage in bundle {
        if stage.implementation_language.as_deref() == Some("rust") {
            if let Some(code) = &stage.implementation_code {
                // Sanitise stage ID to a valid Rust identifier.
                let fn_name = format!("stage_{}", &stage.id.0[..16].replace('-', "_"));
                // The implementation code defines `fn execute(input: &Value) -> Value { ... }`.
                // We wrap it in an outer function, nesting execute inside, then call it.
                let indented_code = code
                    .lines()
                    .map(|l| format!("    {l}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                rust_stage_fns.push_str(&format!(
                    "\n// Stage: {desc}\n#[allow(dead_code, unused_imports)]\nfn {fn_name}(input: &serde_json::Value) -> Result<serde_json::Value, noether_engine::executor::ExecutionError> {{\n{code}\n    Ok(execute(input))\n}}\n",
                    desc = stage.description,
                    fn_name = fn_name,
                    code = indented_code,
                ));
                dispatch_arms.push_str(&format!(
                    "            \"{id}\" => {fn_name}(input),\n",
                    id = stage.id.0,
                    fn_name = fn_name,
                ));
            }
        }
    }

    // Build the custom executor code only if there are Rust stages to dispatch.
    let custom_executor_code = if dispatch_arms.is_empty() {
        // No custom Rust stages — use InlineExecutor directly.
        String::from(
            r#"
fn make_executor(store: &MemoryStore) -> impl noether_engine::executor::StageExecutor + '_ {
    InlineExecutor::from_store(store)
}
"#,
        )
    } else {
        format!(
            r#"
struct WasmExecutor {{
    inline: InlineExecutor,
}}

impl noether_engine::executor::StageExecutor for WasmExecutor {{
    fn execute(
        &self,
        stage_id: &noether_core::stage::StageId,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value, noether_engine::executor::ExecutionError> {{
        match stage_id.0.as_str() {{
{dispatch_arms}            _ => self.inline.execute(stage_id, input),
        }}
    }}
}}

fn make_executor(store: &MemoryStore) -> WasmExecutor {{
    WasmExecutor {{ inline: InlineExecutor::from_store(store) }}
}}
"#,
            dispatch_arms = dispatch_arms,
        )
    };

    format!(
        r#"// Auto-generated by `noether build --target browser`. Do not edit.
use noether_core::stage::Stage;
use noether_core::stdlib::load_stdlib;
use noether_engine::executor::inline::InlineExecutor;
use noether_engine::executor::runner::run_composition;
use noether_engine::lagrange::parse_graph;
use noether_store::{{MemoryStore, StageStore}};
use wasm_bindgen::prelude::*;
use std::sync::OnceLock;

const GRAPH_JSON: &str = include_str!("graph.json");
const BUNDLE_JSON: &str = include_str!("bundle.json");

// ── Custom Rust stage implementations ────────────────────────────────────────
{rust_stage_fns}
// ── Executor ──────────────────────────────────────────────────────────────────
{custom_executor_code}

// ── Lazy-initialised store (bootstrap runs only once per page load) ────────────
static STORE: OnceLock<MemoryStore> = OnceLock::new();

fn get_store() -> &'static MemoryStore {{
    STORE.get_or_init(|| {{
        let mut store = MemoryStore::new();
        for stage in load_stdlib() {{
            store.put(stage).ok();
        }}
        if let Ok(bundle) = serde_json::from_str::<Vec<Stage>>(BUNDLE_JSON) {{
            for stage in bundle {{
                store.put(stage).ok();
            }}
        }}
        store
    }})
}}

/// Execute the embedded composition graph with the given JSON input.
/// Returns JSON-encoded output on success, or a JSON error object on failure.
#[wasm_bindgen]
pub fn execute(input_json: &str) -> String {{
    let input: serde_json::Value = match serde_json::from_str(input_json) {{
        Ok(v) => v,
        Err(e) => {{
            return serde_json::json!({{"ok": false, "error": e.to_string()}}).to_string();
        }}
    }};

    let store = get_store();
    let graph = match parse_graph(GRAPH_JSON) {{
        Ok(g) => g,
        Err(e) => {{
            return serde_json::json!({{"ok": false, "error": e.to_string()}}).to_string();
        }}
    }};

    let executor = make_executor(store);
    match run_composition(&graph.root, &input, &executor, "browser") {{
        Ok(result) => serde_json::json!({{"ok": true, "output": result.output}}).to_string(),
        Err(e) => serde_json::json!({{"ok": false, "error": e.to_string()}}).to_string(),
    }}
}}

/// Execute a single stage by ID with the given JSON input.
/// Used by the JS graph executor in `NoetherRuntime` to run local stages
/// while the JS side handles RemoteStage nodes via `fetch()`.
/// Returns `{{"ok": bool, "output": ...}}`.
#[wasm_bindgen]
pub fn execute_stage(stage_id: &str, input_json: &str) -> String {{
    use noether_engine::executor::StageExecutor;
    let input: serde_json::Value = match serde_json::from_str(input_json) {{
        Ok(v) => v,
        Err(e) => {{
            return serde_json::json!({{"ok": false, "error": e.to_string()}}).to_string();
        }}
    }};

    let store = get_store();
    let executor = make_executor(store);
    let id = noether_core::stage::StageId(stage_id.to_string());
    match executor.execute(&id, &input) {{
        Ok(output) => serde_json::json!({{"ok": true, "output": output}}).to_string(),
        Err(e) => serde_json::json!({{"ok": false, "error": format!("{{}}", e)}}).to_string(),
    }}
}}

/// Return the full graph JSON string so the JS runtime can traverse all nodes
/// (including `RemoteStage`) and orchestrate execution.
#[wasm_bindgen]
pub fn get_graph_json() -> String {{
    GRAPH_JSON.to_string()
}}

/// Return graph metadata as JSON (description).
#[wasm_bindgen]
pub fn get_graph_info() -> String {{
    match parse_graph(GRAPH_JSON) {{
        Ok(g) => serde_json::json!({{
            "description": g.description,
        }}).to_string(),
        Err(e) => serde_json::json!({{"error": e.to_string()}}).to_string(),
    }}
}}
"#,
        rust_stage_fns = rust_stage_fns,
        custom_executor_code = custom_executor_code,
    )
}

// ── CSS scoping ────────────────────────────────────────────────────────────────

/// Prepend `prefix` to every top-level CSS selector in `css`.
///
/// Uses a simple state machine — no full CSS parser needed:
/// - Tracks brace depth; rules at depth 0 are top-level.
/// - Splits on `{` / `}` to locate selector tokens.
/// - Handles comma-separated selector lists (e.g. `h1, h2 { ... }`).
/// - At-rules (@media, @keyframes, etc.) are emitted verbatim (not scoped,
///   since their inner rules inherit the outer scope via DOM cascade).
///
/// ### Example
/// ```
/// scope_css(".card { color: red; }", ".nr-abc123")
/// // → ".nr-abc123 .card { color: red; }"
/// ```
fn scope_css(css: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(css.len() + 64);
    let mut depth: usize = 0;
    let mut current_block = String::new();

    let chars: Vec<char> = css.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];

        if ch == '{' {
            if depth == 0 {
                // `current_block` holds the selector list for this rule.
                let selector = current_block.trim();
                if selector.starts_with('@') {
                    // At-rules: pass through verbatim without scoping.
                    out.push_str(selector);
                } else {
                    // Scope each comma-separated selector.
                    let scoped = selector
                        .split(',')
                        .map(|sel| {
                            let s = sel.trim();
                            if s.is_empty() {
                                String::new()
                            } else {
                                format!("{prefix} {s}")
                            }
                        })
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(", ");
                    out.push_str(&scoped);
                }
                current_block.clear();
            }
            out.push('{');
            depth += 1;
        } else if ch == '}' {
            out.push('}');
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            current_block.push(ch);
        } else {
            out.push(ch);
        }

        i += 1;
    }

    out
}

/// Generate the app's index.html with the NoetherRuntime embedded.
/// The runtime JS is inlined — no external dependencies, no CDN.
pub fn generate_index_html(
    app_name: &str,
    description: &str,
    version: &str,
    wasm_file: &str,
    js_file: &str,
    graph_json: &str,
    bundle: &[noether_core::stage::Stage],
) -> String {
    let runtime_js = NOETHER_RUNTIME_JS;

    // ── Collect stage-scoped CSS from the bundle ───────────────────────────────
    let stage_styles: String = bundle
        .iter()
        .filter_map(|stage| {
            stage.ui_style.as_deref().map(|css| {
                let prefix = format!(".nr-{}", &stage.id.0[..8.min(stage.id.0.len())]);
                scope_css(css, &prefix)
            })
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Extract optional UI section from graph JSON.
    let ui = serde_json::from_str::<serde_json::Value>(graph_json)
        .ok()
        .and_then(|v| v.get("ui").cloned());

    let user_style = ui
        .as_ref()
        .and_then(|u| u.get("style"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();

    // Generate atom definitions: runtime.defineAtom("name", initialValue);
    let atom_js = ui
        .as_ref()
        .and_then(|u| u.get("atoms"))
        .and_then(|a| a.as_object())
        .map(|atoms| {
            atoms
                .iter()
                .map(|(k, v)| {
                    format!(
                        "  runtime.defineAtom({}, {});",
                        serde_json::to_string(k).unwrap_or_default(),
                        serde_json::to_string(v).unwrap_or("null".into()),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    // Generate event registrations: runtime.defineEvent("name", handler);
    // Event handlers are stored as raw JS expression strings in the graph JSON.
    let event_js = ui
        .as_ref()
        .and_then(|u| u.get("events"))
        .and_then(|e| e.as_object())
        .map(|events| {
            events
                .iter()
                .map(|(k, v)| {
                    // Value is a JS function expression string, e.g. "atoms => ({ count: atoms.count + 1 })"
                    let handler = v.as_str().unwrap_or("() => ({})");
                    format!(
                        "  runtime.defineEvent({}, {});",
                        serde_json::to_string(k).unwrap_or_default(),
                        handler,
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{app_name}</title>
<style>
  :root {{
    --bg: #0a0d0f; --surface: #111518; --edge: #1c2127; --text: #e8ecf0;
    --dim: #7a8896; --accent: #4af4a8; --warn: #f4c84a; --bad: #f46a4a;
    --mono: 'Geist Mono', 'JetBrains Mono', 'Fira Code', monospace;
  }}
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  body {{ background: var(--bg); color: var(--text); font-family: var(--mono); font-size: 14px; line-height: 1.5; }}
  #noether-root {{ min-height: 100vh; }}
  .nr-loading {{ display: flex; align-items: center; justify-content: center; height: 100vh; color: var(--dim); font-size: 13px; }}
  .nr-error {{ padding: 24px; color: var(--bad); }}
  .nr-error pre {{ margin-top: 8px; font-size: 12px; opacity: 0.7; }}
  {stage_styles}
  {user_style}
</style>
</head>
<body>
<div id="noether-root"><div class="nr-loading">Loading {app_name}…</div></div>
<script type="module">
import init, {{ execute, execute_stage, get_graph_json, get_graph_info }} from './{js_file}';

{runtime_js}

(async () => {{
  try {{
    await init('./{wasm_file}');

    const info = JSON.parse(get_graph_info());
    document.title = info.description || '{app_name}';

    const mountEl = document.getElementById('noether-root');
    // Pass execute (legacy full-graph), execute_stage (per-stage), and get_graph_json
    // to the runtime. The runtime uses execute_stage + get_graph_json to drive
    // the JS graph executor, enabling RemoteStage nodes to be handled via fetch().
    const runtime = new NoetherRuntime(execute, mountEl, execute_stage, get_graph_json);

    // ── Atoms ──
{atom_js}

    // ── Events ──
{event_js}

    window._noether = runtime;
    await runtime.render();
  }} catch (err) {{
    const root = document.getElementById('noether-root');
    root.innerHTML = `<div class="nr-error"><strong>Noether Error</strong><pre>${{err}}</pre></div>`;
    console.error('Noether init error:', err);
  }}
}})();
</script>
<!-- noether {version} · {description} -->
</body>
</html>"#,
        app_name = app_name,
        description = description,
        version = version,
        wasm_file = wasm_file,
        js_file = js_file,
        runtime_js = runtime_js,
        user_style = user_style,
        stage_styles = stage_styles,
        atom_js = atom_js,
        event_js = event_js,
    )
}

/// The NoetherRuntime JavaScript (embedded at build time).
/// Defined as a const so it can be inlined into the generated HTML.
pub const NOETHER_RUNTIME_JS: &str = include_str!("../noether_runtime.js");
