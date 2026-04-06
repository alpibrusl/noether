mod commands;
mod output;

use clap::{Parser, Subcommand};
use noether_core::stdlib::load_stdlib;
use noether_engine::index::{IndexConfig, SemanticIndex};
use noether_engine::providers;
use noether_engine::trace::JsonFileTraceStore;
use noether_store::{JsonFileStore, StageStore};

#[derive(Parser)]
#[command(name = "noether", about = "Agent-native verified composition platform")]
struct Cli {
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
    },
    /// Retrieve execution trace for a past composition
    Trace {
        /// The composition ID (hash)
        composition_id: String,
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
}

fn noether_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("NOETHER_HOME") {
        std::path::PathBuf::from(dir)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        std::path::PathBuf::from(home).join(".noether")
    }
}

fn init_store() -> JsonFileStore {
    let store_path = noether_dir().join("store.json");
    let mut store = JsonFileStore::open(&store_path).unwrap_or_else(|e| {
        eprintln!(
            "Warning: failed to open store at {}: {e}",
            store_path.display()
        );
        eprintln!("Using empty store.");
        JsonFileStore::open("/dev/null").unwrap()
    });

    // Always upsert stdlib stages so updates to stdlib are applied automatically.
    for stage in load_stdlib() {
        let _ = store.put(stage);
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

fn build_index(store: &dyn StageStore) -> SemanticIndex {
    let (provider, name) = providers::build_embedding_provider();
    if name != "mock" {
        eprintln!("Embedding provider: {name}");
    }

    if name == "mock" {
        // No caching needed for mock
        SemanticIndex::build(store, provider, IndexConfig::default()).unwrap()
    } else {
        // Use cached embeddings for real providers
        let cache_path = noether_dir().join("embeddings.json");
        let cached =
            noether_engine::index::cache::CachedEmbeddingProvider::new(provider, cache_path);
        SemanticIndex::build_cached(store, cached, IndexConfig::default()).unwrap()
    }
}

fn main() {
    let cli = Cli::parse();
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
            let mut store = init_store();
            match command {
                StageCommands::Search { query } => {
                    let index = build_index(&store);
                    commands::stage::cmd_search(&store, &index, &query);
                }
                StageCommands::Add { spec } => commands::stage::cmd_add(&mut store, &spec),
                StageCommands::List => commands::stage::cmd_list(&store),
                StageCommands::Get { hash } => commands::stage::cmd_get(&store, &hash),
            }
        }
        Commands::Store { command } => {
            let mut store = init_store();
            match command {
                StoreCommands::Stats => commands::store::cmd_stats(&store),
                StoreCommands::Retro { dry_run, apply, threshold } => {
                    let index = build_index(&store);
                    commands::store::cmd_retro(&mut store, &index, dry_run, apply, threshold);
                }
            }
        }
        Commands::Run {
            graph,
            dry_run,
            input,
        } => {
            let store = init_store();
            let input_value = input
                .as_deref()
                .map(|s| serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.into())))
                .unwrap_or(serde_json::Value::Null);
            commands::run::cmd_run(&store, &graph, dry_run, &input_value);
        }
        Commands::Trace { composition_id } => {
            let trace_store = init_trace_store();
            commands::trace::cmd_trace(&trace_store, &composition_id);
        }
        Commands::Compose {
            problem,
            model,
            dry_run,
            input,
            force,
        } => {
            let mut store = init_store();
            let mut index = build_index(&store);
            let (llm, llm_name) = providers::build_llm_provider();
            if llm_name != "mock" {
                eprintln!("LLM provider: {llm_name}");
            }

            // Resolve model: CLI flag → VERTEX_AI_MODEL env → default for this provider
            let resolved_model = model
                .or_else(|| std::env::var("VERTEX_AI_MODEL").ok())
                .unwrap_or_else(|| noether_engine::llm::LlmConfig::default().model);

            let input_value = input
                .as_deref()
                .map(|s| serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.into())))
                .unwrap_or(serde_json::Value::Null);

            let cache_path = noether_dir().join("compositions.json");

            commands::compose::cmd_compose(
                &mut store,
                &mut index,
                llm.as_ref(),
                &problem,
                commands::compose::ComposeOptions {
                    model: &resolved_model,
                    dry_run,
                    input: &input_value,
                    force,
                    cache_path: &cache_path,
                },
            );
        }
    }
}
