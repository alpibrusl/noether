//! `noether build` — compile a composition graph into a self-contained binary.
//!
//! The generated binary:
//!   - Has the Lagrange graph baked in via `include_str!`
//!   - Has all non-stdlib stages serialised into a bundle (also `include_str!`)
//!   - Accepts `--input <JSON>`, `--dry-run`, `--version`, `--help`
//!   - Emits ACLI-shaped JSON on stdout
//!   - Auto-detects LLM provider from env vars at runtime (VERTEX_AI_TOKEN etc.)
//!
//! Requires `cargo` to be in `PATH` at build time. The generated project uses
//! *path* dependencies pointing to the Noether workspace that was used to
//! compile this `noether` binary.

use noether_core::stage::{Stage, StageId};
use noether_core::stdlib::load_stdlib;
use noether_engine::checker::{check_graph, verify_signatures};
use noether_engine::lagrange::{collect_stage_ids, parse_graph, resolve_pinning};
use noether_store::StageStore;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use crate::output::{acli_error, acli_error_hints, acli_ok};

/// Build-time path of this CLI crate. Used to locate the workspace at runtime.
const NOETHER_CLI_DIR: &str = env!("CARGO_MANIFEST_DIR");

pub struct BuildOptions<'a> {
    /// Path to the Lagrange graph JSON file.
    pub graph_path: &'a str,
    /// Destination path for the compiled binary (native) or output directory (browser).
    pub output_path: &'a str,
    /// Override the binary / ACLI command name. Defaults to the output filename.
    pub app_name: Option<&'a str>,
    /// One-line description surfaced in `--help`. Defaults to the graph description.
    pub description: Option<&'a str>,
    /// Build target: "native" (default), "browser", or "react-native".
    pub target: &'a str,
    /// After building (native only): immediately exec the binary with `--serve <addr>`.
    pub serve_addr: Option<&'a str>,
}

pub fn cmd_build(store: &dyn StageStore, opts: BuildOptions<'_>) {
    if opts.target == "browser" {
        super::build_browser::cmd_build_browser(store, opts);
        return;
    }
    if opts.target == "react-native" {
        super::build_mobile::cmd_build_mobile(store, opts);
        return;
    }
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
    let mut graph = match parse_graph(&graph_json) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{}", acli_error(&format!("Invalid graph JSON: {e}")));
            std::process::exit(1);
        }
    };

    // ── 1a. Resolve pinning ──────────────────────────────────────────────────
    // Rewrite signature-pinned refs to concrete implementation IDs so every
    // downstream pass (type-check, signature verify, planner, cost map) sees
    // impl hashes and can use `store.get` directly. Without this, a graph
    // that declares `pinning: "signature"` would type-check but fail at
    // subsequent passes. Same logic as `noether run`.
    let resolution = match resolve_pinning(&mut graph.root, store) {
        Ok(rep) => rep,
        Err(e) => {
            eprintln!("{}", acli_error(&format!("Pinning resolution: {e}")));
            std::process::exit(1);
        }
    };
    for rw in &resolution.rewrites {
        eprintln!(
            "Info: {:?}-pinned stage {} resolved to {}",
            rw.pinning,
            &rw.before[..8.min(rw.before.len())],
            &rw.after[..8.min(rw.after.len())]
        );
    }
    for w in &resolution.warnings {
        eprintln!(
            "Warning: signature {} has {} Active implementations — deterministically picked {}",
            &w.signature_id[..8.min(w.signature_id.len())],
            w.active_implementation_ids.len(),
            &w.chosen[..8.min(w.chosen.len())],
        );
    }

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
    // Only check non-stdlib stages: stdlib stages are trusted via load_stdlib()
    // and may have been stored before signatures were introduced.
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

    // ── 4. Collect stage IDs and split stdlib vs. custom ─────────────────────
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

    // ── 5. Resolve binary name and description ────────────────────────────────
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

    // Cargo package names: [a-zA-Z0-9_-]
    let package_name: String = app_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

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

    // ── 7. Write temporary Cargo project ─────────────────────────────────────
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let build_dir = std::env::temp_dir().join(format!("noether-build-{ts}"));
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
        &generate_cargo_toml(
            &package_name,
            app_version,
            &core_path,
            &store_path,
            &engine_path,
        ),
    );
    write_file(
        &src_dir.join("main.rs"),
        &generate_main_rs(&app_name, app_version, &description),
    );

    // ── 8. cargo build --release ──────────────────────────────────────────────
    eprintln!(
        "Building {} (first run may take a minute while Cargo compiles)…",
        app_name
    );
    let cargo_status = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&build_dir)
        .status();

    match cargo_status {
        Err(e) => {
            eprintln!("{}", acli_error(&format!("Failed to invoke cargo: {e}")));
            let _ = std::fs::remove_dir_all(&build_dir);
            std::process::exit(1);
        }
        Ok(s) if !s.success() => {
            eprintln!(
                "{}",
                acli_error("cargo build failed — see compiler errors above")
            );
            eprintln!(
                "Build directory preserved for inspection: {}",
                build_dir.display()
            );
            std::process::exit(1);
        }
        Ok(_) => {}
    }

    // ── 9. Install binary at requested path ───────────────────────────────────
    let built_binary = build_dir.join("target/release").join(&package_name);

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }

    if let Err(e) = std::fs::copy(&built_binary, output_path) {
        eprintln!(
            "{}",
            acli_error(&format!(
                "Failed to install binary at '{}': {e}",
                output_path.display()
            ))
        );
        std::process::exit(1);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(output_path, std::fs::Permissions::from_mode(0o755));
    }

    let _ = std::fs::remove_dir_all(&build_dir);

    println!(
        "{}",
        acli_ok(serde_json::json!({
            "binary": output_path.display().to_string(),
            "app_name": app_name,
            "version": app_version,
            "stages": {
                "bundled": bundle.len(),
                "stdlib": all_ids.len() - bundle.len(),
                "total": all_ids.len(),
            },
            "description": description,
        }))
    );

    // If --serve was requested, immediately exec the binary as an HTTP server.
    // On Unix this replaces the current process image (exec syscall).
    // On Windows we fall back to spawning a child and blocking on it.
    if let Some(addr) = opts.serve_addr {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = Command::new(output_path).arg("--serve").arg(addr).exec();
            // exec() only returns on error
            eprintln!("{}", acli_error(&format!("Failed to exec server: {err}")));
            std::process::exit(1);
        }
        #[cfg(not(unix))]
        {
            let status = Command::new(output_path)
                .arg("--serve")
                .arg(addr)
                .status()
                .unwrap_or_else(|e| {
                    eprintln!("{}", acli_error(&format!("Failed to start server: {e}")));
                    std::process::exit(1);
                });
            std::process::exit(status.code().unwrap_or(0));
        }
    }
}

// ── Code generation ───────────────────────────────────────────────────────────

fn generate_cargo_toml(name: &str, version: &str, core: &str, store: &str, engine: &str) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "{version}"
edition = "2021"

[[bin]]
name = "{name}"
path = "src/main.rs"

[dependencies]
noether-core   = {{ path = "{core}" }}
noether-store  = {{ path = "{store}" }}
noether-engine = {{ path = "{engine}" }}
serde_json = "1"
"#
    )
}

/// Escape a string for embedding inside a Rust double-quoted string literal.
fn rust_str_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn generate_main_rs(app_name: &str, app_version: &str, description: &str) -> String {
    MAIN_RS_TEMPLATE
        .replace("__APP_NAME__", app_name)
        .replace("__APP_VERSION__", app_version)
        .replace("__APP_DESCRIPTION__", &rust_str_escape(description))
}

/// Template for the generated binary's main.rs.
///
/// Substitution tokens (replaced by `generate_main_rs`):
///   __APP_NAME__        — binary / ACLI command name
///   __APP_VERSION__     — version string (inherited from noether)
///   __APP_DESCRIPTION__ — one-line description for --help (already escaped)
const MAIN_RS_TEMPLATE: &str = r###"// Auto-generated by `noether build`. Do not edit.
use noether_core::stage::Stage;
use noether_core::stdlib::load_stdlib;
use noether_engine::checker::check_graph;
use noether_engine::executor::composite::CompositeExecutor;
use noether_engine::executor::runner::run_composition;
use noether_engine::lagrange::{compute_composition_id, parse_graph};
use noether_engine::planner::plan_graph;
use noether_engine::providers;
use noether_store::{MemoryStore, StageStore};
use std::sync::Arc;

const APP_NAME: &str = "__APP_NAME__";
const APP_VERSION: &str = "__APP_VERSION__";
const APP_DESCRIPTION: &str = "__APP_DESCRIPTION__";

const GRAPH_JSON: &str = include_str!("graph.json");
const BUNDLE_JSON: &str = include_str!("bundle.json");

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }
    if args.iter().any(|a| a == "--version") {
        println!("{APP_NAME} {APP_VERSION}");
        return;
    }

    // --serve <addr>: run as an HTTP microservice (thread-per-request)
    if let Some(addr) = args.windows(2).find(|w| w[0] == "--serve").map(|w| w[1].clone()) {
        run_serve(&addr);
        return;
    }

    let dry_run = args.iter().any(|a| a == "--dry-run");

    let input_str = args
        .windows(2)
        .find(|w| w[0] == "--input")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| {
            // If no --input flag, read from stdin if it's not a terminal.
            use std::io::{IsTerminal, Read};
            if !std::io::stdin().is_terminal() {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf).ok();
                let trimmed = buf.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
            "null".into()
        });

    let input: serde_json::Value = serde_json::from_str(&input_str)
        .unwrap_or_else(|_| serde_json::Value::String(input_str.clone()));

    let (store, graph, composition_id, executor) = bootstrap();

    // Type-check
    let check = match check_graph(&graph.root, &store) {
        Ok(c) => c,
        Err(errs) => {
            let msgs: Vec<String> = errs.iter().map(|e| e.to_string()).collect();
            emit_error("TYPE_ERROR", &format!("{} type error(s)", msgs.len()), &msgs);
            std::process::exit(2);
        }
    };

    if dry_run {
        let plan = plan_graph(&graph.root, &store);
        let plan_val = serde_json::to_value(&plan).unwrap_or_default();
        let warnings: Vec<String> = check.warnings.iter().map(|w| w.to_string()).collect();
        emit_ok(serde_json::json!({
            "mode": "dry-run",
            "composition_id": composition_id,
            "type_check": {
                "input":  check.resolved.input.to_string(),
                "output": check.resolved.output.to_string(),
            },
            "plan":     plan_val,
            "warnings": warnings,
        }));
        return;
    }

    match run_composition(&graph.root, &input, executor.as_ref(), &composition_id) {
        Ok(result) => {
            emit_ok(serde_json::json!({
                "composition_id": composition_id,
                "output":         result.output,
            }));
        }
        Err(e) => {
            emit_error("EXECUTION_ERROR", &e.to_string(), &[]);
            std::process::exit(1);
        }
    }
}

// ── Bootstrap ─────────────────────────────────────────────────────────────────

type AppExecutor = Arc<CompositeExecutor>;

fn bootstrap() -> (
    MemoryStore,
    noether_engine::lagrange::CompositionGraph,
    String,
    AppExecutor,
) {
    let mut store = MemoryStore::new();
    for stage in load_stdlib() {
        store.put(stage).ok();
    }
    let bundle: Vec<Stage> =
        serde_json::from_str(BUNDLE_JSON).expect("embedded bundle is invalid");
    for stage in bundle {
        store.put(stage).ok();
    }

    let graph = parse_graph(GRAPH_JSON).expect("embedded graph is invalid");
    // The graph is baked in at `noether build` time — a hash
    // failure here means the build step produced a broken binary.
    // Panicking with a clear message beats silently shipping a
    // stringly-typed "embedded" placeholder that would collide in
    // any post-build correlation log. This path only runs once, at
    // the generated binary's startup.
    let composition_id = compute_composition_id(&graph).unwrap_or_else(|e| {
        panic!(
            "embedded composition graph failed to hash: {e}. \
             The binary produced by `noether build` is malformed — \
             rebuild from a current noether release."
        )
    });

    let (llm, _) = providers::build_llm_provider();
    let executor = Arc::new(
        CompositeExecutor::from_store(&store)
            .with_llm(llm, noether_engine::llm::LlmConfig::default()),
    );

    (store, graph, composition_id, executor)
}

// ── HTTP serve mode ───────────────────────────────────────────────────────────

fn run_serve(addr: &str) {
    use std::net::TcpListener;

    // Build the store, graph, and executor once at startup.
    let (store, graph, composition_id, executor) = bootstrap();

    // Validate the graph once up front — fail fast before accepting connections.
    if let Err(errs) = check_graph(&graph.root, &store) {
        for e in &errs {
            eprintln!("Type error: {e}");
        }
        std::process::exit(2);
    }

    let graph = Arc::new(graph);
    let composition_id = Arc::new(composition_id);

    // Normalise `:PORT` shorthand → `0.0.0.0:PORT` so `TcpListener::bind` is happy.
    let bind_addr: String = if addr.starts_with(':') {
        format!("0.0.0.0{addr}")
    } else {
        addr.to_string()
    };

    let listener = TcpListener::bind(&bind_addr)
        .unwrap_or_else(|e| panic!("Failed to bind to {bind_addr}: {e}"));

    eprintln!("{APP_NAME} {APP_VERSION} — listening on http://{bind_addr}");
    eprintln!("  GET  /        browser dashboard");
    eprintln!("  POST /        execute with {{\"input\": ...}}");
    eprintln!("  GET  /health  liveness check");

    for stream in listener.incoming() {
        match stream {
            Ok(conn) => {
                let executor = Arc::clone(&executor);
                let graph = Arc::clone(&graph);
                let composition_id = Arc::clone(&composition_id);
                std::thread::spawn(move || {
                    handle_connection(conn, &graph, &composition_id, &executor);
                });
            }
            Err(e) => eprintln!("Connection error: {e}"),
        }
    }
}

fn handle_connection(
    stream: std::net::TcpStream,
    graph: &noether_engine::lagrange::CompositionGraph,
    composition_id: &str,
    executor: &CompositeExecutor,
) {
    use std::io::{BufRead, BufReader, Read};

    // Use a shared reference for BufReader so we can still write to the stream.
    let mut reader = BufReader::new(&stream);

    // Parse request line
    let mut req_line = String::new();
    if reader.read_line(&mut req_line).unwrap_or(0) == 0 {
        return;
    }
    let parts: Vec<&str> = req_line.trim().splitn(3, ' ').collect();
    if parts.len() < 2 {
        http_respond(&stream, 400, "application/json", r#"{"ok":false,"error":{"message":"Bad Request"}}"#);
        return;
    }
    let method = parts[0];
    let path_and_query = parts[1];
    let path = path_and_query.split('?').next().unwrap_or(path_and_query);

    // Consume headers, capture Content-Length
    let mut content_length: usize = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            break;
        }
        let header = header.trim();
        if header.is_empty() {
            break;
        }
        if header.to_lowercase().starts_with("content-length:") {
            content_length = header["content-length:".len()..]
                .trim()
                .parse()
                .unwrap_or(0);
        }
    }

    match (method, path) {
        // ── Browser dashboard ──────────────────────────────────────────────
        ("GET", "/") => {
            let html = build_dashboard_html();
            http_respond(&stream, 200, "text/html; charset=utf-8", &html);
        }

        // ── CORS preflight ─────────────────────────────────────────────────
        ("OPTIONS", _) => {
            http_respond(&stream, 200, "application/json", "{}");
        }

        // ── Liveness ───────────────────────────────────────────────────────
        ("GET", "/health") => {
            let body = serde_json::to_string(&serde_json::json!({
                "ok": true, "service": APP_NAME, "version": APP_VERSION,
            }))
            .unwrap();
            http_respond(&stream, 200, "application/json", &body);
        }

        // ── Execute ────────────────────────────────────────────────────────
        ("POST", "/") | ("POST", "") | ("POST", "/run") => {
            let mut body_bytes = vec![0u8; content_length.min(4 * 1024 * 1024)];
            if content_length > 0 && reader.read_exact(&mut body_bytes).is_err() {
                http_respond(&stream, 400, "application/json", r#"{"ok":false,"error":{"message":"Failed to read body"}}"#);
                return;
            }

            let parsed: serde_json::Value =
                serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
            let input = parsed
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            match run_composition(&graph.root, &input, executor, composition_id) {
                Ok(result) => {
                    // If the output is a string that looks like HTML, serve it directly.
                    let output_str = result.output.as_str();
                    let is_html = output_str
                        .map(|s| {
                            let t = s.trim();
                            t.starts_with("<!DOCTYPE") || t.starts_with("<html") || t.starts_with("<HTML")
                        })
                        .unwrap_or(false);

                    if is_html {
                        http_respond(&stream, 200, "text/html; charset=utf-8",
                            output_str.unwrap_or(""));
                    } else {
                        let body = serde_json::to_string_pretty(&serde_json::json!({
                            "ok":      true,
                            "command": APP_NAME,
                            "data": {
                                "composition_id": composition_id,
                                "output":         result.output,
                            },
                            "meta": { "version": APP_VERSION },
                        }))
                        .unwrap();
                        http_respond(&stream, 200, "application/json", &body);
                    }
                }
                Err(e) => {
                    let body = serde_json::to_string_pretty(&serde_json::json!({
                        "ok":      false,
                        "command": APP_NAME,
                        "error": { "code": "EXECUTION_ERROR", "message": e.to_string() },
                        "meta": { "version": APP_VERSION },
                    }))
                    .unwrap();
                    http_respond(&stream, 200, "application/json", &body);
                }
            }
        }

        // ── API discovery fallback ─────────────────────────────────────────
        _ => {
            let body = serde_json::to_string_pretty(&serde_json::json!({
                "service":     APP_NAME,
                "version":     APP_VERSION,
                "description": APP_DESCRIPTION,
                "endpoints": [
                    { "method": "GET",  "path": "/",       "description": "Browser dashboard" },
                    { "method": "POST", "path": "/",       "body": {"input": "<JSON>"}, "description": "Execute the composition" },
                    { "method": "GET",  "path": "/health", "description": "Liveness check" },
                ],
            }))
            .unwrap();
            http_respond(&stream, 200, "application/json", &body);
        }
    }

    let mut w: &std::net::TcpStream = &stream;
    std::io::Write::flush(&mut w).ok();
}

fn build_dashboard_html() -> String {
    // Try to extract example input from the first bundled stage.
    // Used to pre-populate the textarea so users see the right format immediately.
    let example_json: String = serde_json::from_str::<Vec<serde_json::Value>>(BUNDLE_JSON)
        .ok()
        .and_then(|stages| {
            stages.into_iter().find_map(|s| {
                s.get("examples")
                    .and_then(|e| e.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|ex| ex.get("input"))
                    .map(|input| serde_json::to_string_pretty(input).unwrap_or_default())
            })
        })
        .unwrap_or_else(|| "{}".into());

    // Escape for embedding inside an HTML attribute value (single-quoted in the template).
    let example_escaped = example_json
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");

    format!(r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{name}</title>
<style>
  @import url('https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@300;400;600&family=Space+Grotesk:wght@400;600;700&display=swap');
  :root {{
    --bg: #0a0e17; --surface: #111827; --border: #1e2d40; --accent: #3b82f6;
    --accent2: #10b981; --text: #e2e8f0; --muted: #64748b; --danger: #ef4444;
  }}
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  body {{ background: var(--bg); color: var(--text); font-family: 'Space Grotesk', sans-serif;
          min-height: 100vh; display: flex; flex-direction: column; }}
  header {{ border-bottom: 1px solid var(--border); padding: 1.25rem 2rem;
            display: flex; align-items: center; gap: 1rem; background: var(--surface); }}
  .logo {{ font-size: 0.7rem; font-family: 'JetBrains Mono', monospace; color: var(--accent);
           background: #1e3a5f; border: 1px solid var(--accent); border-radius: 4px;
           padding: 0.2rem 0.5rem; letter-spacing: 0.05em; }}
  header h1 {{ font-size: 1.1rem; font-weight: 600; color: var(--text); }}
  header p {{ font-size: 0.8rem; color: var(--muted); margin-left: auto; font-family: 'JetBrains Mono', monospace; }}
  .container {{ flex: 1; display: grid; grid-template-columns: 340px 1fr;
                gap: 0; overflow: hidden; height: calc(100vh - 60px); }}
  .sidebar {{ background: var(--surface); border-right: 1px solid var(--border);
              display: flex; flex-direction: column; overflow: hidden; }}
  .sidebar-header {{ padding: 1.25rem 1.5rem 0.75rem; border-bottom: 1px solid var(--border); }}
  .sidebar-header h2 {{ font-size: 0.75rem; font-weight: 600; text-transform: uppercase;
                        letter-spacing: 0.08em; color: var(--muted); }}
  .input-area {{ flex: 1; padding: 1.25rem 1.5rem; display: flex; flex-direction: column; gap: 1rem; }}
  textarea {{ width: 100%; flex: 1; min-height: 180px; background: var(--bg);
              border: 1px solid var(--border); border-radius: 6px; color: var(--text);
              font-family: 'JetBrains Mono', monospace; font-size: 0.78rem; padding: 0.75rem;
              resize: vertical; line-height: 1.6; outline: none; transition: border-color 0.2s; }}
  textarea:focus {{ border-color: var(--accent); }}
  .run-btn {{ width: 100%; padding: 0.75rem; background: var(--accent); color: white;
              border: none; border-radius: 6px; font-family: 'Space Grotesk', sans-serif;
              font-size: 0.9rem; font-weight: 600; cursor: pointer; transition: all 0.2s;
              display: flex; align-items: center; justify-content: center; gap: 0.5rem; }}
  .run-btn:hover {{ background: #2563eb; }}
  .run-btn:disabled {{ background: var(--border); cursor: not-allowed; color: var(--muted); }}
  .status {{ font-size: 0.75rem; color: var(--muted); font-family: 'JetBrains Mono', monospace;
             padding: 0.5rem 0; text-align: center; min-height: 1.5rem; }}
  .status.ok {{ color: var(--accent2); }}
  .status.err {{ color: var(--danger); }}
  .result-pane {{ overflow: auto; background: var(--bg); position: relative; }}
  .result-pane.empty {{ display: flex; align-items: center; justify-content: center;
                        flex-direction: column; gap: 1rem; color: var(--muted); }}
  .result-pane.empty .hint {{ font-size: 0.8rem; font-family: 'JetBrains Mono', monospace; }}
  .result-pane.empty .big {{ font-size: 3rem; opacity: 0.15; }}
  .html-frame {{ width: 100%; height: 100%; border: none; background: white; }}
  .json-output {{ padding: 1.5rem; font-family: 'JetBrains Mono', monospace;
                  font-size: 0.78rem; line-height: 1.7; white-space: pre-wrap; color: #94a3b8; }}
  .spinner {{ width: 24px; height: 24px; border: 2px solid var(--border);
              border-top-color: var(--accent); border-radius: 50%;
              animation: spin 0.7s linear infinite; }}
  @keyframes spin {{ to {{ transform: rotate(360deg); }} }}
  .loading-overlay {{ position: absolute; inset: 0; background: rgba(10,14,23,0.8);
                      display: flex; align-items: center; justify-content: center;
                      flex-direction: column; gap: 1rem; }}
  .loading-text {{ font-family: 'JetBrains Mono', monospace; font-size: 0.8rem; color: var(--muted); }}
</style>
</head>
<body>
<header>
  <span class="logo">noether</span>
  <h1>{name}</h1>
  <p>v{version} · --serve mode</p>
</header>
<div class="container">
  <aside class="sidebar">
    <div class="sidebar-header"><h2>Input</h2></div>
    <div class="input-area">
      <textarea id="inputJson" placeholder='null'>{example}</textarea>
      <button class="run-btn" id="runBtn" onclick="runComposition()">
        <span>▶ Run</span>
      </button>
      <div class="status" id="status"></div>
    </div>
  </aside>
  <main class="result-pane empty" id="resultPane">
    <div class="big">⬡</div>
    <div class="hint">Enter input and click Run</div>
  </main>
</div>
<script>
const resultPane = document.getElementById('resultPane');
const runBtn = document.getElementById('runBtn');
const statusEl = document.getElementById('status');
const inputEl = document.getElementById('inputJson');

async function runComposition() {{
  runBtn.disabled = true;
  runBtn.innerHTML = '<div class="spinner"></div><span>Running…</span>';
  statusEl.className = 'status';
  statusEl.textContent = 'Executing composition…';

  // Show loading overlay over previous result
  resultPane.classList.remove('empty');
  const overlay = document.createElement('div');
  overlay.className = 'loading-overlay';
  overlay.innerHTML = '<div class="spinner" style="width:36px;height:36px;border-width:3px"></div><div class="loading-text">Executing stages…</div>';
  resultPane.appendChild(overlay);

  const inputStr = inputEl.value.trim() || 'null';
  let inputVal;
  try {{ inputVal = JSON.parse(inputStr); }}
  catch (e) {{ inputVal = inputStr; }}

  // Auto-unwrap if user accidentally wrapped in {{"input": ...}} envelope
  if (inputVal && typeof inputVal === 'object' && !Array.isArray(inputVal)) {{
    const keys = Object.keys(inputVal);
    if (keys.length === 1 && keys[0] === 'input') {{
      inputVal = inputVal.input;
    }}
  }}

  const t0 = performance.now();
  try {{
    const resp = await fetch('/', {{
      method: 'POST',
      headers: {{'Content-Type': 'application/json'}},
      body: JSON.stringify({{input: inputVal}})
    }});
    const ct = resp.headers.get('content-type') || '';
    const ms = Math.round(performance.now() - t0);

    if (ct.includes('text/html')) {{
      const html = await resp.text();
      resultPane.innerHTML = '';
      const iframe = document.createElement('iframe');
      iframe.className = 'html-frame';
      resultPane.appendChild(iframe);
      iframe.contentDocument.open();
      iframe.contentDocument.write(html);
      iframe.contentDocument.close();
      statusEl.className = 'status ok';
      statusEl.textContent = `✓ Done in ${{ms}}ms`;
    }} else {{
      const data = await resp.json();
      overlay.remove();
      if (data.ok) {{
        const out = data?.data?.output;
        if (typeof out === 'string') {{
          const t = out.trim();
          if (t.startsWith('<') && (t.startsWith('<!DOCTYPE') || t.startsWith('<html'))) {{
            resultPane.innerHTML = '';
            const iframe = document.createElement('iframe');
            iframe.className = 'html-frame';
            resultPane.appendChild(iframe);
            iframe.contentDocument.open();
            iframe.contentDocument.write(out);
            iframe.contentDocument.close();
          }} else {{
            resultPane.innerHTML = `<pre class="json-output">${{escHtml(out)}}</pre>`;
          }}
        }} else {{
          resultPane.innerHTML = `<pre class="json-output">${{escHtml(JSON.stringify(data, null, 2))}}</pre>`;
        }}
        statusEl.className = 'status ok';
        statusEl.textContent = `✓ Completed in ${{ms}}ms`;
      }} else {{
        resultPane.innerHTML = `<pre class="json-output" style="color:#ef4444">${{escHtml(JSON.stringify(data, null, 2))}}</pre>`;
        statusEl.className = 'status err';
        statusEl.textContent = `✗ Error: ${{data?.error?.message || 'unknown'}}`;
      }}
    }}
  }} catch(e) {{
    overlay?.remove();
    resultPane.innerHTML = `<pre class="json-output" style="color:#ef4444">Network error: ${{e.message}}</pre>`;
    statusEl.className = 'status err';
    statusEl.textContent = `✗ ${{e.message}}`;
  }}

  runBtn.disabled = false;
  runBtn.innerHTML = '<span>▶ Run</span>';
}}

function escHtml(s) {{
  return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
}}

// Auto-run on load if input looks meaningful
window.addEventListener('load', () => {{
  const v = inputEl.value.trim();
  if (v && v !== '{{}}' && v !== 'null') runComposition();
}});
</script>
</body>
</html>"#,
        name = APP_NAME,
        version = APP_VERSION,
        example = example_escaped,
    )
}

fn http_respond(stream: &std::net::TcpStream, status: u16, content_type: &str, body: &str) {
    use std::io::Write;
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    let mut w: &std::net::TcpStream = stream;
    w.write_all(response.as_bytes()).ok();
}

// ── Single-shot helpers ───────────────────────────────────────────────────────

fn emit_ok(data: serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "ok":      true,
            "command": APP_NAME,
            "data":    data,
            "meta":    { "version": APP_VERSION },
        }))
        .unwrap()
    );
}

fn emit_error(code: &str, message: &str, hints: &[String]) {
    let mut err = serde_json::json!({ "code": code, "message": message });
    if !hints.is_empty() {
        err["hints"] = serde_json::json!(hints);
    }
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "ok":      false,
            "command": APP_NAME,
            "error":   err,
            "meta":    { "version": APP_VERSION },
        }))
        .unwrap()
    );
}

fn print_help() {
    eprintln!("{APP_NAME} {APP_VERSION}");
    eprintln!("{APP_DESCRIPTION}");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("  {APP_NAME} [OPTIONS]");
    eprintln!();
    eprintln!("OPTIONS:");
    eprintln!("  --input <JSON>      Input value passed to the composition (default: null)");
    eprintln!("  --serve <addr>      Start as an HTTP microservice (e.g. :8080 or 0.0.0.0:8080)");
    eprintln!("  --dry-run           Type-check and show execution plan without executing");
    eprintln!("  --version           Show version");
    eprintln!("  --help              Show this help");
    eprintln!();
    eprintln!("HTTP MODE (--serve):");
    eprintln!("  POST /        Execute — body: {{\"input\": <JSON>}}");
    eprintln!("  GET  /health  Liveness check");
    eprintln!();
    eprintln!("OUTPUT:");
    eprintln!("  ACLI-compatible JSON on stdout  (ok/data/meta or ok/error/meta)");
    eprintln!();
    eprintln!("LLM STAGES:");
    eprintln!("  Set VERTEX_AI_TOKEN, VERTEX_AI_PROJECT, VERTEX_AI_LOCATION to enable");
    eprintln!("  cloud LLM execution. VERTEX_AI_MODEL selects the model.");
}
"###;
