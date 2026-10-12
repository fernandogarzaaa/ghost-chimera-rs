//! One `reqwest`-based client covering every OpenAI-compatible provider.
//!
//! Mirrors `model_layer/openai_compatible_providers.py`: the base class plus
//! its 23 subclasses collapse into one struct parameterized by a static
//! provider table (name, chat-completions URL, key env var, default model).

use std::time::Duration;

use serde_json::{json, Value};

use crate::error::ProviderError;
use crate::traits::ModelProvider;

/// One row of the provider table.
#[derive(Debug, Clone, Copy)]
pub struct ProviderSpec {
    /// Provider id, e.g. `"groq"`.
    pub name: &'static str,
    /// Full chat-completions endpoint URL.
    pub base_url: &'static str,
    /// Environment variable holding the API key.
    pub key_env: &'static str,
    /// Default model when no override is configured.
    pub default_model: &'static str,
    /// Environment variable overriding the model.
    pub model_env: &'static str,
}

/// The 24 OpenAI-compatible providers from
/// `model_layer/openai_compatible_providers.py`.
pub const PROVIDER_SPECS: &[ProviderSpec] = &[
    ProviderSpec {
        name: "openai",
        base_url: "https://api.openai.com/v1/chat/completions",
        key_env: "OPENAI_API_KEY",
        default_model: "gpt-4o-mini",
        model_env: "OPENAI_MODEL",
    },
    ProviderSpec {
        name: "groq",
        base_url: "https://api.groq.com/openai/v1/chat/completions",
        key_env: "GROQ_API_KEY",
        default_model: "llama-3.3-70b-versatile",
        model_env: "GROQ_MODEL",
    },
    ProviderSpec {
        name: "xai",
        base_url: "https://api.x.ai/v1/chat/completions",
        key_env: "XAI_API_KEY",
        default_model: "grok-3-mini",
        model_env: "XAI_MODEL",
    },
    ProviderSpec {
        name: "mistral",
        base_url: "https://api.mistral.ai/v1/chat/completions",
        key_env: "MISTRAL_API_KEY",
        default_model: "mistral-small-latest",
        model_env: "MISTRAL_MODEL",
    },
    ProviderSpec {
        name: "deepseek",
        base_url: "https://api.deepseek.com/v1/chat/completions",
        key_env: "DEEPSEEK_API_KEY",
        default_model: "deepseek-chat",
        model_env: "DEEPSEEK_MODEL",
    },
    ProviderSpec {
        name: "together",
        base_url: "https://api.together.xyz/v1/chat/completions",
        key_env: "TOGETHER_API_KEY",
        default_model: "meta-llama/Llama-3-70b-chat-hf",
        model_env: "TOGETHER_MODEL",
    },
    ProviderSpec {
        name: "openrouter",
        base_url: "https://openrouter.ai/api/v1/chat/completions",
        key_env: "OPENROUTER_API_KEY",
        default_model: "openai/gpt-4o-mini",
        model_env: "OPENROUTER_MODEL",
    },
    ProviderSpec {
        name: "perplexity",
        base_url: "https://api.perplexity.ai/chat/completions",
        key_env: "PERPLEXITY_API_KEY",
        default_model: "llama-3.1-sonar-small-128k-online",
        model_env: "PERPLEXITY_MODEL",
    },
    ProviderSpec {
        name: "fireworks",
        base_url: "https://api.fireworks.ai/inference/v1/chat/completions",
        key_env: "FIREWORKS_API_KEY",
        default_model: "accounts/fireworks/models/llama-v3p1-70b-instruct",
        model_env: "FIREWORKS_MODEL",
    },
    ProviderSpec {
        name: "cerebras",
        base_url: "https://api.cerebras.ai/v1/chat/completions",
        key_env: "CEREBRAS_API_KEY",
        default_model: "llama3.1-70b",
        model_env: "CEREBRAS_MODEL",
    },
    ProviderSpec {
        name: "ai21",
        base_url: "https://api.ai21.com/studio/v1/chat/completions",
        key_env: "AI21_API_KEY",
        default_model: "jamba-1.5-mini",
        model_env: "AI21_MODEL",
    },
    ProviderSpec {
        name: "huggingface",
        base_url: "https://router.huggingface.co/v1/chat/completions",
        key_env: "HF_TOKEN",
        default_model: "meta-llama/Llama-3.3-70B-Instruct",
        model_env: "HF_MODEL",
    },
    ProviderSpec {
        name: "nvidia",
        base_url: "https://integrate.api.nvidia.com/v1/chat/completions",
        key_env: "NVIDIA_API_KEY",
        default_model: "meta/llama-3.1-70b-instruct",
        model_env: "NVIDIA_MODEL",
    },
    ProviderSpec {
        name: "nebius",
        base_url: "https://api.tokenfactory.nebius.com/v1/chat/completions",
        key_env: "NEBIUS_API_KEY",
        default_model: "nvidia/NVIDIA-Nemotron-3-Nano-30B-A3B",
        model_env: "NEBIUS_MODEL",
    },
    ProviderSpec {
        name: "moonshot",
        base_url: "https://api.moonshot.cn/v1/chat/completions",
        key_env: "MOONSHOT_API_KEY",
        default_model: "moonshot-v1-8k",
        model_env: "MOONSHOT_MODEL",
    },
    ProviderSpec {
        name: "deepinfra",
        base_url: "https://api.deepinfra.com/v1/openai/chat/completions",
        key_env: "DEEPINFRA_API_KEY",
        default_model: "meta-llama/Meta-Llama-3.1-70B-Instruct",
        model_env: "DEEPINFRA_MODEL",
    },
    ProviderSpec {
        name: "qwen",
        base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions",
        key_env: "DASHSCOPE_API_KEY",
        default_model: "qwen-turbo",
        model_env: "QWEN_MODEL",
    },
    ProviderSpec {
        name: "volcengine",
        base_url: "https://ark.cn-beijing.volces.com/api/v3/chat/completions",
        key_env: "ARK_API_KEY",
        default_model: "doubao-pro-4k",
        model_env: "VOLCENGINE_MODEL",
    },
    ProviderSpec {
        name: "stepfun",
        base_url: "https://api.stepfun.com/v1/chat/completions",
        key_env: "STEPFUN_API_KEY",
        default_model: "step-1-8k",
        model_env: "STEPFUN_MODEL",
    },
    ProviderSpec {
        name: "glm",
        base_url: "https://open.bigmodel.cn/api/paas/v4/chat/completions",
        key_env: "ZHIPUAI_API_KEY",
        default_model: "glm-4-flash",
        model_env: "GLM_MODEL",
    },
    ProviderSpec {
        name: "venice",
        base_url: "https://api.venice.ai/api/v1/chat/completions",
        key_env: "VENICE_API_KEY",
        default_model: "llama-3.3-70b",
        model_env: "VENICE_MODEL",
    },
    ProviderSpec {
        name: "vultr",
        base_url: "",
        key_env: "VULTR_INFERENCE_API_KEY",
        default_model: "",
        model_env: "VULTR_MODEL",
    },
    ProviderSpec {
        name: "gemini-openai",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
        key_env: "GOOGLE_API_KEY",
        default_model: "gemini-2.5-flash-lite",
        model_env: "GEMINI_MODEL",
    },
    ProviderSpec {
        name: "cloudflare",
        base_url: "",
        key_env: "CF_API_TOKEN",
        default_model: "@cf/meta/llama-3.1-8b-instruct",
        model_env: "CF_MODEL",
    },
];

/// A chat-completions client for any OpenAI-compatible endpoint.
pub struct OpenAiCompatibleProvider {
    spec: ProviderSpec,
    api_key: String,
    model: String,
    base_url_override: Option<String>,
    client: reqwest::blocking::Client,
}

impl OpenAiCompatibleProvider {
    /// Build from a provider table entry, reading key/model from the environment.
    pub fn from_spec(spec: ProviderSpec) -> Self {
        let api_key = std::env::var(spec.key_env).unwrap_or_default();
        let model = std::env::var(spec.model_env)
            .ok()
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| spec.default_model.to_string());
        Self::with_credentials(spec, api_key, model, None)
    }

    /// Build with explicit credentials (used by tests and custom configs).
    pub fn with_credentials(
        spec: ProviderSpec,
        api_key: String,
        model: String,
        base_url_override: Option<String>,
    ) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client builds with rustls");
        Self {
            spec,
            api_key,
            model,
            base_url_override,
            client,
        }
    }

    /// Look up a spec by provider name.
    pub fn spec_for(name: &str) -> Option<ProviderSpec> {
        PROVIDER_SPECS.iter().find(|s| s.name == name).copied()
    }

    fn endpoint(&self) -> String {
        self.base_url_override
            .clone()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| self.spec.base_url.to_string())
    }

    /// The request body, exposed for tests.
    pub fn request_body(&self, system: &str, user: &str) -> Value {
        json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        })
    }

    /// Extract the assistant text from a chat-completions response body.
    pub fn parse_response(body: &Value) -> Result<String, ProviderError> {
        let text = body
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ProviderError::InvalidResponse("missing choices[0].message.content".to_string())
            })?;
        Ok(text.trim().to_string())
    }
}

impl ModelProvider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        self.spec.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn chat(&self, system: &str, user: &str) -> Result<String, ProviderError> {
        if !self.is_available() {
            return Err(ProviderError::Unavailable(format!(
                "{} is not available; set {} in the environment",
                self.spec.name, self.spec.key_env
            )));
        }
        let endpoint = self.endpoint();
        if endpoint.is_empty() {
            return Err(ProviderError::Unavailable(format!(
                "{} needs a base URL configured (its default is empty)",
                self.spec.name
            )));
        }
        let resp = self
            .client
            .post(&endpoint)
            .bearer_auth(&self.api_key)
            .json(&self.request_body(system, user))
            .send()?
            .error_for_status()?;
        let body: Value = resp.json()?;
        Self::parse_response(&body)
    }

    fn validate_config(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.api_key.is_empty() {
            errors.push(format!("{} is not set", self.spec.key_env));
        }
        if self.model.is_empty() {
            errors.push(format!("{} must be non-empty", self.spec.model_env));
        }
        if self.endpoint().is_empty() {
            errors.push(format!("{} has no base URL configured", self.spec.name));
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

    fn test_spec() -> ProviderSpec {
        ProviderSpec {
            name: "test-provider",
            base_url: "",
            key_env: "TEST_PROVIDER_KEY",
            default_model: "test-model",
            model_env: "TEST_PROVIDER_MODEL",
        }
    }

    /// Spin up a one-shot HTTP server on loopback that records the request
    /// and replies with `response_body`. Returns (url, recorded_request).
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
        (format!("http://{addr}/v1/chat/completions"), rx)
    }

    #[test]
    fn model_reports_the_configured_model() {
        let spec = OpenAiCompatibleProvider::spec_for("groq").expect("groq exists");
        let provider = OpenAiCompatibleProvider::with_credentials(
            spec,
            "k".to_string(),
            "groq-test-model".to_string(),
            None,
        );
        assert_eq!(provider.model(), "groq-test-model");
    }

    #[test]
    fn provider_table_covers_24_entries() {
        assert_eq!(PROVIDER_SPECS.len(), 24);
        for spec in PROVIDER_SPECS {
            assert!(!spec.name.is_empty(), "provider name must not be empty");
            assert!(!spec.key_env.is_empty(), "key env must not be empty");
        }
    }

    #[test]
    fn spec_lookup_by_name() {
        let spec = OpenAiCompatibleProvider::spec_for("nebius").expect("nebius exists");
        assert_eq!(spec.key_env, "NEBIUS_API_KEY");
        assert!(spec.base_url.contains("nebius"));
        assert!(OpenAiCompatibleProvider::spec_for("no-such-provider").is_none());
    }

    #[test]
    fn request_body_has_model_and_messages() {
        let p = OpenAiCompatibleProvider::with_credentials(
            test_spec(),
            "key".to_string(),
            "m".to_string(),
            None,
        );
        let body = p.request_body("sys", "hello");
        assert_eq!(body["model"], "m");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "sys");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "hello");
    }

    #[test]
    fn chat_round_trip_against_local_server() {
        let response = r#"{"choices":[{"message":{"role":"assistant","content":"  hi there  "}}]}"#;
        let (url, recorded) = one_shot_server(response);
        let p = OpenAiCompatibleProvider::with_credentials(
            test_spec(),
            "secret-key".to_string(),
            "m".to_string(),
            Some(url),
        );
        let reply = p.chat("sys", "hello").expect("chat succeeds");
        assert_eq!(reply, "hi there");

        let raw = recorded
            .recv_timeout(Duration::from_secs(5))
            .expect("request recorded");
        assert!(raw.starts_with("POST /v1/chat/completions"), "got: {raw}");
        assert!(
            raw.contains("authorization: Bearer secret-key"),
            "got: {raw}"
        );
        assert!(raw.contains(r#""model":"m""#), "got: {raw}");
    }

    #[test]
    fn chat_fails_without_api_key() {
        let p = OpenAiCompatibleProvider::with_credentials(
            test_spec(),
            String::new(),
            "m".to_string(),
            None,
        );
        assert!(!p.is_available());
        let err = p.chat("s", "u").expect_err("must fail");
        assert!(matches!(err, ProviderError::Unavailable(_)));
        assert!(!p.validate_config().is_empty());
    }

    #[test]
    fn chat_fails_with_empty_endpoint() {
        // vultr/cloudflare have empty default base URLs and need configuration.
        let spec = OpenAiCompatibleProvider::spec_for("vultr").expect("vultr exists");
        let p = OpenAiCompatibleProvider::with_credentials(
            spec,
            "key".to_string(),
            "m".to_string(),
            None,
        );
        let err = p.chat("s", "u").expect_err("must fail");
        assert!(matches!(err, ProviderError::Unavailable(_)));
    }

    #[test]
    fn parse_response_rejects_malformed_body() {
        let bad = json!({"nope": true});
        assert!(OpenAiCompatibleProvider::parse_response(&bad).is_err());
        let empty_choices = json!({"choices": []});
        assert!(OpenAiCompatibleProvider::parse_response(&empty_choices).is_err());
    }
}
