//! Minijinja-based transactional email template renderer.

use std::collections::HashMap;

use super::MailerError;

/// Named email template with subject and optional text/HTML bodies.
#[derive(Debug, Clone)]
pub struct EmailTemplate {
    /// Minijinja template string for the subject line.
    pub subject: String,
    /// Optional Minijinja template string for the plain-text body.
    pub text_body: Option<String>,
    /// Optional Minijinja template string for the HTML body.
    pub html_body: Option<String>,
}

/// Rendered email subject and body parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedEmail {
    /// Rendered subject line.
    pub subject: String,
    /// Rendered plain-text body, if configured.
    pub text_body: Option<String>,
    /// Rendered HTML body, if configured.
    pub html_body: Option<String>,
}

/// Renders transactional email templates using `minijinja`.
#[derive(Debug, Default)]
pub struct TemplateRenderer {
    env: minijinja::Environment<'static>,
    templates: HashMap<String, (bool, bool)>,
}

impl TemplateRenderer {
    /// Creates an empty [`TemplateRenderer`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a named [`EmailTemplate`].
    pub fn register_template(
        &mut self,
        name: impl Into<String>,
        template: EmailTemplate,
    ) -> Result<(), MailerError> {
        let name = name.into();
        self.env
            .add_template_owned(format!("{name}.subject"), template.subject)
            .map_err(|e| MailerError::Template(e.to_string()))?;

        let has_text = if let Some(text) = template.text_body {
            self.env
                .add_template_owned(format!("{name}.text"), text)
                .map_err(|e| MailerError::Template(e.to_string()))?;
            true
        } else {
            false
        };

        let has_html = if let Some(html) = template.html_body {
            self.env
                .add_template_owned(format!("{name}.html"), html)
                .map_err(|e| MailerError::Template(e.to_string()))?;
            true
        } else {
            false
        };

        self.templates.insert(name, (has_text, has_html));
        Ok(())
    }

    /// Renders the named template against `context`.
    pub fn render(
        &self,
        name: &str,
        context: &serde_json::Value,
    ) -> Result<RenderedEmail, MailerError> {
        let (has_text, has_html) = self
            .templates
            .get(name)
            .copied()
            .ok_or_else(|| MailerError::Template(format!("unknown template: {name}")))?;

        let ctx = json_to_minijinja(context);

        let subject = self
            .env
            .get_template(&format!("{name}.subject"))
            .and_then(|t| t.render(&ctx))
            .map_err(|e| MailerError::Template(e.to_string()))?;

        let text_body = if has_text {
            Some(
                self.env
                    .get_template(&format!("{name}.text"))
                    .and_then(|t| t.render(&ctx))
                    .map_err(|e| MailerError::Template(e.to_string()))?,
            )
        } else {
            None
        };

        let html_body = if has_html {
            Some(
                self.env
                    .get_template(&format!("{name}.html"))
                    .and_then(|t| t.render(&ctx))
                    .map_err(|e| MailerError::Template(e.to_string()))?,
            )
        } else {
            None
        };

        Ok(RenderedEmail {
            subject,
            text_body,
            html_body,
        })
    }
}

fn json_to_minijinja(value: &serde_json::Value) -> minijinja::Value {
    match value {
        serde_json::Value::Null => minijinja::Value::from(()),
        serde_json::Value::Bool(b) => minijinja::Value::from(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                minijinja::Value::from(i)
            } else if let Some(u) = n.as_u64() {
                minijinja::Value::from(u)
            } else {
                minijinja::Value::from(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => minijinja::Value::from(s.as_str()),
        serde_json::Value::Array(arr) => {
            minijinja::Value::from(arr.iter().map(json_to_minijinja).collect::<Vec<_>>())
        }
        serde_json::Value::Object(map) => {
            let entries: std::collections::BTreeMap<&str, minijinja::Value> = map
                .iter()
                .map(|(k, v)| (k.as_str(), json_to_minijinja(v)))
                .collect();
            minijinja::Value::from(entries)
        }
    }
}
