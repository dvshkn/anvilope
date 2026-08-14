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

// database state for one classifier: its active row id and a map of
// plain class name -> active class row id
#[derive(Debug)]
pub struct ClassifierState {
    pub id: i64,
    pub class_key_id_map: HashMap<String, i64>,
}

pub struct Llm {
    pub api: Box<dyn InferenceApi>,
    pub classifiers: HashMap<String, ClassifierState>,
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
        let classifiers = Self::sync_prompts().await?;

        Ok(Llm { api, classifiers, root_tx, busy })
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
    /// database tables. Returns the active database state for each configured classifier, keyed
    /// by classifier name.
    pub async fn sync_prompts() -> Result<HashMap<String, ClassifierState>> {
        let mut result: HashMap<String, ClassifierState> = HashMap::new();

        // active db classifiers, keyed by name (highest id wins on duplicates)
        let db_classifiers = Classifier::get_active().await?;
        let mut db_cf_map: HashMap<String, &Classifier> = HashMap::new();
        for db_cf in db_classifiers.iter() {
            match db_cf_map.get(db_cf.key.as_str()) {
                Some(existing) if existing.id >= db_cf.id => {},
                _ => { db_cf_map.insert(db_cf.key.to_owned(), db_cf); },
            }
        }

        let db_classes = Class::get_active().await?;

        for cfg_cf in anvilope_config!().classifiers.iter() {
            let cf_key = cfg_cf._key.as_str();

            // ensure an active classifier row exists for this config entry
            let cf_id: i64;
            match db_cf_map.get(cf_key) {
                Some(db_cf) if db_cf.prompt == cfg_cf.prompt => {
                    cf_id = db_cf.id;
                },
                Some(db_cf) => {
                    let new_cf = Classifier::create(
                        cf_key.to_string(),
                        cfg_cf.prompt.to_owned(),
                        false
                    ).await?;
                    archive_classifier_and_classes(db_cf.id, &db_classes).await?;
                    cf_id = new_cf.id;
                },
                None => {
                    let new_cf = Classifier::create(
                        cf_key.to_string(),
                        cfg_cf.prompt.to_owned(),
                        false
                    ).await?;
                    cf_id = new_cf.id;
                },
            }

            // active class rows for this classifier, keyed by class name
            let mut db_class_map: HashMap<&str, &Class> = HashMap::new();
            for db_class in db_classes.iter().filter(|c| c.classifier_id == cf_id) {
                match db_class_map.get(db_class.key.as_str()) {
                    Some(existing) if existing.id >= db_class.id => {},
                    _ => { db_class_map.insert(db_class.key.as_str(), db_class); },
                }
            }

            let mut class_key_id_map: HashMap<String, i64> = HashMap::new();
            for (class_key, class_prompt) in cfg_cf.classes.iter() {
                match db_class_map.get(class_key.as_str()) {
                    Some(db_class) if db_class.prompt == *class_prompt => {
                        class_key_id_map.insert(class_key.to_owned(), db_class.id);
                    },
                    Some(db_class) => {
                        let new_class = Class::create(
                            cf_id,
                            class_key.to_owned(),
                            class_prompt.to_owned(),
                            false
                        ).await?;
                        Class::archive(db_class.id).await?;
                        class_key_id_map.insert(class_key.to_owned(), new_class.id);
                    },
                    None => {
                        let new_class = Class::create(
                            cf_id,
                            class_key.to_owned(),
                            class_prompt.to_owned(),
                            false
                        ).await?;
                        class_key_id_map.insert(class_key.to_owned(), new_class.id);
                    },
                }
            }

            // archive classes for this classifier that are no longer in config
            for (class_key, db_class) in db_class_map.iter() {
                if !cfg_cf.classes.contains_key(*class_key) {
                    Class::archive(db_class.id).await?;
                }
            }

            result.insert(cf_key.to_owned(), ClassifierState { id: cf_id, class_key_id_map });
        }

        // archive classifiers that are no longer in config
        for (cf_key, db_cf) in db_cf_map.iter() {
            let still_configured = anvilope_config!().classifiers.iter()
                .any(|c| c._key == *cf_key);
            if !still_configured {
                archive_classifier_and_classes(db_cf.id, &db_classes).await?;
            }
        }

        Ok(result)
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

    pub async fn classify(&self, message_id: i64, classifier_key: &String) -> Result<LlmCompletion> {
        let message = Message::get(message_id).await?
            .ok_or(Error::msg("message not found in db"))?;

        let classifier_state = self.classifiers.get(classifier_key.as_str())
            .ok_or(Error::msg(format!("unknown classifier {}", classifier_key)))?;
        let cfg_classifier = anvilope_config!().classifiers.iter()
            .find(|c| c._key == *classifier_key)
            .ok_or(Error::msg(format!("classifier {} not present in config", classifier_key)))?;

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
            cfg_classifier,
            &message.from_address,
            &message.from_name,
            &message.subject,
            &body,
        )?;

        let mut chat = ClassificationChat::new(classify_prompt)?;
        self.run_chat(&mut chat).await?;
        let final_response = chat.last_message()?.content.clone();

        let mut class_id: Option<i64> = None;
        let mut class_key: Option<String> = None;
        match self.process_llm_response(&final_response, classifier_key).await {
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
            classifier_state.id,
            final_response,
            full_trace,
            class_id,
            class_key,
        ).await?;

        Ok(completion)
    }

    /// Takes the LLM response and attempts to cleanly extract the chosen class.
    /// Returns the class row id and the namespaced "<classifier>.<class>" key.
    pub async fn process_llm_response(&self, llm_response: &String, classifier_key: &String) -> Result<(i64, String)> {
        // TODO: make custom llm error type
        let obj: Value = serde_json::from_str(llm_response.as_str())
            .map_err(|_| Error::msg("LLM response not valid JSON"))?;
        let obj = obj.as_object()
            .ok_or(Error::msg("LLM response not a JSON object"))?;

        let class_k = obj.keys().find(|k| k.to_lowercase() == "class")
            .ok_or(Error::msg("LLM response lacks \"class\" property"))?;
        let class_v = obj[class_k].as_str()
            .ok_or(Error::msg("LLM repsonse contains unexpected type for \"class\" property"))?;

        let state = self.classifiers.get(classifier_key.as_str())
            .ok_or(Error::msg(format!("unknown classifier {}", classifier_key)))?;
        for (key, id) in state.class_key_id_map.iter() {
            if key.to_lowercase() == class_v.to_lowercase() {
                let namespaced_key = format!("{}.{}", classifier_key, key);
                return Ok((id.to_owned(), namespaced_key));
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

            info!("sending message_id={} (classifier={}) to LLM", job.message_id, job.classifier_key);
            let completion_result = self.classify(job.message_id, &job.classifier_key).await;

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

/// Archives a classifier row along with all active class rows that belong to it.
async fn archive_classifier_and_classes(cf_id: i64, db_classes: &[Class]) -> Result<()> {
    for db_class in db_classes.iter().filter(|c| c.classifier_id == cf_id) {
        Class::archive(db_class.id).await?;
    }
    Classifier::archive(cf_id).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AnvilopeConfig,
        RunMode,
    };

    fn init_test_globals() {
        crate::ANVILOPE_CONFIG.get_or_init(|| AnvilopeConfig::new(RunMode::Test));
        // delete the test db if it exists so we get a fresh run
        let db_path = anvilope_config!().db_path.to_owned();
        let _ = std::fs::remove_file(&db_path);
        crate::DB_POOL.get_or_init(|| crate::db::init_pool());
        crate::db::ensure_tables().unwrap();
    }

    #[tokio::test]
    async fn test_sync_prompts() {
        init_test_globals();

        // simulate a classifier from a previous config that no longer exists
        let stale_cf = Classifier::create(
            "stale".to_string(),
            "stale prompt".to_string(),
            false,
        ).await.unwrap();
        Class::create(
            stale_cf.id,
            "staleclass".to_string(),
            "stale".to_string(),
            false,
        ).await.unwrap();

        // first sync
        let state = Llm::sync_prompts().await.unwrap();
        let cfg = anvilope_config!();

        assert_eq!(state.len(), cfg.classifiers.len());
        let db_cfs = Classifier::get_active().await.unwrap();
        assert_eq!(db_cfs.len(), cfg.classifiers.len());
        for cfg_cf in cfg.classifiers.iter() {
            let db_cf = db_cfs.iter().find(|c| c.key == cfg_cf._key)
                .unwrap_or_else(|| panic!("classifier {} not active", cfg_cf._key));
            assert_eq!(db_cf.prompt, cfg_cf.prompt);
            let st = state.get(cfg_cf._key.as_str()).unwrap();
            assert_eq!(st.id, db_cf.id);
            assert_eq!(st.class_key_id_map.len(), cfg_cf.classes.len());
        }

        // stale classifier and its classes should have been archived
        assert!(db_cfs.iter().all(|c| c.key != "stale"));

        // all configured classes should be present as active rows under the
        // correct classifier
        let total_classes: usize = cfg.classifiers.iter().map(|c| c.classes.len()).sum();
        let db_classes = Class::get_active().await.unwrap();
        assert_eq!(db_classes.len(), total_classes);
        assert!(db_classes.iter().all(|c| c.key != "staleclass"));
        for cfg_cf in cfg.classifiers.iter() {
            let db_cf = db_cfs.iter().find(|c| c.key == cfg_cf._key).unwrap();
            for (class_key, class_prompt) in cfg_cf.classes.iter() {
                let db_class = db_classes.iter()
                    .find(|c| c.classifier_id == db_cf.id && c.key == *class_key)
                    .unwrap_or_else(|| panic!("class {}.{} not active", cfg_cf._key, class_key));
                assert_eq!(db_class.prompt, *class_prompt);
            }
        }

        // second sync should be a no-op
        let state2 = Llm::sync_prompts().await.unwrap();
        for cfg_cf in cfg.classifiers.iter() {
            let s1 = state.get(cfg_cf._key.as_str()).unwrap();
            let s2 = state2.get(cfg_cf._key.as_str()).unwrap();
            assert_eq!(s2.id, s1.id);
            assert_eq!(s2.class_key_id_map, s1.class_key_id_map);
        }
        assert_eq!(Classifier::get_active().await.unwrap().len(), cfg.classifiers.len());
        assert_eq!(Class::get_active().await.unwrap().len(), total_classes);
    }
}
