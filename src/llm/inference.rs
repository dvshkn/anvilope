use anyhow::Result;
use async_trait::async_trait;
use log::{
    debug,
    warn,
};
use reqwest;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use super::prompt;

#[async_trait]
pub trait InferenceApi: Send + Sync {
    async fn generate(&self, prompt: String) -> Result<String>;
}

pub struct Ollama {
    pub host: String,
    pub model: String,
    pub num_ctx: usize,
    pub temperature: f64,
}

#[derive(Debug, Serialize)]
pub struct OllamaGenerateBody {
    pub prompt: String,
    pub model: String,
    pub format: Value,
    pub raw: bool,
    pub stream: bool,
    pub options: OllamaGenerateOptions,
}

#[derive(Debug, Deserialize)]
pub struct OllamaGenerateResponse {
    pub response: String,
    pub prompt_eval_count: usize,
}

#[derive(Debug, Serialize)]
pub struct OllamaGenerateOptions {
    pub num_ctx: usize,
    pub num_predict: Option<usize>,
    pub temperature: f64,
}

pub const LLM_RESPONSE_FORMAT: &'static str = r#"
{
    "type": "object",
    "properties": {
        "reasoning": { "type": "string" },
        "class": { "type": "string" }
    },
    "required": ["reasoning", "class"]
}
"#;

#[async_trait]
impl InferenceApi for Ollama {
    async fn generate(&self, prompt: String) -> Result<String> {
        debug!("sending prompt to ollama\n{}", prompt);

        let model = self.model.to_owned();
        let url = format!("{}/api/generate", self.host);
        let num_ctx = self.num_ctx;
        let num_predict = Some(prompt::MAX_OUTPUT_LEN);
        let temperature = self.temperature;

        let body = OllamaGenerateBody {
            prompt,
            model,
            format: serde_json::from_str(LLM_RESPONSE_FORMAT)?,
            raw: false,
            stream: false,
            options: OllamaGenerateOptions { num_ctx, num_predict, temperature },
        };

        let client = reqwest::Client::new();
        let res = client.post(url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let res_text = res.text().await?;
        let modeled_res: OllamaGenerateResponse = serde_json::from_str(res_text.as_str())?;
        debug!("{:#?}", modeled_res);

        if modeled_res.prompt_eval_count >= num_ctx {
            warn!("prompt was likely truncated and will impact LLM results (len={})", modeled_res.prompt_eval_count);
        }

        Ok(modeled_res.response)
    }
}
