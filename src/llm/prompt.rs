use anyhow::Result;
use minijinja::context;
use minijinja::Environment;

use crate::anvilope_config;

pub const TEMPLATE_EMAIL_MESSAGE: &'static str = r#"
From: "{{ from_name }}" {{ from_address }}
Subject: {{ subject }}
{{ body }}
"#;

pub const TEMPLATE_CLASSES: &'static str = r#"
{% for c in classes %}
{{ c._key }}: {{ c.prompt }}
{% endfor %}
"#;

pub const MAX_OUTPUT_LEN: usize = 1024; // tokens

// Returns a complete prompt that can be submitted to the LLM
pub fn assemble_prompt(template_env: &Environment,
    from_address: &String,
    from_name: &String,
    subject: &String,
    body: &String) -> Result<String> {

    let inst_template = inst_template(template_env)?;
    let message_max_len = message_max_len(&inst_template);
    let mut message_string = template_env.get_template("email_message")?
        .render(context! {
            from_address => from_address,
            from_name => from_name,
            subject => subject,
            body => body,
        })?;
    message_string.truncate(message_max_len);
    let prompt = template_env.render_str(inst_template.as_str(), context! {
        email_message => message_string,
    })?;

    Ok(prompt)
}

/// Returns a template for an almost complete prompt that does not include the
/// email message content
fn inst_template(template_env: &Environment) -> Result<String> {
    let classes = template_env.get_template("classes")?
        .render(context! { classes => anvilope_config!().classes })?;
    let instructions = template_env.get_template("classifier")?
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
