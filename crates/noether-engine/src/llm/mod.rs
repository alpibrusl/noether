pub mod anthropic;
pub mod claude_cli;
pub mod mistral;
pub mod openai;
pub mod vertex;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("LLM provider error: {0}")]
    Provider(String),
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("response parse error: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            // mistral-small-2503: fastest + cheapest on europe-west4 ($0.05/1K calls).
            // Override with VERTEX_AI_MODEL=gemini-2.5-flash or =mistral-medium-3, etc.
            model: std::env::var("VERTEX_AI_MODEL").unwrap_or_else(|_| "mistral-small-2503".into()),
            max_tokens: 8192,
            temperature: 0.2,
        }
    }
}

/// Trait for LLM text completion.
pub trait LlmProvider: Send + Sync {
    fn complete(&self, messages: &[Message], config: &LlmConfig) -> Result<String, LlmError>;
}

/// Mock LLM provider for testing.
/// Returns the pre-configured response regardless of input.
pub struct MockLlmProvider {
    response: String,
}

impl MockLlmProvider {
    pub fn new(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
        }
    }
}

impl LlmProvider for MockLlmProvider {
    fn complete(&self, _messages: &[Message], _config: &LlmConfig) -> Result<String, LlmError> {
        Ok(self.response.clone())
    }
}

/// Mock LLM provider that returns responses from a queue.
/// When the queue is exhausted, returns the fallback response.
/// Useful for testing multi-step flows like synthesis (compose → codegen → recompose).
pub struct SequenceMockLlmProvider {
    responses: std::sync::Mutex<std::collections::VecDeque<String>>,
    fallback: String,
}

impl SequenceMockLlmProvider {
    pub fn new(responses: Vec<impl Into<String>>, fallback: impl Into<String>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into_iter().map(|s| s.into()).collect()),
            fallback: fallback.into(),
        }
    }
}

impl LlmProvider for SequenceMockLlmProvider {
    fn complete(&self, _messages: &[Message], _config: &LlmConfig) -> Result<String, LlmError> {
        let mut queue = self.responses.lock().unwrap();
        Ok(queue.pop_front().unwrap_or_else(|| self.fallback.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_returns_configured_response() {
        let provider = MockLlmProvider::new("hello world");
        let result = provider
            .complete(&[Message::user("test")], &LlmConfig::default())
            .unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn message_constructors() {
        let sys = Message::system("sys");
        assert!(matches!(sys.role, Role::System));
        let usr = Message::user("usr");
        assert!(matches!(usr.role, Role::User));
        let ast = Message::assistant("ast");
        assert!(matches!(ast.role, Role::Assistant));
    }
}
