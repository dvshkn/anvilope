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

pub const LLM_RESPONSE_FORMAT_JSON_SCHEMA: &'static str = r#"{
    "type": "json_schema",
    "json_schema": {
        "name": "classification_result",
        "strict": true,
        "schema": {
            "type": "object",
            "properties": {
                "class": { "type": "string" },
                "confidence": { "type": "string" },
                "justification": { "type": "string" }
            },
            "required": ["class", "confidence", "justification"]
        }
    }
}"#;

pub const LLAMA_CPP_DISABLE_THINKING_KWARGS: &'static str = r#"{"enable_thinking": false}"#;

#[derive(Debug, Serialize, Deserialize)]
pub struct CompletionMessage {
    pub role: String,
    pub content: String,

    // llama.cpp
    #[serde(skip_serializing)]
    pub reasoning_content: Option<String>,

    // olamma
    #[serde(skip_serializing)]
    pub reasoning: Option<String>,
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompletionReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionsRequest<'msgs> {
    pub messages: &'msgs [prompt::ChatMessage],
    pub model: String,
    pub stream: bool,
    pub temperature: f64,

    // NOTE: want max_tokens > thinking_budget_tokens
    pub max_completion_tokens: usize,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,

    // ollama reasoning controls (openai compliant)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<CompletionReasoningEffort>,

    // llama.cpp reasoning controls
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget_tokens: Option<usize>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsResponse {
    pub id: String,
    pub choices: Vec<CompletionChoice>,
    pub usage: CompletionUsage,
}

//
// ollama support
////////////////////

pub struct Ollama {
    pub host: String,
    pub model: String,
    pub num_ctx: usize,
    pub temperature: f64,
}

#[async_trait]
impl InferenceApi for Ollama {
    async fn generate(&self, messages: &[prompt::ChatMessage]) -> Result<String> {
        debug!("sending prompt to ollama\n {:#?}", messages);

        let url = format!("{}/v1/chat/completions", self.host);
        let mut body = ChatCompletionsRequest {
            messages,
            model: self.model.to_owned(),
            stream: false,
            temperature: self.temperature,
            // TOOD: refactor token budgeting logic in llm.rs
            max_completion_tokens: self.num_ctx / 8,

            response_format: None,
            reasoning_effort: None,
            thinking_budget_tokens: None,
            chat_template_kwargs: None,
        };

        // only enforce JSON schema if specified by the message
        if messages.last().unwrap().force_json {
            body.response_format = Some(serde_json::from_str(LLM_RESPONSE_FORMAT_JSON_SCHEMA)?);
        }

        // disable reasoning for now
        body.reasoning_effort = Some(CompletionReasoningEffort::None);

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

//
// llama.cpp support
///////////////////////

pub struct LlamaCpp {
    pub host: String,
    pub model: String,
    pub temperature: f64,
    pub num_ctx: usize,
}

#[async_trait]
impl InferenceApi for LlamaCpp {
    async fn generate(&self, messages: &[prompt::ChatMessage]) -> Result<String> {
        debug!("sending prompt to llama.cpp\n {:#?}", messages);

        let url = format!("{}/v1/chat/completions", self.host);
        let mut body = ChatCompletionsRequest {
            messages,
            model: self.model.to_owned(),
            stream: false,
            temperature: self.temperature,
            // TOOD: refactor token budgeting logic in llm.rs
            max_completion_tokens: self.num_ctx / 8,

            response_format: None,
            reasoning_effort: None,
            thinking_budget_tokens: None,
            chat_template_kwargs: None,
        };

        // only enforce JSON schema if specified by the message
        if messages.last().unwrap().force_json {
            body.response_format = Some(serde_json::from_str(LLM_RESPONSE_FORMAT_JSON_SCHEMA)?);
        }

        // TODO: add config option to enable thinking budget
        // HACK: disabling thinking completely for now because of
        //       instability with some models
        body.thinking_budget_tokens = Some(0);
        body.chat_template_kwargs = Some(serde_json::from_str(LLAMA_CPP_DISABLE_THINKING_KWARGS)?);

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
