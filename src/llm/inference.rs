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
    async fn generate(&self, messages: &[prompt::ChatMessage]) -> Result<String>;
}

pub const LLM_RESPONSE_FORMAT: &'static str = r#"
{
    "type": "object",
    "properties": {
        "class": { "type": "string" },
        "confidence": { "type": "string" },
        "justification": { "type": "string" }
    },
    "required": ["class", "confidence", "justification"]
}
"#;

pub const LLAMA_CPP_DISABLE_THINKING_KWARGS: &'static str = r#"{"enable_thinking": false}"#;

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
    async fn generate(&self, messages: &[prompt::ChatMessage]) -> Result<String> {
        let prompt = messages.first().unwrap().content.as_ref().unwrap().clone();

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
pub struct ChatCompletionsRequest<'msgs> {
    pub messages: &'msgs [prompt::ChatMessage],
    pub model: String,
    pub stream: bool,
    pub temperature: f64,

    // NOTE: want max_tokens > thinking_budget_tokens
    pub max_tokens: usize,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget_tokens: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<Value>,

    // NOTE: OpenAI-compatible response_format field seems bugged atm, using
    //       llama.cpp's json_schema field instead
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsResponse {
    pub id: String,
    pub choices: Vec<CompletionChoice>,
    pub usage: CompletionUsage,
}

#[async_trait]
impl InferenceApi for LlamaCpp {
    async fn generate(&self, messages: &[prompt::ChatMessage]) -> Result<String> {
        debug!("sending prompt to llama.cpp\n {:#?}", messages);

        let url = format!("{}/v1/chat/completions", self.host);

        // only enforce JSON schema if specified by the message
        let mut json_schema = Some(serde_json::from_str(LLM_RESPONSE_FORMAT)?);
        if !messages.last().unwrap().force_json {
            json_schema = None;
        }

        // TODO: add config option to enable thinking budget
        // HACK: disabling thinking completely for now because of
        //       instability with some models
        let thinking_budget_tokens = Some(0);
        let chat_template_kwargs = Some(serde_json::from_str(LLAMA_CPP_DISABLE_THINKING_KWARGS)?);

        let body = ChatCompletionsRequest {
            messages,
            model: self.model.to_owned(),
            stream: false,
            temperature: self.temperature,
            // TOOD: refactor token budgeting logic in llm.rs
            max_tokens: self.num_ctx / 8,
            thinking_budget_tokens,
            chat_template_kwargs,
            json_schema,
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
