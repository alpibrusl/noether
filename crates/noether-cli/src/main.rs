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
    },
}

#[derive(Subcommand)]
enum StageCommands {
    /// Search the store by semantic query
    Search {
        /// The search query
        query: String,
    },
    /// Register a new stage from a spec file
    Add {
        /// Path to the stage spec JSON file
        spec: String,
    },
    /// List all stages in the store
    List,
    /// Retrieve a stage by its hash ID
    Get {
        /// The stage hash (or prefix)
        hash: String,
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

fn build_index(store: &dyn StageStore) -> SemanticIndex {
    let (provider, name) = providers::build_embedding_provider();
    if name != "mock" {
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
            eprintln!("Warning: embedding provider unavailable ({e}), using mock index.");
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
                StageCommands::Search { query } => {
                    let index = build_index(store.as_ref());
                    commands::stage::cmd_search(store.as_ref(), &index, &query);
                }
                StageCommands::Add { spec } => {
                    let author_key = load_or_create_author_key(&noether_dir());
                    let index = build_index(store.as_ref());
                    commands::stage::cmd_add(store.as_mut(), &spec, &author_key, &index);
                }
                StageCommands::List => commands::stage::cmd_list(store.as_ref()),
                StageCommands::Get { hash } => commands::stage::cmd_get(store.as_ref(), &hash),
            }
        }
        Commands::Store { command } => {
            let mut store = build_store(registry);
            match command {
                StoreCommands::Stats => {
                    let index = build_index(store.as_ref());
                    commands::store::cmd_stats(store.as_ref(), &index);
                }
                StoreCommands::Retro {
                    dry_run,
                    apply,
                    threshold,
                } => {
                    let index = build_index(store.as_ref());
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
                    let index = build_index(store.as_ref());
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
            let input_value = input
                .as_deref()
                .map(|s| serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.into())))
                .unwrap_or(serde_json::Value::Null);
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
        Commands::Compose {
            problem,
            model,
            dry_run,
            input,
            force,
            allow_capabilities,
            budget_cents,
        } => {
            let mut store = build_store(registry);
            let mut index = build_index(store.as_ref());
            let (llm, llm_name) = providers::build_llm_provider();
            if llm_name != "mock" {
                eprintln!("LLM provider: {llm_name}");
            }

            let resolved_model = model
                .or_else(|| std::env::var("VERTEX_AI_MODEL").ok())
                .unwrap_or_else(|| noether_engine::llm::LlmConfig::default().model);

            let input_value = input
                .as_deref()
                .map(|s| serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.into())))
                .unwrap_or(serde_json::Value::Null);

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
