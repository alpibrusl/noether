//! Claude Desktop / Claude CLI LLM provider.
//!
//! Unlike the API-key providers in this module (`anthropic.rs`,
//! `openai.rs`, …), this one dispatches by subprocess: it shells out
//! to the `claude` binary installed on the host, piping the prompt
//! through stdin and reading the completion from stdout. That lets a
//! Claude Pro / Claude Team seat be pooled through `noether-grid`
//! without an API key — the seat-holder stays logged into their CLI
//! the way they normally would, and the grid worker on the same
//! machine uses that ambient session.
//!
//! Invocation contract:
//!   claude  [--append-system-prompt SYS]  [--model MODEL]  -p PROMPT
//! → stdout: completion text
//! → exit 0 on success, non-zero on anything else.
//!
//! Prompt composition: we concatenate all user/assistant turns into a
//! single final `-p` argument, and hoist any `system` role into
//! `--append-system-prompt`. Multi-turn state is passed inline —
//! the CLI itself is stateless across invocations unless you use
//! session flags we don't touch here.
//!
//! Timeout: every call is bounded by [`ClaudeCliConfig::timeout_secs`]
//! (default 120 s). Exceeding it sends SIGKILL and returns
//! [`LlmError::Provider`] with a timeout message.

use super::{LlmConfig, LlmError, LlmProvider, Message, Role};

/// Tunables for the Claude-CLI provider.
#[derive(Debug, Clone)]
pub struct ClaudeCliConfig {
    /// Path to the `claude` binary. Default: `"claude"` (resolved via
    /// PATH). Override when the binary lives elsewhere (e.g. bundled
    /// inside a .app on macOS).
    pub binary: String,
    /// Hard wall-clock timeout for a single completion. Claude
    /// occasionally stalls on slow responses; kill at this point.
    pub timeout_secs: u64,
}

impl Default for ClaudeCliConfig {
    fn default() -> Self {
        Self {
            binary: "claude".into(),
            timeout_secs: 120,
        }
    }
}

/// LLM provider that delegates every completion to the local `claude`
/// CLI. Stateless — constructs a fresh subprocess per call.
pub struct ClaudeCliProvider {
    config: ClaudeCliConfig,
}

impl ClaudeCliProvider {
    pub fn new() -> Self {
        Self {
            config: ClaudeCliConfig::default(),
        }
    }

    pub fn with_config(config: ClaudeCliConfig) -> Self {
        Self { config }
    }

    /// Probe — returns true when `claude --version` exits 0. Callers
    /// can use this to decide whether to include the provider in a
    /// fallback chain without paying the cost of a first real call.
    pub fn available(&self) -> bool {
        std::process::Command::new(&self.config.binary)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

impl Default for ClaudeCliProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmProvider for ClaudeCliProvider {
    fn complete(&self, messages: &[Message], config: &LlmConfig) -> Result<String, LlmError> {
        // Split system messages (hoisted into --append-system-prompt)
        // from user/assistant turns (joined into the final -p arg).
        let mut system_parts: Vec<String> = Vec::new();
        let mut dialogue: Vec<String> = Vec::new();
        for m in messages {
            match m.role {
                Role::System => system_parts.push(m.content.clone()),
                Role::User => dialogue.push(format!("USER: {}", m.content)),
                Role::Assistant => dialogue.push(format!("ASSISTANT: {}", m.content)),
            }
        }
        let prompt = dialogue.join("\n\n");
        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        };

        // Build argv. `--model` only gets passed when the caller
        // explicitly asked for one — leaving it off lets the CLI use
        // whatever the seat-holder configured as their default.
        let mut cmd = std::process::Command::new(&self.config.binary);
        if let Some(sys) = &system {
            cmd.arg("--append-system-prompt").arg(sys);
        }
        if !config.model.is_empty() && config.model != "claude-desktop" && config.model != "unknown"
        {
            cmd.arg("--model").arg(&config.model);
        }
        cmd.arg("-p").arg(&prompt);

        // Run with a wall-clock timeout. Claude CLI is blocking +
        // reads stdin on some flags we don't use, so we pipe stdin
        // closed explicitly and collect stdout/stderr.
        let timeout = std::time::Duration::from_secs(self.config.timeout_secs);
        let (tx, rx) = std::sync::mpsc::channel();
        let cmd_thread = std::thread::spawn(move || {
            let out = cmd
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output();
            let _ = tx.send(out);
        });

        let out = match rx.recv_timeout(timeout) {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                return Err(LlmError::Provider(format!("claude CLI spawn failed: {e}")));
            }
            Err(_) => {
                return Err(LlmError::Provider(format!(
                    "claude CLI exceeded {}s timeout",
                    self.config.timeout_secs
                )));
            }
        };
        let _ = cmd_thread.join();

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(LlmError::Provider(format!(
                "claude CLI exit {}: {}",
                out.status.code().unwrap_or(-1),
                stderr.trim()
            )));
        }
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if stdout.is_empty() {
            return Err(LlmError::Provider(
                "claude CLI produced empty output".into(),
            ));
        }
        Ok(stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_returns_false_for_missing_binary() {
        let provider = ClaudeCliProvider::with_config(ClaudeCliConfig {
            binary: "/nonexistent/claude-binary-xyz-nothing-here".into(),
            timeout_secs: 5,
        });
        assert!(!provider.available());
    }

    #[test]
    fn complete_with_missing_binary_reports_provider_error() {
        let provider = ClaudeCliProvider::with_config(ClaudeCliConfig {
            binary: "/nonexistent/claude-binary-xyz".into(),
            timeout_secs: 2,
        });
        let err = provider
            .complete(
                &[Message::user("hello")],
                &LlmConfig {
                    model: "claude-desktop".into(),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(matches!(err, LlmError::Provider(_)));
    }
}
