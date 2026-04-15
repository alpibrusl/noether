//! Generic subprocess-based LLM provider.
//!
//! Covers Claude Desktop / Claude CLI, Gemini CLI, Cursor Agent, and
//! OpenCode — the four "subscription CLIs" a developer commonly has
//! logged in on a workstation. Each has its own argv shape but they
//! all share the same execution contract:
//!
//!   - spawn the binary with a fixed flag set + the prompt as argv;
//!   - stdin is closed (these tools read their prompt from `-p`);
//!   - exit 0 + non-empty stdout = success;
//!   - anything else = `LlmError::Provider` with stderr text.
//!
//! ## Why a single generic provider
//!
//! caloron-noether already implements this multi-provider fallback in
//! Python (`stages/phases/_llm.py`) and learned the hard edge cases —
//! the 25-second timeout cap to stay under Nix's default 30-second
//! kill, the `SKIP_CLI` escape hatch for sandboxed environments where
//! CLI auth isn't mounted, the exact argv incantation per tool. This
//! module ports those lessons into the Rust engine so noether-grid
//! workers get the same behaviour, for free, with the same failure
//! modes. See `docs/research/llm-here.md` for the long-term plan to
//! unify all three implementations behind one shared tool.
//!
//! ## Sandbox handling
//!
//! When `NOETHER_LLM_SKIP_CLI=1` is set, every CLI provider refuses to
//! advertise itself as available (`available() == false`). Intended
//! for stages that run inside the Nix executor, which mounts a
//! restricted `$HOME` that doesn't carry the operator's CLI auth
//! state — without this gate, a subscription CLI stalls waiting for
//! interactive login and gets SIGKILL'd by the runner.
//!
//! ## Timeout
//!
//! Default `timeout_secs = 25`. Deliberately under Nix's 30-second
//! default stage kill so a stalled CLI reports `Provider(timeout)`
//! instead of the stage runner's less useful "process killed" error.
//! Callers outside the Nix executor can bump it up via
//! `CliConfig::timeout_secs`.

use super::{LlmConfig, LlmError, LlmProvider, Message, Role};

// ── Per-CLI definitions ─────────────────────────────────────────────────────

/// A static description of one CLI tool: the binary name, the argv
/// template, and how system prompts are passed (if at all). Used by
/// [`CliProvider::new`] to pick a concrete tool.
#[derive(Debug, Clone, Copy)]
pub struct CliSpec {
    /// The executable name to look up on `PATH` (and to invoke).
    pub binary: &'static str,
    /// Provider slug the broker routes on (matches
    /// `Effect::Llm { model: "<slug>" }` exactly).
    pub provider_slug: &'static str,
    /// Default model slug the worker advertises when none is configured.
    pub default_model: &'static str,
    /// How this CLI takes its prompt on the argv.
    pub prompt_style: PromptStyle,
    /// Shape of the system-prompt flag. `None` = the CLI has no system
    /// prompt support and any system-role messages are concatenated
    /// into the user prompt.
    pub system_flag: Option<&'static str>,
}

#[derive(Debug, Clone, Copy)]
pub enum PromptStyle {
    /// `-p <prompt>` positional flag (claude, gemini, cursor-agent).
    DashP,
    /// `run <prompt>` subcommand (opencode).
    RunSubcommand,
}

/// Concrete per-CLI specs. Argv shapes are identical to what
/// caloron's `_llm.py` uses — keep in sync when either side changes.
pub mod specs {
    use super::*;

    /// Claude Desktop / Claude CLI — `claude -p PROMPT`.
    ///
    /// The `--dangerously-skip-permissions` flag is what caloron uses
    /// to bypass the interactive tool-use prompt that otherwise
    /// appears even in non-interactive mode. Without it the CLI
    /// blocks waiting for "do you want to allow this?".
    pub const CLAUDE: CliSpec = CliSpec {
        binary: "claude",
        provider_slug: "anthropic-cli",
        default_model: "claude-desktop",
        prompt_style: PromptStyle::DashP,
        system_flag: Some("--append-system-prompt"),
    };

    /// Google Gemini CLI — `gemini -y -p PROMPT`. `-y` auto-accepts
    /// the first-run consent prompt.
    pub const GEMINI: CliSpec = CliSpec {
        binary: "gemini",
        provider_slug: "google-cli",
        default_model: "gemini-desktop",
        prompt_style: PromptStyle::DashP,
        system_flag: None,
    };

    /// Cursor Agent CLI — `cursor-agent -p PROMPT --output-format text`.
    pub const CURSOR: CliSpec = CliSpec {
        binary: "cursor-agent",
        provider_slug: "cursor-cli",
        default_model: "cursor-desktop",
        prompt_style: PromptStyle::DashP,
        system_flag: None,
    };

    /// OpenCode CLI — `opencode run PROMPT`.
    pub const OPENCODE: CliSpec = CliSpec {
        binary: "opencode",
        provider_slug: "opencode",
        default_model: "opencode-default",
        prompt_style: PromptStyle::RunSubcommand,
        system_flag: None,
    };

    /// All specs, in the fallback order caloron settled on.
    pub const ALL: &[CliSpec] = &[CLAUDE, GEMINI, CURSOR, OPENCODE];
}

// ── Config ──────────────────────────────────────────────────────────────────

/// Tunables for one [`CliProvider`] instance.
#[derive(Debug, Clone)]
pub struct CliConfig {
    /// Override the binary path. Defaults to `spec.binary` (PATH lookup).
    pub binary: Option<String>,
    /// Wall-clock timeout for a single completion. Default 25s so a
    /// stalled CLI reports a timeout before Nix's 30s stage kill.
    pub timeout_secs: u64,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            binary: None,
            timeout_secs: 25,
        }
    }
}

/// Check whether CLI providers are globally suppressed. Set
/// `NOETHER_LLM_SKIP_CLI=1` inside a sandboxed environment (the Nix
/// executor being the obvious one) where subscription CLIs would
/// stall waiting for auth state that isn't mounted.
pub fn cli_providers_suppressed() -> bool {
    std::env::var("NOETHER_LLM_SKIP_CLI")
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

// ── The provider ────────────────────────────────────────────────────────────

/// LLM provider that delegates each completion to a subscription CLI.
/// Stateless — each call spawns a fresh subprocess.
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
        self.config.binary.as_deref().unwrap_or(self.spec.binary)
    }

    /// True when this CLI is installed on the host and CLI providers
    /// aren't globally suppressed. Does NOT verify auth state — we
    /// find that out at first dispatch.
    pub fn available(&self) -> bool {
        if cli_providers_suppressed() {
            return false;
        }
        binary_runs(self.binary())
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

        let (system_text, dialogue) = split_system_from_dialogue(messages);
        let prompt = compose_prompt(&dialogue, &system_text, self.spec.system_flag);

        let mut cmd = std::process::Command::new(self.binary());
        match self.spec.prompt_style {
            PromptStyle::DashP => {
                // Tool-specific extra flags — keep aligned with
                // caloron's _llm.py; if either side changes, update both.
                if self.spec.binary == "claude" {
                    cmd.arg("--dangerously-skip-permissions");
                }
                if self.spec.binary == "gemini" {
                    cmd.arg("-y");
                }
                if let (Some(flag), Some(sys)) = (self.spec.system_flag, system_text.as_ref()) {
                    cmd.arg(flag).arg(sys);
                }
                if !config.model.is_empty()
                    && config.model != self.spec.default_model
                    && config.model != "unknown"
                {
                    cmd.arg("--model").arg(&config.model);
                }
                cmd.arg("-p").arg(&prompt);
                if self.spec.binary == "cursor-agent" {
                    cmd.arg("--output-format").arg("text");
                }
            }
            PromptStyle::RunSubcommand => {
                cmd.arg("run").arg(&prompt);
            }
        }

        run_with_timeout(cmd, self.config.timeout_secs)
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

/// Final prompt string passed as the tool's last argv. Tools that can
/// carry a system prompt via flag (claude) get the dialogue only;
/// others get `SYSTEM: …\n\n` prepended so the instructions aren't
/// lost.
fn compose_prompt(
    dialogue: &[String],
    system: &Option<String>,
    system_flag: Option<&str>,
) -> String {
    let body = dialogue.join("\n\n");
    match (system, system_flag) {
        (Some(sys), None) => format!("SYSTEM: {sys}\n\n{body}"),
        _ => body,
    }
}

/// `binary --version` succeeds. Fast, cheap, doesn't need auth state.
fn binary_runs(binary: &str) -> bool {
    std::process::Command::new(binary)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_with_timeout(mut cmd: std::process::Command, timeout_secs: u64) -> Result<String, LlmError> {
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let (tx, rx) = std::sync::mpsc::channel();
    let child = std::thread::spawn(move || {
        let out = cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output();
        let _ = tx.send(out);
    });

    let out = match rx.recv_timeout(timeout) {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(LlmError::Provider(format!("CLI spawn failed: {e}"))),
        Err(_) => {
            return Err(LlmError::Provider(format!(
                "CLI exceeded {timeout_secs}s timeout"
            )))
        }
    };
    let _ = child.join();

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(LlmError::Provider(format!(
            "CLI exit {}: {}",
            out.status.code().unwrap_or(-1),
            stderr.trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err(LlmError::Provider("CLI produced empty output".into()));
    }
    Ok(stdout)
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

    fn provider_for(spec: CliSpec, binary_override: &str) -> CliProvider {
        CliProvider::with_config(
            spec,
            CliConfig {
                binary: Some(binary_override.into()),
                timeout_secs: 2,
            },
        )
    }

    #[test]
    fn missing_binary_is_not_available() {
        for spec in specs::ALL {
            let p = provider_for(*spec, "/nonexistent/never-here-xyz");
            assert!(!p.available(), "should be unavailable for {}", spec.binary);
        }
    }

    #[test]
    fn missing_binary_completion_returns_provider_error() {
        let p = provider_for(specs::CLAUDE, "/nonexistent/never-here-xyz");
        let err = p
            .complete(
                &[Message::user("hi")],
                &LlmConfig {
                    model: "claude-desktop".into(),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(matches!(err, LlmError::Provider(_)));
    }

    #[test]
    fn skip_cli_env_suppresses_all_providers() {
        let prev = std::env::var("NOETHER_LLM_SKIP_CLI").ok();
        std::env::set_var("NOETHER_LLM_SKIP_CLI", "1");
        let p = provider_for(specs::CLAUDE, "/bin/true");
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
    fn compose_prompt_inlines_system_when_no_flag() {
        let body = compose_prompt(&["USER: hello".into()], &Some("be terse".into()), None);
        assert!(body.contains("SYSTEM: be terse"));
        assert!(body.contains("USER: hello"));
    }

    #[test]
    fn compose_prompt_omits_inline_system_when_flag_exists() {
        let body = compose_prompt(
            &["USER: hi".into()],
            &Some("be terse".into()),
            Some("--append-system-prompt"),
        );
        assert!(!body.contains("SYSTEM:"));
        assert!(body.contains("USER: hi"));
    }

    #[test]
    fn all_specs_have_distinct_binaries_and_slugs() {
        let binaries: std::collections::HashSet<_> = specs::ALL.iter().map(|s| s.binary).collect();
        let slugs: std::collections::HashSet<_> =
            specs::ALL.iter().map(|s| s.provider_slug).collect();
        assert_eq!(binaries.len(), specs::ALL.len());
        assert_eq!(slugs.len(), specs::ALL.len());
    }
}
