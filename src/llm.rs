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
use crate::db::{
    Class, Classifier, LlmCompletion, LlmJob, Message
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
    pub classifier_key_id_map: HashMap<String, i64>,
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
        let (classifier_key_id_map, class_key_id_map) = Self::sync_prompts().await?;

        Ok(Llm { api, classifier_key_id_map, class_key_id_map, root_tx, busy })
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

    /// Iterates through configuration and updates prompt records in the `classifier` and `class`
    /// database tables. Class keys are namespaced per classifier (eg. "categories.spam").
    /// Returns maps of classifier key -> classifier db id and namespaced class key -> class db id.
    pub async fn sync_prompts() -> Result<(HashMap<String, i64>, HashMap<String, i64>)> {
        let mut classifier_key_id_map: HashMap<String, i64> = HashMap::new();
        let mut class_key_id_map: HashMap<String, i64> = HashMap::new();

        let db_classes = Class::get_active().await?;
        let db_key_class_map: HashMap<_, _> = db_classes.iter()
            .map(|c| (c.key.as_str(), c))
            .collect();

        for cfg_classifier in anvilope_config!().classifiers.iter() {
            let key = cfg_classifier._key.to_owned();
            let curr_prompt = cfg_classifier.prompt.to_owned();

            // sync classifier prompt record
            let classifier_id = match Classifier::get_active(&key).await? {
                Some(prev_classifier) => {
                    if prev_classifier.prompt != curr_prompt {
                        let new_classifier = Classifier::create(key.clone(), curr_prompt, true).await?;
                        new_classifier.id
                    } else {
                        prev_classifier.id
                    }
                }
                None => {
                    Classifier::create(key.clone(), curr_prompt, false).await?.id
                }
            };
            classifier_key_id_map.insert(key.clone(), classifier_id);

            // sync class prompt records for this classifier
            for cfg_class in cfg_classifier.classes.iter() {
                let class_key = cfg_classifier.class_key(&cfg_class._key);
                if let Some(class) = db_key_class_map.get(class_key.as_str()) {
                    class_key_id_map.insert(class.key.to_owned(), class.id);
                    if cfg_class.prompt != class.prompt {
                        let new_class = Class::create(
                            class_key.clone(),
                            cfg_class.prompt.to_owned(),
                            true
                        ).await?;
                        class_key_id_map.insert(new_class.key.to_owned(), new_class.id);
                    }
                } else {
                    let class = Class::create(class_key.clone(), cfg_class.prompt.to_owned(), true).await?;
                    class_key_id_map.insert(class.key.to_owned(), class.id);
                }
            }
        }

        // archive any active classifier/class records that are no longer
        // declared in the configuration
        let active_classifier_ids: Vec<i64> = classifier_key_id_map.values().cloned().collect();
        Classifier::set_active(active_classifier_ids).await?;

        let active_ids: Vec<i64> = class_key_id_map.values().cloned().collect();
        Class::set_active(active_ids).await?;

        Ok((classifier_key_id_map, class_key_id_map))
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

    /// Classifies a message with every classifier declared for the message's account.
    /// Returns one completion per classifier that ran.
    pub async fn classify(&self, message_id: i64) -> Result<Vec<LlmCompletion>> {
        let message = Message::get(message_id).await?
            .ok_or(Error::msg("message not found in db"))?;

        let account = anvilope_config!().accounts.iter()
            .find(|a| a._key == message.account_key)
            .ok_or(Error::msg("message account not found in config"))?;

        let mut completions: Vec<LlmCompletion> = Vec::new();
        for classifier_key in account.classifiers.iter() {
            let classifier = anvilope_config!().classifiers.iter()
                .find(|c| &c._key == classifier_key)
                .ok_or(Error::msg("account references unknown classifier"))?;

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
                body = message.body_plain.clone();
            }

            let classify_prompt = prompt::assemble_prompt(
                classifier,
                &message.from_address,
                &message.from_name,
                &message.subject,
                &body,
            )?;

            let mut chat = ClassificationChat::new(classify_prompt)?;
            self.run_chat(&mut chat).await?;
            let final_response = chat.last_message()?.content.clone();

            let classifier_id = *self.classifier_key_id_map.get(classifier_key)
                .ok_or(Error::msg("classifier id not found in db sync map"))?;

            let mut class_id: Option<i64> = None;
            let mut class_key: Option<String> = None;
            match self.process_llm_response(classifier_key, &final_response).await {
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

            completions.push(completion);
        }

        Ok(completions)
    }

    /// Takes the LLM response for a particular classifier and attempts to
    /// cleanly extract the chosen class.
    pub async fn process_llm_response(&self, classifier_key: &String, llm_response: &String) -> Result<(i64, String)> {
        // TODO: make custom llm error type
        let obj: Value = serde_json::from_str(llm_response.as_str())
            .map_err(|_| Error::msg("LLM response not valid JSON"))?;
        let obj = obj.as_object()
            .ok_or(Error::msg("LLM response not a JSON object"))?;

        let class_k = obj.keys().find(|k| k.to_lowercase() == "class")
            .ok_or(Error::msg("LLM response lacks \"class\" property"))?;
        let class_v = obj[class_k].as_str()
            .ok_or(Error::msg("LLM repsonse contains unexpected type for \"class\" property"))?;

        let expected_key = format!("{}.{}", classifier_key, class_v);
        for (key, id) in self.class_key_id_map.iter() {
            if key.to_lowercase() == expected_key.to_lowercase() {
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
            let completions_result = self.classify(job.message_id).await;

            let mut job_failed = true;
            if let Ok(completions) = completions_result {
                for completion in completions {
                    if let Some(class_key) = completion.class_key {
                        let message_id = completion.message_id;
                        LlmJob::set_status(job.id, LlmJob::JOB_STATE_COMPLETED).await?;
                        self.root_tx.send(AnvilopeEvent::MessageClassified {
                            message_id,
                            class_key,
                        }).await?;
                        job_failed = false;
                    }
                }
            } else if let Err(e) = completions_result {
                error!("classify: {:#?}", e);
            }
            if job_failed {
                LlmJob::set_status(job.id, LlmJob::JOB_STATE_FAILED).await?;
            }
        }

        Ok(())
    }
}
