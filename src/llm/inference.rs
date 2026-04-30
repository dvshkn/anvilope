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
    // TODO: switch to chat-oriented flow, return messages instead of string
    async fn generate(&self, prompt: String) -> Result<String>;
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

pub const LLAMA_CPP_RESPONSE_FORMAT: &'static str = r#"
{
    "type": "json_schema",
    "strict": true,
    "schema": {
        "type": "object",
        "properties": {
            "reasoning": { "type": "string" },
            "class": { "type": "string" }
        },
        "required": ["reasoning", "class"]
    }
}
"#;

//
// ollama support
////////////////////

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

//
// llama.cpp support
///////////////////////

pub struct LlamaCpp {
    pub host: String,
    pub model: String,
    pub temperature: f64,
    pub num_ctx: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CompletionMessage {
    pub role: String,
    pub content: String,

    #[serde(skip_serializing)]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CompletionChoice {
    pub index: usize,
    pub message: CompletionMessage,
    pub finish_reason: String,
}

#[derive(Debug, Deserialize)]
pub struct CompletionUsage {
    pub completion_tokens: usize,
    pub prompt_tokens: usize,
    pub total_tokens: usize,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionsRequest {
    pub messages: Vec<CompletionMessage>,
    pub model: String,
    pub stream: bool,
    pub temperature: f64,

    // OpenAI name: max_tokens / max_completion_tokens
    pub n_predict: usize,

    // NOTE: OpenAI-compatible response_format field seems bugged atm, using
    //       llama.cpp's json_schema field instead
    pub json_schema: Value,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsResponse {
    pub id: String,
    pub choices: Vec<CompletionChoice>,
    pub usage: CompletionUsage,
}

#[async_trait]
impl InferenceApi for LlamaCpp {
    async fn generate(&self, prompt: String) -> Result<String> {
        debug!("sending prompt to llama.cpp\n{}", prompt);

        let url = format!("{}/v1/chat/completions", self.host);
        let body = ChatCompletionsRequest {
            messages: Vec::from([
                CompletionMessage {
                    role: "user".to_string(),
                    content: prompt,
                    reasoning_content: None,
                }
            ]),
            model: self.model.to_owned(),
            stream: false,
            temperature: self.temperature,
            // TOOD: refactor token budgeting logic in llm.rs
            n_predict: self.num_ctx / 8,
            json_schema: serde_json::from_str(LLM_RESPONSE_FORMAT)?,
        };

        let client = reqwest::Client::new();
        let res = client.post(url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let res_text = res.text().await?;
        let modeled_res: ChatCompletionsResponse = serde_json::from_str(res_text.as_str())?;
        debug!("{:#?}", modeled_res);

        if modeled_res.usage.prompt_tokens >= self.num_ctx {
            warn!("prompt exceeded context window length (num_ctx={}, prompt_len={})", self.num_ctx, modeled_res.usage.prompt_tokens);
        }
        if modeled_res.choices.get(0).unwrap().finish_reason == "length" {
            warn!("LLM response was truncated (response_len={})", modeled_res.usage.completion_tokens);
        }

        Ok(modeled_res.choices.get(0).unwrap().message.content.to_owned())
    }
}
