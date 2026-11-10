//! Chat with models served by NVIDIA's OpenAI-compatible API.

use serde_json::{Value, json};

use super::LoadAgent;
use crate::error::{AriaError, Result};
use crate::paths::http;

pub const BASE_URL: &str = "https://integrate.api.nvidia.com/v1";

pub struct NvidiaAgent {
    pub model: String,
    pub model_provider: String,
    pub provider: String,
    api_key: String,
    base_url: String,
}

impl NvidiaAgent {
    pub fn new(agent: &LoadAgent) -> Result<Self> {
        let choice = agent.model_roulette(Some("nvidia"))?;
        let api_key = agent.model_key(&choice.model_provider, &choice.model)?;
        Ok(Self {
            model: choice.model,
            model_provider: choice.model_provider,
            provider: choice.provider,
            api_key,
            base_url: BASE_URL.to_owned(),
        })
    }

    /// The model id the API expects: `<vendor>/<model>`.
    pub fn model_id(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    pub fn chat(&self, prompt: &str) -> Result<String> {
        let body = json!({
            "model": self.model_id(),
            "messages": [{"role": "user", "content": prompt}],
            "stream": false,
        });
        let response: Value = http()
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send_json(&body)?
            .body_mut()
            .read_json()?;
        response["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| AriaError::Http(format!("unexpected chat response: {response}")))
    }
}
