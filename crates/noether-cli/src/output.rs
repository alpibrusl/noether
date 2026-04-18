pub use acli::{CommandInfo, CommandTree, ExitCode};

use acli::output::CacheMeta;
use acli::{error_envelope, success_envelope};
use serde_json::Value;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Format an ACLI success envelope as JSON string.
pub fn acli_ok(data: Value) -> String {
    let envelope = success_envelope("noether", data, VERSION, None, None);
    serde_json::to_string_pretty(&envelope).unwrap()
}

/// Format an ACLI success envelope with cache metadata as JSON string.
pub fn acli_ok_cached(data: Value, cache: CacheMeta) -> String {
    let envelope = success_envelope("noether", data, VERSION, None, Some(cache));
    serde_json::to_string_pretty(&envelope).unwrap()
}

/// Format an ACLI error envelope as JSON string.
pub fn acli_error(message: &str) -> String {
    acli_error_hints(message, None, None)
}

/// Format an ACLI error envelope with an optional single hint as JSON string.
pub fn acli_error_hint(message: &str, hint: Option<&str>) -> String {
    acli_error_hints(message, hint, None)
}

/// Format an ACLI error envelope with an optional hint and structured hints list.
pub fn acli_error_hints(message: &str, hint: Option<&str>, hints: Option<Vec<String>>) -> String {
    let envelope = error_envelope(
        "noether",
        ExitCode::GeneralError,
        message,
        hint,
        hints,
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

    let stage_activate = CommandInfo::new("activate", "Promote a Draft stage to Active")
        .add_argument("hash", "string", "The stage hash or prefix", true);

    let stage_test = CommandInfo::new(
        "test",
        "Verify a stage's implementation against its declared examples",
    )
    .add_argument(
        "hash",
        "string",
        "Optional stage hash or prefix; omit to test every Active stage",
        false,
    )
    .idempotent(true);

    let stage_verify = CommandInfo::new(
        "verify",
        "Verify a stage's Ed25519 signature and its declarative properties against examples",
    )
    .add_argument(
        "hash",
        "string",
        "Optional stage hash or prefix; omit to verify every Active stage",
        false,
    )
    .add_option(
        "signatures",
        "bool",
        "Check Ed25519 signatures only (default: check both)",
        None,
    )
    .add_option(
        "properties",
        "bool",
        "Check declarative properties only (default: check both)",
        None,
    )
    .idempotent(true);

    let stage_sync = CommandInfo::new(
        "sync",
        "Bulk-import all *.json stage specs from a directory",
    )
    .add_argument("directory", "path", "Directory of stage spec JSONs", true);

    let mut stage_cmd = CommandInfo::new("stage", "Stage management commands");
    stage_cmd.subcommands = vec![
        stage_search,
        stage_add,
        stage_sync,
        stage_list,
        stage_get,
        stage_activate,
        stage_test,
        stage_verify,
    ];
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
        (
            "Preview duplicates without changes",
            "noether store retro --dry-run",
        ),
        (
            "Apply suggested deprecations",
            "noether store retro --apply",
        ),
    ]);
    let store_migrate = CommandInfo::new(
        "migrate-effects",
        "Infer and apply effects for stages currently marked Unknown",
    )
    .add_option(
        "dry-run",
        "bool",
        "Show the migration plan without applying changes",
        None,
    )
    .idempotent(false)
    .with_examples(vec![
        (
            "Preview what would be inferred",
            "noether store migrate-effects --dry-run",
        ),
        ("Apply inferred effects", "noether store migrate-effects"),
    ]);
    let mut store_cmd = CommandInfo::new("store", "Store management commands");
    store_cmd.subcommands = vec![store_stats, store_retro, store_migrate];
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
            .add_option_with_version(
                "allow-capabilities",
                "string",
                "Comma-separated capabilities to grant (e.g. network,fs-read). Default: all allowed.",
                Some(serde_json::json!(null)),
                Some("0.1.0"),
                None,
            )
            .add_option(
                "budget-cents",
                "number",
                "Reject compositions whose estimated cost exceeds this value in cents.",
                None,
            )
            .with_examples(vec![
                ("Execute a graph", "noether run graph.json"),
                ("Dry-run only", "noether run --dry-run graph.json"),
                ("Pass input data", "noether run --input '{\"key\":\"value\"}' graph.json"),
                ("Restrict to network only", "noether run --allow-capabilities network graph.json"),
                ("Cap cost at 10 cents", "noether run --budget-cents 10 graph.json"),
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
        .add_option_with_version(
            "force",
            "bool",
            "Bypass the composition cache and always call the LLM",
            None,
            Some("0.1.0"),
            None,
        )
        .add_option_with_version(
            "allow-capabilities",
            "string",
            "Comma-separated capabilities to grant (e.g. network,fs-read). Default: all allowed.",
            Some(serde_json::json!(null)),
            Some("0.1.0"),
            None,
        )
        .add_option(
            "budget-cents",
            "number",
            "Reject compositions whose estimated cost exceeds this value in cents.",
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

    // Build command
    tree.add_command(
        CommandInfo::new(
            "build",
            "Compile a composition graph into a self-contained standalone binary",
        )
        .add_argument(
            "graph",
            "path",
            "Path to the Lagrange graph JSON file",
            true,
        )
        .add_option(
            "output",
            "path",
            "Output binary path (default: ./noether-app)",
            Some(serde_json::json!("./noether-app")),
        )
        .add_option(
            "name",
            "string",
            "Override the binary name used in ACLI output and --help",
            None,
        )
        .add_option(
            "description",
            "string",
            "One-line description shown in the binary's --help",
            None,
        )
        .add_option(
            "serve",
            "string",
            "Address to bind when running the built binary as an HTTP microservice (e.g. :8080)",
            None,
        )
        .with_examples(vec![
            (
                "Build a rail-search binary from a graph",
                "noether build rail-search.json -o ./rail-search",
            ),
            (
                "Build with a custom name and description",
                "noether build graph.json -o ./my-app --name my-app --description 'Sorts a list'",
            ),
            (
                "Run the built binary as an HTTP microservice",
                "./rail-search --serve :8080",
            ),
        ]),
    );

    tree
}
