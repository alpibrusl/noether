mod commands;
mod output;

use clap::{Parser, Subcommand};
use ed25519_dalek::SigningKey;
use noether_core::capability::Capability;
use noether_core::effects::EffectKind;
use noether_core::stdlib::load_stdlib;
use noether_engine::checker::{CapabilityPolicy, EffectPolicy};
use noether_engine::index::{IndexConfig, SemanticIndex};
use noether_engine::providers;
use noether_engine::registry_client::RemoteStageStore;
use noether_engine::trace::JsonFileTraceStore;
use noether_store::{JsonFileStore, StageStore};

#[derive(Parser)]
#[command(name = "noether", about = "Agent-native verified composition platform")]
struct Cli {
    /// Remote registry URL (e.g. http://localhost:3000).
    /// Also read from NOETHER_REGISTRY env var. When set, all stage/store
    /// commands talk to the remote registry instead of the local file store.
    #[arg(long, global = true, env = "NOETHER_REGISTRY")]
    registry: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Return full command tree as JSON (ACLI standard)
    Introspect,
    /// Show version information
    Version,
    /// Stage management commands
    Stage {
        #[command(subcommand)]
        command: StageCommands,
    },
    /// Store management commands
    Store {
        #[command(subcommand)]
        command: StoreCommands,
    },
    /// Execute a composition graph
    Run {
        /// Path to the Lagrange graph JSON file
        graph: String,
        /// Verify and plan without executing
        #[arg(long)]
        dry_run: bool,
        /// Input data as JSON string (default: null)
        #[arg(long)]
        input: Option<String>,
        /// Comma-separated list of capabilities to grant (e.g. network,fs-read).
        /// Default: all capabilities are allowed.
        #[arg(long)]
        allow_capabilities: Option<String>,
        /// Comma-separated list of effect kinds to allow
        /// (pure, fallible, llm, network, non-deterministic, cost, unknown).
        /// Default: all effects are allowed.
        #[arg(long)]
        allow_effects: Option<String>,
        /// Reject compositions whose estimated cost exceeds this value (in cents).
        #[arg(long)]
        budget_cents: Option<u64>,
    },
    /// Retrieve execution trace for a past composition
    Trace {
        /// The composition ID (hash)
        composition_id: String,
    },
    /// Compile a composition graph into a self-contained binary
    Build {
        /// Path to the Lagrange graph JSON file
        graph: String,
        /// Output binary path (native) or directory (browser). Default: ./noether-app
        #[arg(short, long, default_value = "./noether-app")]
        output: String,
        /// Override the binary name used in ACLI output and --help (default: output filename)
        #[arg(long)]
        name: Option<String>,
        /// One-line description shown in the binary's --help (default: graph description)
        #[arg(long)]
        description: Option<String>,
        /// Build target: "native" (default), "browser" (produces HTML+WASM directory), or "react-native"
        #[arg(long, default_value = "native")]
        target: String,
        /// After building (native target only): immediately start the binary as an HTTP server.
        /// Accepts ":PORT" shorthand or "HOST:PORT". E.g. --serve :8080
        #[arg(long)]
        serve: Option<String>,
    },
    /// Serve a composition graph as an HTTP API (no compilation needed)
    Serve {
        /// Path to the Lagrange graph JSON file
        graph: String,
        /// Bind address (e.g. ":8080", "0.0.0.0:3000")
        #[arg(long, short, default_value = ":8080")]
        port: String,
    },
    /// Compose a solution from a problem description using the Composition Agent
    Compose {
        /// Problem description in natural language
        problem: String,
        /// LLM model to use. Defaults to VERTEX_AI_MODEL env var, then gemini-2.5-flash.
        /// Mistral models auto-route: mistral-small-2503, mistral-medium-3, codestral-2
        #[arg(long)]
        model: Option<String>,
        /// Show the graph without executing
        #[arg(long)]
        dry_run: bool,
        /// Input data as JSON string (default: null)
        #[arg(long)]
        input: Option<String>,
        /// Bypass the composition cache and always call the LLM
        #[arg(long)]
        force: bool,
        /// Comma-separated list of capabilities to grant (e.g. network,fs-read).
        /// Default: all capabilities are allowed.
        #[arg(long)]
        allow_capabilities: Option<String>,
        /// Reject compositions whose estimated cost exceeds this value (in cents).
        #[arg(long)]
        budget_cents: Option<u64>,
        /// Show the composition reasoning: search candidates, LLM prompt, and
        /// each attempt's response. Useful for debugging and understanding
        /// how noether compose selects stages.
        #[arg(long, short)]
        verbose: bool,
    },
}

#[derive(Subcommand)]
enum StageCommands {
    /// Search the store by semantic query
    Search {
        /// The search query
        query: String,
        /// Filter results to stages carrying this tag (exact match)
        #[arg(long, value_name = "TAG")]
        tag: Option<String>,
    },
    /// Register a new stage from a spec file
    Add {
        /// Path to the stage spec JSON file
        spec: String,
        /// Keep the stage in Draft lifecycle instead of auto-promoting to Active.
        /// By default, `stage add` activates the stage immediately.
        #[arg(long)]
        draft: bool,
    },
    /// Bulk-import all *.json stage specs from a directory
    Sync {
        /// Path to a directory containing one stage spec JSON per file
        directory: String,
        /// Keep imported stages in Draft lifecycle instead of auto-promoting
        #[arg(long)]
        draft: bool,
    },
    /// List all stages in the store
    List {
        /// Filter stages by tag (exact match)
        #[arg(long, value_name = "TAG")]
        tag: Option<String>,
        /// Filter by signer: `stdlib` (only stdlib stages), `custom` (only
        /// non-stdlib), or a hex-encoded public key prefix.
        #[arg(long, value_name = "WHO")]
        signed_by: Option<String>,
        /// Filter by lifecycle: draft | active | deprecated | tombstone
        /// (default: active only)
        #[arg(long, value_name = "STATE")]
        lifecycle: Option<String>,
        /// Print full 64-character stage IDs instead of 8-character prefixes.
        #[arg(long)]
        full_ids: bool,
    },
    /// Retrieve a stage by its hash ID
    Get {
        /// The stage hash (or prefix)
        hash: String,
    },
    /// Promote a Draft stage to Active
    Activate {
        /// The stage hash (or prefix)
        hash: String,
    },
    /// Verify a stage's implementation against its declared examples.
    /// With no argument, tests every Active stage in the store.
    Test {
        /// Optional stage hash or prefix. If omitted, every Active stage
        /// is tested.
        hash: Option<String>,
    },
    /// Verify a stage's Ed25519 signature and/or its declarative
    /// properties against examples. Complements `stage test`, which
    /// executes each example through the runtime.
    ///
    /// By default both checks run. `--signatures` restricts to
    /// signature verification only; `--properties` restricts to
    /// property checking only. Passing both flags is equivalent to
    /// passing neither (all checks run).
    Verify {
        /// Optional stage hash or prefix. If omitted, every Active
        /// stage in the store is verified.
        hash: Option<String>,
        /// Only check Ed25519 signatures.
        #[arg(long)]
        signatures: bool,
        /// Only check declarative properties.
        #[arg(long)]
        properties: bool,
    },
}

#[derive(Subcommand)]
enum StoreCommands {
    /// Show store statistics
    Stats,
    /// Scan for near-duplicate stages and optionally deprecate them
    Retro {
        /// Show the report without applying changes
        #[arg(long)]
        dry_run: bool,
        /// Apply suggested deprecations and merges
        #[arg(long)]
        apply: bool,
        /// Cosine similarity threshold (default: 0.92)
        #[arg(long, default_value_t = 0.92)]
        threshold: f32,
    },
    /// Find near-duplicate stages and optionally tombstone confirmed duplicates
    Dedup {
        /// Cosine similarity threshold — pairs above this are shown (default: 0.90)
        #[arg(long, default_value_t = 0.90)]
        threshold: f32,
        /// Tombstone the lower-scored stage in each duplicate pair
        #[arg(long)]
        apply: bool,
    },
    /// Infer and apply effects for stages currently marked Unknown
    MigrateEffects {
        /// Show the migration plan without applying changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Audit store health: signatures, lifecycle, orphans, examples
    Health,
}

/// Read JSON input from stdin if it has been piped or redirected.
/// Returns `None` when stdin is an interactive terminal — callers should
/// then fall back to their default (typically `Value::Null`).
fn read_stdin_input() -> Option<serde_json::Value> {
    use std::io::{IsTerminal, Read};
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut buf = String::new();
    if stdin.lock().read_to_string(&mut buf).is_err() {
        return None;
    }
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(
        serde_json::from_str(trimmed).unwrap_or_else(|_| serde_json::Value::String(trimmed.into())),
    )
}

fn noether_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("NOETHER_HOME") {
        std::path::PathBuf::from(dir)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        std::path::PathBuf::from(home).join(".noether")
    }
}

/// Return the active store.
///
/// If `registry` is `Some(url)` (from `--registry` or `NOETHER_REGISTRY`),
/// returns a `RemoteStageStore` connected to that URL, printing the stage count.
/// Otherwise returns the local `JsonFileStore` with stdlib seeded.
fn build_store(registry: Option<&str>) -> Box<dyn StageStore> {
    if let Some(url) = registry {
        let api_key = std::env::var("NOETHER_API_KEY").ok();
        match RemoteStageStore::connect(url, api_key.as_deref()) {
            Ok(remote) => {
                eprintln!(
                    "Connected to remote registry at {} ({} stages cached)",
                    remote.base_url(),
                    remote.list(None).len()
                );
                return Box::new(remote);
            }
            Err(e) => {
                eprintln!("Warning: could not connect to registry at {url}: {e}");
                eprintln!("Falling back to local store.");
            }
        }
    }
    Box::new(init_local_store())
}

fn init_local_store() -> JsonFileStore {
    let store_path = noether_dir().join("store.json");
    let mut store = JsonFileStore::open(&store_path).unwrap_or_else(|e| {
        eprintln!(
            "Warning: failed to open store at {}: {e}",
            store_path.display()
        );
        eprintln!("Using empty store.");
        JsonFileStore::open("/dev/null").unwrap()
    });

    // Always upsert stdlib stages so updates to stdlib (including new signatures)
    // are applied automatically, replacing any old unsigned copies.
    // Exception: if a stdlib stage has been explicitly tombstoned, preserve that
    // decision — tombstone is a permanent administrative action.
    let stdlib_ids: std::collections::HashSet<String> =
        load_stdlib().iter().map(|s| s.id.0.clone()).collect();
    for stage in load_stdlib() {
        let already_tombstoned = store
            .get(&stage.id)
            .ok()
            .flatten()
            .map(|s| matches!(s.lifecycle, noether_core::stage::StageLifecycle::Tombstone))
            .unwrap_or(false);
        if !already_tombstoned {
            let _ = store.upsert(stage);
        }
    }

    // Purge legacy unsigned non-stdlib stages: these were synthesized before signing
    // was implemented and would fail verify_signatures. Removing them forces fresh
    // synthesis with a properly signed stage on the next compose call.
    let unsigned_non_stdlib: Vec<noether_core::stage::StageId> = store
        .list(None)
        .iter()
        .filter(|s| s.ed25519_signature.is_none() && !stdlib_ids.contains(&s.id.0))
        .map(|s| s.id.clone())
        .collect();

    if !unsigned_non_stdlib.is_empty() {
        eprintln!(
            "init_store: removing {} unsigned legacy synthesized stage(s) from store",
            unsigned_non_stdlib.len()
        );
        for id in &unsigned_non_stdlib {
            let _ = store.remove(id);
        }
    }

    store
}

fn init_trace_store() -> JsonFileTraceStore {
    let trace_path = noether_dir().join("traces.json");
    JsonFileTraceStore::open(&trace_path).unwrap_or_else(|e| {
        eprintln!("Warning: failed to open trace store: {e}");
        JsonFileTraceStore::open("/tmp/noether-traces.json").unwrap()
    })
}

/// Load the author signing key from `~/.noether/author-key.hex`, generating
/// and saving it if it does not exist yet.
fn load_or_create_author_key(dir: &std::path::Path) -> SigningKey {
    use rand::rngs::OsRng;
    let key_path = dir.join("author-key.hex");
    if key_path.exists() {
        let hex = std::fs::read_to_string(&key_path).unwrap_or_default();
        let bytes = hex::decode(hex.trim()).unwrap_or_default();
        if bytes.len() == 32 {
            let arr: [u8; 32] = bytes.try_into().expect("checked length");
            return SigningKey::from_bytes(&arr);
        }
        eprintln!(
            "Warning: author key at {} is corrupt — regenerating.",
            key_path.display()
        );
    }
    let key = SigningKey::generate(&mut OsRng);
    let hex = hex::encode(key.to_bytes());
    if let Err(e) = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&key_path, &hex)) {
        eprintln!(
            "Warning: could not save author key to {}: {e}",
            key_path.display()
        );
    } else {
        eprintln!(
            "Generated new author signing key → {}\n\
             Public key: {}\n\
             Back this file up — stages you sign are tied to it.",
            key_path.display(),
            hex::encode(key.verifying_key().to_bytes()),
        );
    }
    key
}

/// Build the semantic search index.
///
/// `loud` controls user-facing diagnostics:
/// - `true` (search / compose): prints the active provider and any auth
///   failure, since the user is performing a semantic-search operation and
///   needs to know whether real embeddings are in use.
/// - `false` (dedup checks during `add`, store stats, etc.): silently falls
///   back to the mock provider on failure. The embedding result is best-
///   effort here, so spamming a warning every invocation is just noise.
///   Set `NOETHER_VERBOSE=1` to surface the warning anyway.
fn build_index(store: &dyn StageStore, loud: bool) -> SemanticIndex {
    let verbose = loud || std::env::var("NOETHER_VERBOSE").is_ok();
    let (provider, name) = providers::build_embedding_provider();
    if verbose && name != "mock" {
        eprintln!("Embedding provider: {name}");
    }

    if name == "mock" {
        // No caching needed for mock
        SemanticIndex::build(store, provider, IndexConfig::default()).unwrap()
    } else {
        // Use cached embeddings for real providers; fall back to mock on auth failure
        let cache_path = noether_dir().join("embeddings.json");
        let cached =
            noether_engine::index::cache::CachedEmbeddingProvider::new(provider, cache_path);
        SemanticIndex::build_cached(store, cached, IndexConfig::default()).unwrap_or_else(|e| {
            if verbose {
                eprintln!("Warning: embedding provider unavailable ({e}), using mock index.");
            }
            let mock = noether_engine::index::embedding::MockEmbeddingProvider::new(128);
            SemanticIndex::build(store, Box::new(mock), IndexConfig::default()).unwrap()
        })
    }
}

/// Parse a comma-separated capability list into a `CapabilityPolicy`.
/// `None` (flag not provided) → allow all.
fn parse_capability_policy(raw: Option<&str>) -> CapabilityPolicy {
    match raw {
        None => CapabilityPolicy::allow_all(),
        Some(s) => {
            let caps = s.split(',').filter_map(|token| match token.trim() {
                "network" => Some(Capability::Network),
                "fs-read" => Some(Capability::FsRead),
                "fs-write" => Some(Capability::FsWrite),
                "gpu" => Some(Capability::Gpu),
                "llm" => Some(Capability::Llm),
                other => {
                    eprintln!("Warning: unknown capability '{other}', ignoring");
                    None
                }
            });
            CapabilityPolicy::restrict(caps)
        }
    }
}

/// Parse a comma-separated effect-kind list into an `EffectPolicy`.
/// `None` (flag not provided) → allow all.
fn parse_effect_policy(raw: Option<&str>) -> EffectPolicy {
    match raw {
        None => EffectPolicy::allow_all(),
        Some(s) => {
            let kinds = s.split(',').filter_map(|token| match token.trim() {
                "pure" => Some(EffectKind::Pure),
                "fallible" => Some(EffectKind::Fallible),
                "llm" => Some(EffectKind::Llm),
                "network" => Some(EffectKind::Network),
                "non-deterministic" | "nondeterministic" => Some(EffectKind::NonDeterministic),
                "cost" => Some(EffectKind::Cost),
                "process" => Some(EffectKind::Process),
                "unknown" => Some(EffectKind::Unknown),
                other => {
                    eprintln!("Warning: unknown effect kind '{other}', ignoring");
                    None
                }
            });
            EffectPolicy::restrict(kinds)
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let registry = cli.registry.as_deref();

    match cli.command {
        Commands::Introspect => {
            let tree = output::build_command_tree();
            let json = serde_json::to_value(&tree).unwrap();
            println!("{}", output::acli_ok(json));
        }
        Commands::Version => {
            println!(
                "{}",
                output::acli_ok(serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION")
                }))
            );
        }
        Commands::Stage { command } => {
            let mut store = build_store(registry);
            match command {
                StageCommands::Search { query, tag } => {
                    let index = build_index(store.as_ref(), true);
                    commands::stage::cmd_search(store.as_ref(), &index, &query, tag.as_deref());
                }
                StageCommands::Add { spec, draft } => {
                    let author_key = load_or_create_author_key(&noether_dir());
                    let index = build_index(store.as_ref(), false);
                    commands::stage::cmd_add(store.as_mut(), &spec, &author_key, &index, !draft);
                }
                StageCommands::Sync { directory, draft } => {
                    let author_key = load_or_create_author_key(&noether_dir());
                    let index = build_index(store.as_ref(), false);
                    commands::stage::cmd_sync(
                        store.as_mut(),
                        &directory,
                        &author_key,
                        &index,
                        !draft,
                    );
                }
                StageCommands::List {
                    tag,
                    signed_by,
                    lifecycle,
                    full_ids,
                } => commands::stage::cmd_list(
                    store.as_ref(),
                    commands::stage::ListOptions {
                        tag: tag.as_deref(),
                        signed_by: signed_by.as_deref(),
                        lifecycle: lifecycle.as_deref(),
                        full_ids,
                    },
                ),
                StageCommands::Get { hash } => commands::stage::cmd_get(store.as_ref(), &hash),
                StageCommands::Activate { hash } => {
                    commands::stage::cmd_activate(store.as_mut(), &hash)
                }
                StageCommands::Test { hash } => {
                    let executor = commands::executor_builder::build_executor(store.as_ref());
                    commands::stage::cmd_test(store.as_ref(), &executor, hash.as_deref());
                }
                StageCommands::Verify {
                    hash,
                    signatures,
                    properties,
                } => {
                    // Both-or-neither flag: if the caller passed neither or
                    // both, run all checks; otherwise restrict to the one
                    // they asked for.
                    let (run_sigs, run_props) = match (signatures, properties) {
                        (false, false) | (true, true) => (true, true),
                        (true, false) => (true, false),
                        (false, true) => (false, true),
                    };
                    commands::stage::cmd_verify(
                        store.as_ref(),
                        hash.as_deref(),
                        run_sigs,
                        run_props,
                    );
                }
            }
        }
        Commands::Store { command } => {
            let mut store = build_store(registry);
            match command {
                StoreCommands::Stats => {
                    let index = build_index(store.as_ref(), false);
                    commands::store::cmd_stats(store.as_ref(), &index);
                }
                StoreCommands::Retro {
                    dry_run,
                    apply,
                    threshold,
                } => {
                    let index = build_index(store.as_ref(), false);
                    commands::store::cmd_retro(store.as_mut(), &index, dry_run, apply, threshold);
                }
                StoreCommands::MigrateEffects { dry_run } => {
                    let (llm, _) = providers::build_llm_provider();
                    commands::store::cmd_migrate_effects(store.as_mut(), llm.as_ref(), dry_run);
                }
                StoreCommands::Health => {
                    commands::store::cmd_health(store.as_ref());
                }
                StoreCommands::Dedup { threshold, apply } => {
                    let index = build_index(store.as_ref(), false);
                    commands::store::cmd_dedup(store.as_mut(), &index, threshold, apply);
                }
            }
        }
        Commands::Run {
            graph,
            dry_run,
            input,
            allow_capabilities,
            allow_effects,
            budget_cents,
        } => {
            let store = build_store(registry);
            let mut trace_store = init_trace_store();
            // Resolve --input. If absent, fall back to reading stdin when it
            // is a pipe (so `echo '{...}' | noether run graph.json` works).
            // When stdin is a terminal AND no --input was given, treat input
            // as JSON null (preserves prior CLI semantics).
            let input_value = match input.as_deref() {
                Some(s) => serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.into())),
                None => read_stdin_input().unwrap_or(serde_json::Value::Null),
            };
            let policy = parse_capability_policy(allow_capabilities.as_deref());
            let effect_policy = parse_effect_policy(allow_effects.as_deref());
            commands::run::cmd_run(
                store.as_ref(),
                &mut trace_store,
                &graph,
                dry_run,
                &input_value,
                commands::run::RunPolicies {
                    capabilities: &policy,
                    effects: &effect_policy,
                    budget_cents,
                },
            );
        }
        Commands::Trace { composition_id } => {
            let trace_store = init_trace_store();
            commands::trace::cmd_trace(&trace_store, &composition_id);
        }
        Commands::Build {
            graph,
            output,
            name,
            description,
            target,
            serve,
        } => {
            let store = build_store(registry);
            commands::build::cmd_build(
                store.as_ref(),
                commands::build::BuildOptions {
                    graph_path: &graph,
                    output_path: &output,
                    app_name: name.as_deref(),
                    description: description.as_deref(),
                    target: &target,
                    serve_addr: serve.as_deref(),
                },
            );
        }
        Commands::Serve { graph, port } => {
            let store = build_store(registry);
            let executor = commands::executor_builder::build_executor(store.as_ref());
            commands::serve::cmd_serve(store.as_ref(), &executor, &graph, &port);
        }
        Commands::Compose {
            problem,
            model,
            dry_run,
            input,
            force,
            allow_capabilities,
            budget_cents,
            verbose,
        } => {
            if verbose {
                std::env::set_var("NOETHER_VERBOSE", "1");
            }
            let mut store = build_store(registry);
            let mut index = build_index(store.as_ref(), true);
            let (llm, llm_name) = providers::build_llm_provider();
            if llm_name != "mock" {
                eprintln!("LLM provider: {llm_name}");
            }

            let resolved_model = model
                .or_else(|| std::env::var("VERTEX_AI_MODEL").ok())
                .unwrap_or_else(|| noether_engine::llm::LlmConfig::default().model);

            let input_value = match input.as_deref() {
                Some(s) => serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.into())),
                None => read_stdin_input().unwrap_or(serde_json::Value::Null),
            };

            let cache_path = noether_dir().join("compositions.json");
            let policy = parse_capability_policy(allow_capabilities.as_deref());

            commands::compose::cmd_compose(
                store.as_mut(),
                &mut index,
                llm.as_ref(),
                &problem,
                commands::compose::ComposeOptions {
                    model: &resolved_model,
                    dry_run,
                    input: &input_value,
                    force,
                    cache_path: &cache_path,
                    policy: &policy,
                    budget_cents,
                },
            );
        }
    }
}
