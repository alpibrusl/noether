pub use acli::{CommandInfo, CommandTree, ExitCode};

use acli::{error_envelope, success_envelope};
use serde_json::Value;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Format an ACLI success envelope as JSON string.
pub fn acli_ok(data: Value) -> String {
    let envelope = success_envelope("noether", data, VERSION, None);
    serde_json::to_string_pretty(&envelope).unwrap()
}

/// Format an ACLI error envelope as JSON string.
pub fn acli_error(message: &str) -> String {
    acli_error_hint(message, None)
}

/// Format an ACLI error envelope with an optional hint as JSON string.
pub fn acli_error_hint(message: &str, hint: Option<&str>) -> String {
    let envelope = error_envelope(
        "noether",
        ExitCode::GeneralError,
        message,
        hint,
        None,
        VERSION,
        None,
    );
    serde_json::to_string_pretty(&envelope).unwrap()
}

/// Build the Noether command tree for ACLI introspection.
pub fn build_command_tree() -> CommandTree {
    let mut tree = CommandTree::new("noether", VERSION);

    tree.add_command(
        CommandInfo::new(
            "introspect",
            "Return full command tree as JSON (ACLI standard)",
        )
        .idempotent(true),
    );

    tree.add_command(CommandInfo::new("version", "Show version information").idempotent(true));

    // Stage commands
    let stage_search = CommandInfo::new("search", "Search the store by semantic query")
        .add_argument("query", "string", "The search query", true)
        .idempotent(true)
        .with_examples(vec![
            (
                "Search for text conversion stages",
                "noether stage search \"convert text to number\"",
            ),
            (
                "Search for HTTP stages",
                "noether stage search \"http request\"",
            ),
        ]);

    let stage_add = CommandInfo::new("add", "Register a new stage from a spec file").add_argument(
        "spec",
        "path",
        "Path to the stage spec JSON file",
        true,
    );

    let stage_list = CommandInfo::new("list", "List all stages in the store").idempotent(true);

    let stage_get = CommandInfo::new("get", "Retrieve a stage by its hash ID")
        .add_argument("hash", "string", "The stage hash", true)
        .idempotent(true);

    let mut stage_cmd = CommandInfo::new("stage", "Stage management commands");
    stage_cmd.subcommands = vec![stage_search, stage_add, stage_list, stage_get];
    tree.add_command(stage_cmd);

    // Store commands
    let store_stats = CommandInfo::new("stats", "Show store statistics").idempotent(true);
    let store_retro = CommandInfo::new(
        "retro",
        "Scan for near-duplicate stages and optionally deprecate them",
    )
    .add_option(
        "dry-run",
        "bool",
        "Show the retro report without applying any changes",
        None,
    )
    .add_option(
        "apply",
        "bool",
        "Apply deprecations and merges suggested by the retro report",
        None,
    )
    .add_option(
        "threshold",
        "number",
        "Cosine similarity threshold for near-duplicate detection (default: 0.92)",
        Some(serde_json::json!(0.92)),
    )
    .idempotent(false)
    .with_examples(vec![
        ("Preview duplicates without changes", "noether store retro --dry-run"),
        ("Apply suggested deprecations", "noether store retro --apply"),
    ]);
    let mut store_cmd = CommandInfo::new("store", "Store management commands");
    store_cmd.subcommands = vec![store_stats, store_retro];
    tree.add_command(store_cmd);

    // Run command
    tree.add_command(
        CommandInfo::new("run", "Execute a composition graph")
            .add_argument(
                "graph",
                "path",
                "Path to the Lagrange graph JSON file",
                true,
            )
            .add_option("dry-run", "bool", "Verify and plan without executing", None)
            .add_option(
                "input",
                "string",
                "Input data as JSON string passed to the composition (default: null)",
                Some(serde_json::json!(null)),
            )
            .with_examples(vec![
                ("Execute a graph", "noether run graph.json"),
                ("Dry-run only", "noether run --dry-run graph.json"),
                ("Pass input data", "noether run --input '{\"key\":\"value\"}' graph.json"),
            ]),
    );

    // Trace command
    tree.add_command(
        CommandInfo::new("trace", "Retrieve execution trace for a past composition")
            .add_argument(
                "composition_id",
                "string",
                "The composition ID (hash)",
                true,
            )
            .idempotent(true),
    );

    // Compose command
    tree.add_command(
        CommandInfo::new(
            "compose",
            "Compose a solution from a problem description using the Composition Agent",
        )
        .add_argument(
            "problem",
            "string",
            "Problem description in natural language",
            true,
        )
        .add_option(
            "model",
            "string",
            "LLM model to use (e.g. mistral-small-2503, gemini-2.5-flash). \
             Defaults to VERTEX_AI_MODEL env var or mistral-small-2503.",
            Some(serde_json::json!("mistral-small-2503")),
        )
        .add_option("dry-run", "bool", "Show the graph without executing", None)
        .add_option(
            "input",
            "string",
            "Input data as JSON string passed to the composition (default: null)",
            Some(serde_json::json!(null)),
        )
        .add_option(
            "force",
            "bool",
            "Bypass the composition cache and always call the LLM",
            None,
        )
        .with_examples(vec![
            (
                "Compose and execute a pipeline",
                "noether compose \"parse CSV and extract emails\"",
            ),
            (
                "Dry-run to inspect the graph before executing",
                "noether compose --dry-run \"sort a list of numbers\"",
            ),
            (
                "Pass input data and force re-composition",
                "noether compose --force --input '[1,3,2]' \"sort a list\"",
            ),
        ]),
    );

    tree
}
