use anyhow::{
    Error,
    Result,
};
use log::{
    warn,
};
use minijinja::context;
use serde::{
    Deserialize,
    Serialize,
};

use crate::anvilope_config;
use crate::config::Classifier;
use crate::template_env;

pub const TEMPLATE_EMAIL_MESSAGE: &'static str = r#"
From: "{{ from_name }}" {{ from_address }}
Subject: {{ subject }}
{{ body }}
"#;

pub const TEMPLATE_CLASSES: &'static str = r#"
{% for name, prompt in classes %}
{{ name }}: {{ prompt }}
{% endfor %}
"#;

pub const TEMPLATE_SYSTEM: &'static str = r#"
You are a conscientious, perceptive, and cordial email assistant piloting Anvilope, an email inbox harness.
You classify incoming email messages for the user according to critereon they set for each of the classes and any additional instructions they provide.
The current date is {{ date }}. The current time is {{ time }}. Anvilope classification session now starting after this sentence.
"#;

pub const PROMPT_JSON: &'static str = r#"
Response acknowledged. The Anvilope system requires final classifications to be JSON formatted. Please reformat your response as a single object containing:
1.) a machine readable "class" property utilizing any user-specified naming conventions
2.) a qualitative "confidence" property with possible enum values of high, medium, or low
3.) a free-form "justification" property
"#;

pub const MAX_OUTPUT_LEN: usize = 1024; // tokens

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    // inference fields ---
    pub role: ChatRole,
    pub content: String,

    #[serde(alias="reasoning_content", skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,

    // harness fields ---
    #[serde(skip)]
    pub contextualized_at: Option<i64>,

    #[serde(skip)]
    pub force_json: bool,
}

pub struct ClassificationChat {
    pub messages: Vec<ChatMessage>,
}

impl ClassificationChat {
    pub fn new(classification_prompt: String) -> Result<ClassificationChat> {
        let mut messages = Vec::new();

        let now = chrono::Local::now();
        let date = now.format("%e %B %Y").to_string();
        let time = now.format("%k:%I %P").to_string();
        let utc_timestamp = chrono::Utc::now().timestamp_millis();

        let prompt_system = template_env!().get_template("system_prompt")?
            .render(context! {
                date => date,
                time => time,
            })?
            .trim()
            .to_string();

        let prompt_json = PROMPT_JSON.trim().to_string();

        messages.push(ChatMessage {
            role: ChatRole::System,
            content: prompt_system,
            reasoning: None,
            contextualized_at: Some(utc_timestamp),
            force_json: false,
        });

        // classification
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: classification_prompt,
            reasoning: None,
            contextualized_at: Some(utc_timestamp),
            force_json: false,
        });
        messages.push(ChatMessage {
            role: ChatRole::Assistant,
            content: "".to_string(),
            reasoning: None,
            contextualized_at: None,
            force_json: false,
        });

        // json coersion
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: prompt_json,
            reasoning: None,
            contextualized_at: None,
            force_json: true,
        });
        messages.push(ChatMessage {
            role: ChatRole::Assistant,
            content: "".to_string(),
            reasoning: None,
            contextualized_at: None,
            force_json: false,
        });

        Ok(ClassificationChat { messages })
    }

    /// returns a slice of all messages in the current context window
    pub fn in_context_messages(&self) -> &[ChatMessage] {
        let index = self.messages.iter()
            .position(|msg| msg.contextualized_at.is_none())
            .unwrap_or(self.messages.len());

        // linear search until it actually matters :-)

        &self.messages[0..index]
    }

    pub fn last_message(&self) -> Result<&ChatMessage> {
        if self.messages.len() < 1 {
            return Err(Error::msg("no messages"));
        }

        let message = &self.messages[self.messages.len()-1];

        if message.contextualized_at.is_none() {
            return Err(Error::msg("last message is empty, not in context window"));
        }

        Ok(message)
    }

    /// returns a serialized copy of all the chat messages
    pub fn full_trace(&self) -> Result<String> {
        Ok(serde_json::to_string(&self.messages)?)
    }

    /// returns the number of assistant messages left that need LLM responses
    pub fn count_empty_assistant_messages(&self) -> usize {
        self.messages.iter().filter(|msg| msg.contextualized_at.is_none()).count()
    }

    /// sets content for the next empty assistant-role message, returns number
    /// of remaining empty assistant messages
    pub fn push_assistant_message(&mut self, msg: ChatMessage) -> Result<usize> {
        if msg.role != ChatRole::Assistant {
            warn!("unexpected non-assistant role in message: {:#?}", msg.role);
        }

        let index = self.messages.iter()
            .position(|msg| msg.contextualized_at.is_none())
            .ok_or_else(|| Error::msg("unexpected assistant response"))?;
        let now = chrono::Utc::now().timestamp_millis();

        self.messages[index].content = msg.content;
        self.messages[index].reasoning = msg.reasoning;
        self.messages[index].contextualized_at = Some(now);

        if index + 1 < self.messages.len() {
            self.messages[index+1].contextualized_at = Some(now);
        }

        Ok(self.count_empty_assistant_messages())
    }
}

// Returns a complete prompt that can be submitted to the LLM
pub fn assemble_prompt(
    classifier: &Classifier,
    from_address: &String,
    from_name: &String,
    subject: &String,
    body: &String) -> Result<String> {

    let inst_template = inst_template(classifier)?;
    let message_max_len = message_max_len(&inst_template);
    let mut message_string = template_env!().get_template("email_message")?
        .render(context! {
            from_address => from_address,
            from_name => from_name,
            subject => subject,
            body => body,
        })?;
    message_string.truncate(message_max_len);
    let prompt = template_env!().render_str(inst_template.as_str(), context! {
        email_message => message_string,
    })?;

    Ok(prompt)
}

/// Returns a template for an almost complete prompt that does not include the
/// email message content
fn inst_template(classifier: &Classifier) -> Result<String> {
    let classes = template_env!().get_template("classes")?
        .render(context! { classes => &classifier.classes })?;
    let instructions = template_env!()
        .get_template(&format!("classifier.{}", classifier._key))?
        .render(context! {
            email_message => "{{ email_message }}", // no-op
            classes => classes.trim(),
        })?;

    Ok(instructions)
}

/// Estimates how many characters can be used for the email message in the
/// prompt without running into the context limit. As-is this math is NOT exact because
/// Ollama does not expose a tokenize endpoint yet.
///
/// See: https://github.com/ollama/ollama/issues/3582
fn message_max_len(inst_template: &String) -> usize {
    let mut result_len_tok = anvilope_config!().llm.context_length;

    // assume instructions are human language heavy, use 3 char/tok
    let inst_len_char = inst_template.len();
    let inst_len_tok = inst_len_char / 3;
    result_len_tok -= inst_len_tok;

    // reserve tokens for LLM response
    result_len_tok -= MAX_OUTPUT_LEN;

    // assume email message will be HTML heavy, use 1.5 char/tok
    let result_len_char = (result_len_tok as f64 * 1.5).trunc() as usize;

    result_len_char
}
