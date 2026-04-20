//! Subscription CLI LLM provider — delegates to `llm-here-core`.
//!
//! Covers Claude Desktop / Claude CLI, Gemini CLI, Cursor Agent, and
//! OpenCode — the four subscription CLIs a developer commonly has
//! logged in on a workstation. Each `CliProvider` instance wraps one
//! of those and implements [`LlmProvider`] by composing messages into
//! a single prompt, then handing dispatch to the shared
//! `llm-here-core::dispatch` module.
//!
//! ## Why delegate
//!
//! Three sibling projects (caloron-noether, agentspec, this) used to
//! re-implement "which CLI is installed, what's the argv shape, how do
//! I timeout the subprocess." They drifted — caloron discovered the
//! 25-second-under-Nix-30-second timeout cap first, this codebase
//! backported it later, and agentspec still carried its own copy.
//! `llm-here` is the consolidation; see
//! [`docs/research/llm-here.md`](../../../docs/research/llm-here.md).
//!
//! ## What this module owns
//!
//! - The `LlmProvider` trait impl (noether-specific).
//! - `Message`/`Role` → single-prompt composition. llm-here takes a
//!   single prompt string; multi-message chat history is collapsed
//!   here into one text block with role prefixes.
//! - System-prompt routing: claude gets it as a native
//!   `--append-system-prompt` flag (via llm-here v0.4+); other CLIs
//!   get it inlined into the prompt with a `SYSTEM: …` prefix.
//! - `NOETHER_LLM_SKIP_CLI` check (belt-and-braces — llm-here also
//!   honours this as one of four aliases, but we short-circuit here
//!   to match the original behaviour exactly).
//!
//! ## What llm-here owns
//!
//! - PATH lookup for each binary.
//! - Exact argv construction (the `-p` flag, `-y` for gemini, etc.).
//! - Subprocess spawn + wall-clock timeout + child-kill on expiry.
//! - Exit-status → error translation.

use std::time::Duration;

use llm_here_core::dispatch::{run_cli_provider, DispatchOptions, RealCommandRunner};
use llm_here_core::providers::ProviderId;

use super::{LlmConfig, LlmError, LlmProvider, Message, Role};

// ── Per-CLI definitions ─────────────────────────────────────────────────────

/// A static description of one CLI tool. Each spec maps 1:1 to a
/// [`ProviderId`] in the `llm-here-core::providers::REGISTRY`.
#[derive(Debug, Clone, Copy)]
pub struct CliSpec {
    /// The executable name expected on `PATH`. Matches the binary
    /// llm-here-core looks up — keep in sync with
    /// `llm_here_core::providers::REGISTRY` entries.
    pub binary: &'static str,
    /// Provider slug the broker routes on (matches
    /// `Effect::Llm { model: "<slug>" }` exactly).
    pub provider_slug: &'static str,
    /// Default model slug the worker advertises when none is configured.
    pub default_model: &'static str,
    /// Corresponding id in the shared registry.
    pub(crate) id: ProviderId,
}

pub mod specs {
    use super::*;

    /// Claude Desktop / Claude CLI — `claude -p PROMPT`.
    pub const CLAUDE: CliSpec = CliSpec {
        binary: "claude",
        provider_slug: "anthropic-cli",
        default_model: "claude-desktop",
        id: ProviderId::ClaudeCli,
    };

    /// Google Gemini CLI — `gemini -y -p PROMPT`.
    pub const GEMINI: CliSpec = CliSpec {
        binary: "gemini",
        provider_slug: "google-cli",
        default_model: "gemini-desktop",
        id: ProviderId::GeminiCli,
    };

    /// Cursor Agent CLI — `cursor-agent -p PROMPT --output-format text`.
    pub const CURSOR: CliSpec = CliSpec {
        binary: "cursor-agent",
        provider_slug: "cursor-cli",
        default_model: "cursor-desktop",
        id: ProviderId::CursorCli,
    };

    /// OpenCode CLI — `opencode run PROMPT`.
    pub const OPENCODE: CliSpec = CliSpec {
        binary: "opencode",
        provider_slug: "opencode",
        default_model: "opencode-default",
        id: ProviderId::Opencode,
    };

    /// All specs, in the fallback order caloron settled on.
    pub const ALL: &[CliSpec] = &[CLAUDE, GEMINI, CURSOR, OPENCODE];
}

// ── Config ──────────────────────────────────────────────────────────────────

/// Tunables for one [`CliProvider`] instance.
#[derive(Debug, Clone)]
pub struct CliConfig {
    /// Wall-clock timeout for a single completion. Default 25 s so a
    /// stalled CLI reports a timeout before Nix's 30 s stage kill.
    pub timeout_secs: u64,
    /// Pass `--dangerously-skip-permissions` to `claude`. Default `true`
    /// to preserve prior behaviour (noether-grid has always set this for
    /// claude). No ambient env is read; callers can flip it off
    /// per-instance.
    pub dangerous_claude: bool,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 25,
            dangerous_claude: true,
        }
    }
}

/// Check whether CLI providers are globally suppressed. Set
/// `NOETHER_LLM_SKIP_CLI=1` inside a sandboxed environment (the Nix
/// executor being the obvious one) where subscription CLIs would
/// stall waiting for auth state that isn't mounted.
///
/// llm-here honours this env var natively (as one of four aliases),
/// but we check here too so `CliProvider::available()` short-circuits
/// before hitting the PATH lookup.
pub fn cli_providers_suppressed() -> bool {
    std::env::var("NOETHER_LLM_SKIP_CLI")
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

// ── The provider ────────────────────────────────────────────────────────────

/// LLM provider that delegates each completion to a subscription CLI
/// via `llm-here-core`. Stateless — each call spawns a fresh subprocess.
pub struct CliProvider {
    spec: CliSpec,
    config: CliConfig,
}

impl CliProvider {
    pub fn new(spec: CliSpec) -> Self {
        Self::with_config(spec, CliConfig::default())
    }

    pub fn with_config(spec: CliSpec, config: CliConfig) -> Self {
        Self { spec, config }
    }

    /// The binary this provider will invoke.
    pub fn binary(&self) -> &str {
        self.spec.binary
    }

    /// True when this CLI is installed on the host and CLI providers
    /// aren't globally suppressed. Uses llm-here's detection so the
    /// availability check is identical to what dispatch will do.
    pub fn available(&self) -> bool {
        if cli_providers_suppressed() {
            return false;
        }
        llm_here_core::detect()
            .providers
            .iter()
            .any(|p| p.id == self.spec.id.as_str())
    }

    pub fn spec(&self) -> CliSpec {
        self.spec
    }
}

impl LlmProvider for CliProvider {
    fn complete(&self, messages: &[Message], config: &LlmConfig) -> Result<String, LlmError> {
        if cli_providers_suppressed() {
            return Err(LlmError::Provider(
                "CLI providers suppressed via NOETHER_LLM_SKIP_CLI".into(),
            ));
        }

        let (system, dialogue) = split_system_from_dialogue(messages);
        let has_native_system_flag = matches!(self.spec.id, ProviderId::ClaudeCli);
        let prompt = compose_prompt(&dialogue, system.as_deref(), has_native_system_flag);

        let opts = DispatchOptions {
            timeout: Duration::from_secs(self.config.timeout_secs),
            dangerous_claude: self.config.dangerous_claude && self.spec.id == ProviderId::ClaudeCli,
            // Pass through the per-stage model when it differs from the
            // spec's default, matching the pre-migration behaviour. Treat
            // the placeholder "unknown" as "no override" (upstream callers
            // sometimes pass it as a sentinel).
            model: model_override(&config.model, self.spec.default_model),
            // Only claude consumes this today; other CLIs ignore it and
            // get the system text inlined into the main prompt above.
            system_prompt: if has_native_system_flag { system } else { None },
        };

        let report = run_cli_provider(self.spec.id, &prompt, &opts, &RealCommandRunner);

        if report.ok {
            Ok(report.text.unwrap_or_default())
        } else {
            Err(LlmError::Provider(report.error.unwrap_or_else(|| {
                "llm-here dispatch failed without an error message".into()
            })))
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn split_system_from_dialogue(messages: &[Message]) -> (Option<String>, Vec<String>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut dialogue: Vec<String> = Vec::new();
    for m in messages {
        match m.role {
            Role::System => system_parts.push(m.content.clone()),
            Role::User => dialogue.push(format!("USER: {}", m.content)),
            Role::Assistant => dialogue.push(format!("ASSISTANT: {}", m.content)),
        }
    }
    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };
    (system, dialogue)
}

/// Compose the final prompt. When the target CLI has no native
/// system-prompt flag, we inline the system text with a `SYSTEM: …`
/// prefix so instructions aren't lost. Claude CLI (the only one with a
/// native flag today) gets dialogue only.
fn compose_prompt(
    dialogue: &[String],
    system: Option<&str>,
    has_native_system_flag: bool,
) -> String {
    let body = dialogue.join("\n\n");
    match (system, has_native_system_flag) {
        (Some(sys), false) => format!("SYSTEM: {sys}\n\n{body}"),
        _ => body,
    }
}

/// Translate noether's per-config model to llm-here's optional model
/// override. Keeps the pre-migration semantics: empty / default / the
/// "unknown" sentinel all mean "no override".
fn model_override(model: &str, default: &str) -> Option<String> {
    if model.is_empty() || model == default || model == "unknown" {
        None
    } else {
        Some(model.to_string())
    }
}

// ── Back-compat shims ──────────────────────────────────────────────────────

/// Old name kept so existing call-sites still compile.
#[deprecated(note = "use CliProvider::new(specs::CLAUDE)")]
pub type ClaudeCliProvider = CliProvider;

/// Convenience constructor preserved for the call-site in providers.rs.
pub fn new_claude_cli() -> CliProvider {
    CliProvider::new(specs::CLAUDE)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_prompt_inlines_system_when_no_native_flag() {
        let body = compose_prompt(&["USER: hello".into()], Some("be terse"), false);
        assert!(body.contains("SYSTEM: be terse"));
        assert!(body.contains("USER: hello"));
    }

    #[test]
    fn compose_prompt_omits_inline_system_when_native_flag_exists() {
        let body = compose_prompt(&["USER: hi".into()], Some("be terse"), true);
        assert!(!body.contains("SYSTEM:"));
        assert!(body.contains("USER: hi"));
    }

    #[test]
    fn compose_prompt_handles_empty_system() {
        let body = compose_prompt(&["USER: hi".into()], None, false);
        assert_eq!(body, "USER: hi");
    }

    #[test]
    fn split_separates_system_and_dialogue() {
        let messages = vec![
            Message::system("be terse"),
            Message::user("hi"),
            Message::assistant("hello"),
            Message::user("bye"),
        ];
        let (system, dialogue) = split_system_from_dialogue(&messages);
        assert_eq!(system.as_deref(), Some("be terse"));
        assert_eq!(dialogue.len(), 3);
        assert!(dialogue[0].starts_with("USER: hi"));
        assert!(dialogue[1].starts_with("ASSISTANT: hello"));
        assert!(dialogue[2].starts_with("USER: bye"));
    }

    #[test]
    fn split_joins_multiple_system_messages() {
        let messages = vec![
            Message::system("rule one"),
            Message::system("rule two"),
            Message::user("hi"),
        ];
        let (system, _) = split_system_from_dialogue(&messages);
        let s = system.unwrap();
        assert!(s.contains("rule one"));
        assert!(s.contains("rule two"));
    }

    #[test]
    fn model_override_empty_or_default_returns_none() {
        assert_eq!(model_override("", "claude-desktop"), None);
        assert_eq!(model_override("claude-desktop", "claude-desktop"), None);
        assert_eq!(model_override("unknown", "claude-desktop"), None);
    }

    #[test]
    fn model_override_non_default_returns_some() {
        assert_eq!(
            model_override("claude-opus-4-1", "claude-desktop"),
            Some("claude-opus-4-1".into())
        );
    }

    #[test]
    fn skip_cli_env_suppresses_all_providers() {
        let prev = std::env::var("NOETHER_LLM_SKIP_CLI").ok();
        std::env::set_var("NOETHER_LLM_SKIP_CLI", "1");
        let p = CliProvider::new(specs::CLAUDE);
        assert!(!p.available());
        let err = p
            .complete(
                &[Message::user("hi")],
                &LlmConfig {
                    model: "claude-desktop".into(),
                    ..Default::default()
                },
            )
            .unwrap_err();
        match err {
            LlmError::Provider(m) => assert!(m.contains("suppressed")),
            _ => panic!("expected Provider(suppressed)"),
        }
        match prev {
            Some(v) => std::env::set_var("NOETHER_LLM_SKIP_CLI", v),
            None => std::env::remove_var("NOETHER_LLM_SKIP_CLI"),
        }
    }

    #[test]
    fn all_specs_have_distinct_binaries_and_slugs() {
        let binaries: std::collections::HashSet<_> = specs::ALL.iter().map(|s| s.binary).collect();
        let slugs: std::collections::HashSet<_> =
            specs::ALL.iter().map(|s| s.provider_slug).collect();
        assert_eq!(binaries.len(), specs::ALL.len());
        assert_eq!(slugs.len(), specs::ALL.len());
    }

    #[test]
    fn each_spec_maps_to_a_distinct_provider_id() {
        let ids: std::collections::HashSet<_> = specs::ALL.iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), specs::ALL.len());
    }
}
