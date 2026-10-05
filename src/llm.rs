pub mod prompt;
mod inference;

use anyhow::{
    Error,
    Result,
};
use log::{
    debug,
    error,
    info,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Sender;

use crate::anvilope_config;
use crate::config::Class;
use crate::db::{
    Classifier, LlmCompletion, LlmJob, Message
};
use crate::AnvilopeEvent;
use crate::mail::parse;

use inference::{
    InferenceApi,
    Ollama,
    LlamaCpp,
};
use prompt::ClassificationChat;

pub const JOB_STATE_QUEUED: &'static str = "queued";
pub const JOB_STATE_PROCESSING: &'static str = "processing";

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

pub struct Llm {
    pub api: Box<dyn InferenceApi>,
    pub classifier_id: i64,
    pub class_key_id_map: HashMap<String, i64>,
    pub root_tx: Sender<AnvilopeEvent>,
    pub busy: Mutex<bool>,
}

impl Llm  {
    pub async fn new(root_tx: Sender<AnvilopeEvent>) -> Result<Llm> {
        let busy = Mutex::new(false);

        // setup for inference calls
        let api: Box<dyn InferenceApi> = if let Some(ollama_host) = anvilope_config!().llm.ollama_host.as_ref() {
            Box::new(Ollama {
                host: ollama_host.to_owned(),
                model: anvilope_config!().llm.model.to_owned(),
                num_ctx: anvilope_config!().llm.context_length,
                temperature: anvilope_config!().llm.temperature,
            })
        } else if let Some(llama_cpp_host) = anvilope_config!().llm.llama_cpp_host.as_ref() {
            Box::new(LlamaCpp {
                host: llama_cpp_host.to_owned(),
                model: anvilope_config!().llm.model.to_owned(),
                num_ctx: anvilope_config!().llm.context_length,
                temperature: anvilope_config!().llm.temperature,
            })
        } else {
            return Err(Error::msg("bad config: no inference host"));
        };

        // make sure database reflects current configuration
        // let (classifier_id, class_key_id_map) = Self::sync_prompts().await?;
        // HACK: revisit this
        Self::sync_prompts().await?;
        let classifier_id = 0;
        let class_key_id_map: HashMap<String, i64> = HashMap::new();

        Ok(Llm { api, classifier_id, class_key_id_map, root_tx, busy })
    }

    pub fn spawn(llm_ref: Arc<Llm>) -> () {
        tokio::spawn(async move {
            if let Ok(lock) = llm_ref.busy.try_lock() {
                debug!("LLM will start processing jobs");
                match llm_ref.process_jobs().await {
                    Ok(_) => {
                        debug!("LLM job processing finished");
                    },
                    Err(e) => {
                        error!("process_jobs: {:#?}", e);
                    }
                };
                drop(lock)
            } else {
                debug!("LLM is already processing jobs, aborting...");
            }
        });
    }

    /// Iterates through configuration and updates prompt records in the
    /// `classifier` database tables. Returns relevant IDs of active entries.
    pub async fn sync_prompts() -> Result<()> {
        let db_classifiers = Classifier::get_active().await?;
        let mut new_active_ids: Vec<i64> = Vec::new();

        for (key, classifier) in anvilope_config!().classifiers.iter() {
            let mut need_write = false;
            if let Some(db_copy) = db_classifiers.get(key) {
                println!("db copy");
                if !db_copy.equivalent_to(classifier) {
                    need_write = true;
                } else {
                    new_active_ids.push(db_copy.id);
                }
            } else {
                println!("not found");
                need_write = true;
            }
            println!("\tclassifier {} -- need_write={}", key, need_write);
            if need_write {
                let db_classifier = Classifier::create(
                    key.to_owned(),
                    classifier.prompt.to_owned(),
                    &classifier.classes,
                    false
                ).await?;
                new_active_ids.push(db_classifier.id);
            }
        }

        Classifier::set_active(&new_active_ids).await?;

        Ok(())
    }

    pub async fn run_chat(&self, chat: &mut ClassificationChat) -> Result<()> {
        let mut messages_remaining = chat.count_empty_assistant_messages();
        while messages_remaining > 0 {
            let chat_messages = chat.in_context_messages();
            let assistant_response = self.api.generate(chat_messages).await?;
            messages_remaining = chat.push_assistant_message(assistant_response)?;
        }
        Ok(())
    }

    pub async fn classify(&self, message_id: i64) -> Result<LlmCompletion> {
        let message = Message::get(message_id).await?
            .ok_or(Error::msg("message not found in db"))?;

        //
        // decide whether to read plain text or HTML content
        //
        // TODO: add config flag to prefer plain text
        let mut body = "(body content could not be decoded)".to_string();
        if message.body_html.len() > 0 {
            // TODO: add config flag to allow for raw HTML
            if let Ok(body_md) = parse::html_to_markdown(&message.body_html) {
                body = body_md;
            }
        } else if message.body_plain.len() > 0 {
            body = message.body_plain;
        }

        let classify_prompt = prompt::assemble_prompt(
            &message.from_address,
            &message.from_name,
            &message.subject,
            &body,
        )?;

        let mut chat = ClassificationChat::new(classify_prompt)?;
        self.run_chat(&mut chat).await?;
        let final_response = chat.last_message()?.content.clone();

        // HACK: fix this for real
        let classifier_id = Classifier::get_active().await?.get("default").unwrap().id;

        let mut class_id: Option<i64> = None;
        let mut class_key: Option<String> = None;
        match self.process_llm_response(&final_response).await {
            Ok((id, key)) => {
                class_id = Some(id);
                class_key = Some(key);
            },
            Err(e) => {
                error!("process_llm_response: {:#?}", e);
            },
        };

        let full_trace = chat.full_trace()?;

        let completion = LlmCompletion::create(
            message_id,
            classifier_id,
            final_response,
            full_trace,
            class_id,
            class_key,
        ).await?;

        Ok(completion)
    }

    /// Takes the LLM response and attempts to cleanly extract the chosen class.
    pub async fn process_llm_response(&self, llm_response: &String) -> Result<(i64, String)> {
        // TODO: make custom llm error type
        let obj: Value = serde_json::from_str(llm_response.as_str())
            .map_err(|_| Error::msg("LLM response not valid JSON"))?;
        let obj = obj.as_object()
            .ok_or(Error::msg("LLM response not a JSON object"))?;

        let class_k = obj.keys().find(|k| k.to_lowercase() == "class")
            .ok_or(Error::msg("LLM response lacks \"class\" property"))?;
        let class_v = obj[class_k].as_str()
            .ok_or(Error::msg("LLM repsonse contains unexpected type for \"class\" property"))?;

        for (key, id) in self.class_key_id_map.iter() {
            if key.to_lowercase() == class_v.to_lowercase() {
                return Ok((id.to_owned(), key.to_owned()));
            }
        }

        Err(Error::msg("LLM response specified undefined class"))
    }

    /// Processes all jobs waiting in the LLM queue until empty.
    async fn process_jobs(&self) -> Result<()> {
        loop {
            let job = match LlmJob::dequeue().await? {
                Some(job) => job,
                None => break,
            };

            info!("sending message_id={} to LLM", job.message_id);
            let completion_result = self.classify(job.message_id).await;

            let mut job_failed = true;
            if let Ok(completion) = completion_result {
                if let Some(class_key) = completion.class_key {
                    let message_id = completion.message_id;
                    LlmJob::set_status(job.id, LlmJob::JOB_STATE_COMPLETED).await?;
                    self.root_tx.send(AnvilopeEvent::MessageClassified {
                        message_id,
                        class_key,
                    }).await?;
                    job_failed = false;
                }
            } else if let Err(e) = completion_result {
                error!("classify: {:#?}", e);
            }
            if job_failed {
                LlmJob::set_status(job.id, LlmJob::JOB_STATE_FAILED).await?;
            }
        }

        Ok(())
    }
}
