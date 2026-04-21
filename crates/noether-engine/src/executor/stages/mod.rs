#[cfg(feature = "native")]
pub mod arrow;
pub mod collections;
pub mod control;
pub mod data;
pub mod generic;
#[cfg(feature = "native")]
pub mod io;
#[cfg(feature = "native")]
pub mod kv;
#[cfg(feature = "native")]
pub mod process;
pub mod scalar;
pub mod text;
pub mod ui;
pub mod validation;

use super::{ExecutionError, StageExecutor};
use noether_core::stage::StageId;
use serde_json::Value;

/// A stage implementation function.
pub type StageFn = fn(&Value) -> Result<Value, ExecutionError>;

/// Find the implementation for a stage by matching its description.
/// Returns None for stages without real implementations (LLM, Arrow, internal).
pub fn find_implementation(description: &str) -> Option<StageFn> {
    match description {
        // Generic (polymorphic — M3 slice 3)
        "Return the input unchanged. Polymorphic: <T> -> <T>." => Some(generic::identity),
        "Return the first element of a list. Empty list is a Fallible error." => Some(generic::head),
        "Return every element of a list except the first. Empty list -> empty list." => {
            Some(generic::tail)
        }

        // Scalar
        "Convert any value to its text representation" => Some(scalar::to_text),
        "Parse a value as a number; fails on non-numeric text" => Some(scalar::to_number),
        "Convert a value to boolean using truthiness rules" => Some(scalar::to_bool),
        "Parse a JSON string into a structured value" => Some(scalar::parse_json),
        "Serialize any value to a JSON string" => Some(scalar::to_json),

        // Text
        "Split text by a delimiter into a list of strings" => Some(text::text_split),
        "Join a list of strings with a delimiter" => Some(text::text_join),
        "Match text against a regex pattern; fails on invalid regex" => Some(text::regex_match),
        "Replace regex matches in text; fails on invalid regex" => Some(text::regex_replace),
        "Interpolate variables into a template string using {{key}} syntax" => {
            Some(text::text_template)
        }
        "Compute a cryptographic hash of text; defaults to SHA-256" => Some(text::text_hash),
        "Convert text to uppercase" => Some(text::text_upper),
        "Convert text to lowercase" => Some(text::text_lower),
        "Remove leading and trailing whitespace from text" => Some(text::text_trim),
        "Return the number of characters in a text string" => Some(text::text_length),
        "Check if text contains a substring; case-sensitive" => Some(text::text_contains),
        "Reverse the characters in a text string" => Some(text::text_reverse),
        "Replace all literal occurrences of a substring in text" => Some(text::text_replace),

        // Collections
        "Sort a list; optionally by a field name and/or in descending order" => {
            Some(collections::sort)
        }
        "Flatten a list of lists into a single list" => Some(collections::flatten),
        "Combine two lists into a list of pairs, truncating to the shorter list" => {
            Some(collections::zip)
        }
        "Take the first N elements from a list" => Some(collections::take),
        "Group list items by the value of a named field" => Some(collections::group_by),
        "Sum all numbers in a list" => Some(collections::num_sum),
        "Compute the arithmetic mean of a list of numbers; fails on empty list" => Some(collections::num_avg),
        "Return the minimum value in a list of numbers; fails on empty list" => Some(collections::num_min),
        "Return the maximum value in a list of numbers; fails on empty list" => Some(collections::num_max),
        "Remove duplicate values from a list, preserving first-occurrence order" => Some(collections::list_dedup),
        "Return the number of elements in a list" => Some(collections::list_length),

        // Data
        "Deep merge two JSON values; patch values override base" => Some(data::json_merge),
        "Extract a value from JSON data using a JSONPath expression" => Some(data::json_path),
        "Validate data against a JSON schema; returns validation results" => {
            Some(data::json_schema_validate)
        }

        // Control (pure / stateless)
        "Select between two values based on a boolean condition" => Some(control::branch),
        "Check if one type is a structural subtype of another" => Some(control::is_subtype),

        // UI
        "Route a path to a VNode: return routes[route] or routes[default]" => {
            Some(ui::router)
        }

        // I/O (native only: requires reqwest + std::fs)
        #[cfg(feature = "native")]
        "Read a file's contents as text" => Some(io::read_file),
        #[cfg(feature = "native")]
        "Write text content to a file" => Some(io::write_file),
        #[cfg(feature = "native")]
        "Write text to standard output" => Some(io::stdout_write),
        #[cfg(feature = "native")]
        "Read all available text from standard input" => Some(io::stdin_read),
        #[cfg(feature = "native")]
        "Read an environment variable; returns null if not set" => Some(io::env_get),
        #[cfg(feature = "native")]
        "Make an HTTP GET request" => Some(io::http_get),
        #[cfg(feature = "native")]
        "Make an HTTP POST request" => Some(io::http_post),
        #[cfg(feature = "native")]
        "Make an HTTP PUT request" => Some(io::http_put),
        #[cfg(feature = "native")]
        "Extract the body text from an HTTP response record" => Some(io::http_body),
        #[cfg(feature = "native")]
        "Extract the status code from an HTTP response record" => Some(io::http_status),

        // KV store (native only: requires rusqlite)
        #[cfg(feature = "native")]
        "Store a JSON value under a key in the persistent key-value store; returns \"ok\"" => Some(kv::kv_set),
        #[cfg(feature = "native")]
        "Retrieve a JSON value by key from the persistent key-value store; returns null if not found" => Some(kv::kv_get),
        #[cfg(feature = "native")]
        "Delete a key from the persistent key-value store; returns true if the key existed" => Some(kv::kv_delete),
        #[cfg(feature = "native")]
        "Check whether a key exists in the persistent key-value store" => Some(kv::kv_exists),
        #[cfg(feature = "native")]
        "List all keys in the persistent key-value store that start with a given prefix" => Some(kv::kv_list),

        // Validation pipeline (Rust-native, no Nix)
        "Verify that a stage's content hash matches its declared ID" => {
            Some(validation::verify_stage_content_hash)
        }
        "Verify the Ed25519 signature of a stage, if present" => {
            Some(validation::verify_stage_ed25519)
        }
        "Check that a stage description is non-empty" => {
            Some(validation::check_stage_description)
        }
        "Check that a stage has at least one example" => {
            Some(validation::check_stage_examples)
        }
        "Aggregate stage validation check results into a report" => {
            Some(validation::merge_validation_checks)
        }

        // Arrow IPC (native only: requires arrow + base64)
        #[cfg(feature = "native")]
        "Convert a list of records to Apache Arrow IPC bytes" => Some(arrow::arrow_from_records),
        #[cfg(feature = "native")]
        "Decode Apache Arrow IPC bytes to a list of record maps" => Some(arrow::records_to_arrow),

        // Process management (native only: spawns OS subprocesses)
        #[cfg(feature = "native")]
        "Spawn a subprocess; returns its PID and Unix start timestamp" => {
            Some(process::spawn_process)
        }
        #[cfg(feature = "native")]
        "Poll until a process exits or timeout_ms elapses; default timeout 30 s" => {
            Some(process::wait_process)
        }
        #[cfg(feature = "native")]
        "Send a Unix signal to a process (TERM by default); returns whether the signal was delivered" => {
            Some(process::signal_process)
        }
        #[cfg(feature = "native")]
        "Send SIGKILL to a process; returns whether the signal was delivered" => {
            Some(process::kill_process)
        }

        _ => None,
    }
}

/// HOF and control stages that need access to the executor itself.
/// Returns true if the stage should be routed through execute_hof_extended.
pub fn is_executor_stage(description: &str) -> bool {
    matches!(
        description,
        "Try stages in order until one succeeds; fails if all fail"
            | "Run N stages concurrently on N inputs; collect all results"
            | "Retry a fallible stage up to N times with optional delay between attempts"
            | "Run a stage with a deadline; fails if the stage exceeds the timeout"
            | "Run multiple stages concurrently; return the first to complete"
    )
}

pub fn execute_executor_stage<E: StageExecutor>(
    executor: &E,
    description: &str,
    input: &Value,
) -> Result<Value, ExecutionError> {
    match description {
        "Try stages in order until one succeeds; fails if all fail" => {
            control::fallback(executor, input)
        }
        "Run N stages concurrently on N inputs; collect all results" => {
            control::parallel_n(executor, input)
        }
        "Retry a fallible stage up to N times with optional delay between attempts" => {
            control::retry_hof(executor, input)
        }
        "Run a stage with a deadline; fails if the stage exceeds the timeout" => {
            control::timeout_hof(executor, input)
        }
        "Run multiple stages concurrently; return the first to complete" => {
            control::race_hof(executor, input)
        }
        _ => Err(ExecutionError::StageNotFound(StageId("unknown".into()))),
    }
}
