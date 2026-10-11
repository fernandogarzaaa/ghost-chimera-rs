//! Anthropic Messages API provider.
//!
//! Mirrors `model_layer/providers.py::AnthropicProvider`.

use std::time::Duration;

use serde_json::{json, Value};

use crate::error::ProviderError;
use crate::traits::ModelProvider;

const API_VERSION: &str = "2023-06-01";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1/messages";
const DEFAULT_MODEL: &str = "claude-3-5-haiku-20241022";
const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Claude via the Anthropic Messages API.
pub struct AnthropicProvider {
    api_key: String,
    model: String,
    base_url: String,
    client: reqwest::blocking::Client,
}

impl AnthropicProvider {
    /// Build from the environment (`ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL`).
    pub fn from_env() -> Self {
        let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
        let model = std::env::var("ANTHROPIC_MODEL")
            .ok()
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Self::with_credentials(api_key, model, None)
    }

    /// Build with explicit credentials (used by tests and custom configs).
    pub fn with_credentials(
        api_key: String,
        model: String,
        base_url_override: Option<String>,
    ) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client builds with rustls");
        let base_url = base_url_override
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        Self {
            api_key,
            model,
            base_url,
            client,
        }
    }

    /// The request body, exposed for tests.
    pub fn request_body(&self, system: &str, user: &str) -> Value {
        json!({
            "model": self.model,
            "max_tokens": DEFAULT_MAX_TOKENS,
            "system": system,
            "messages": [{"role": "user", "content": user}],
        })
    }

    /// Extract the assistant text from a Messages API response body.
    pub fn parse_response(body: &Value) -> Result<String, ProviderError> {
        let text = body
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .ok_or_else(|| ProviderError::InvalidResponse("missing content[0].text".to_string()))?;
        Ok(text.trim().to_string())
    }
}

impl ModelProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn chat(&self, system: &str, user: &str) -> Result<String, ProviderError> {
        if !self.is_available() {
            return Err(ProviderError::Unavailable(
                "anthropic is not available; set ANTHROPIC_API_KEY in the environment".to_string(),
            ));
        }
        let resp = self
            .client
            .post(&self.base_url)
            .header("Content-Type", "application/json")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&self.request_body(system, user))
            .send()?
            .error_for_status()?;
        let body: Value = resp.json()?;
        Self::parse_response(&body)
    }

    fn validate_config(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.api_key.is_empty() {
            errors.push("ANTHROPIC_API_KEY is not set".to_string());
        }
        if self.model.is_empty() {
            errors.push("ANTHROPIC_MODEL must be non-empty".to_string());
        }
        errors
    }

    fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn one_shot_server(response_body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).expect("read request");
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            let _ = stream.write_all(reply.as_bytes());
        });
        (format!("http://{addr}/v1/messages"), rx)
    }

    #[test]
    fn chat_round_trip_against_local_server() {
        let response = r#"{"content":[{"type":"text","text":"  hello from claude  "}]}"#;
        let (url, recorded) = one_shot_server(response);
        let p = AnthropicProvider::with_credentials("k".to_string(), "m".to_string(), Some(url));
        let reply = p.chat("sys", "hi").expect("chat succeeds");
        assert_eq!(reply, "hello from claude");

        let raw = recorded
            .recv_timeout(Duration::from_secs(5))
            .expect("request recorded");
        assert!(raw.starts_with("POST /v1/messages"), "got: {raw}");
        assert!(raw.contains("x-api-key: k"), "got: {raw}");
        assert!(raw.contains("anthropic-version: 2023-06-01"), "got: {raw}");
        assert!(raw.contains(r#""max_tokens":1024"#), "got: {raw}");
    }

    #[test]
    fn chat_fails_without_api_key() {
        let p = AnthropicProvider::with_credentials(String::new(), "m".to_string(), None);
        assert!(!p.is_available());
        let err = p.chat("s", "u").expect_err("must fail");
        assert!(matches!(err, ProviderError::Unavailable(_)));
        assert_eq!(
            p.validate_config(),
            vec!["ANTHROPIC_API_KEY is not set".to_string()]
        );
    }

    #[test]
    fn parse_response_rejects_malformed_body() {
        assert!(AnthropicProvider::parse_response(&json!({"nope": 1})).is_err());
        assert!(AnthropicProvider::parse_response(&json!({"content": []})).is_err());
    }
}
