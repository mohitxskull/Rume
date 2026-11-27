//! Transactional email (`Mailer` trait, `TemplateRenderer`, and `SmtpMailer`).

#[cfg(feature = "mailer")]
pub mod smtp;
#[cfg(feature = "mailer")]
pub mod template;

#[cfg(feature = "mailer")]
pub use smtp::{SmtpConfig, SmtpMailer};
#[cfg(feature = "mailer")]
pub use template::{EmailTemplate, RenderedEmail, TemplateRenderer};

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Pinned boxed future type alias for async mailer operations.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Errors returned by transactional email rendering or delivery.
#[derive(Debug, Error)]
pub enum MailerError {
    /// Failed to render a template.
    #[error("email template error: {0}")]
    Template(String),
    /// Invalid sender or recipient email address.
    #[error("invalid email address: {0}")]
    InvalidAddress(String),
    /// Failed to construct the MIME email message.
    #[error("failed to build email: {0}")]
    Build(String),
    /// Failed to transmit the email via the configured transport.
    #[error("failed to send email: {0}")]
    Send(String),
}

/// Outgoing transactional email message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailMessage {
    /// Recipient email addresses.
    pub to: Vec<String>,
    /// Sender email address.
    pub from: String,
    /// Email subject line.
    pub subject: String,
    /// Optional plain-text body.
    pub text_body: Option<String>,
    /// Optional HTML body.
    pub html_body: Option<String>,
}

impl EmailMessage {
    /// Creates a new [`EmailMessage`].
    pub fn new(from: impl Into<String>, to: Vec<String>, subject: impl Into<String>) -> Self {
        Self {
            to,
            from: from.into(),
            subject: subject.into(),
            text_body: None,
            html_body: None,
        }
    }

    /// Sets the plain-text body.
    pub fn with_text_body(mut self, text: impl Into<String>) -> Self {
        self.text_body = Some(text.into());
        self
    }

    /// Sets the HTML body.
    pub fn with_html_body(mut self, html: impl Into<String>) -> Self {
        self.html_body = Some(html.into());
        self
    }
}

/// Contract for sending transactional emails.
pub trait Mailer: std::fmt::Debug + Send + Sync + 'static {
    /// Sends a pre-built [`EmailMessage`].
    fn send<'a>(&'a self, message: EmailMessage) -> BoxFuture<'a, Result<(), MailerError>>;

    /// Renders a named template with `context` JSON and sends it.
    fn send_template<'a>(
        &'a self,
        from: &'a str,
        to: Vec<String>,
        template_name: &'a str,
        context: serde_json::Value,
    ) -> BoxFuture<'a, Result<(), MailerError>>;
}

#[cfg(all(test, feature = "mailer"))]
mod tests {
    use super::*;

    #[test]
    fn minijinja_template_renderer_renders_subject_and_body() {
        let mut renderer = TemplateRenderer::new();
        renderer
            .register_template(
                "welcome",
                EmailTemplate {
                    subject: "Welcome, {{ name }}!".into(),
                    text_body: Some("Hello {{ name }}, your code is {{ code }}.".into()),
                    html_body: Some("<p>Hello <strong>{{ name }}</strong>!</p>".into()),
                },
            )
            .expect("register_template");

        let rendered = renderer
            .render("welcome", &serde_json::json!({"name": "Alice", "code": 42}))
            .expect("render");

        assert_eq!(rendered.subject, "Welcome, Alice!");
        assert_eq!(
            rendered.text_body.as_deref(),
            Some("Hello Alice, your code is 42.")
        );
        assert_eq!(
            rendered.html_body.as_deref(),
            Some("<p>Hello <strong>Alice</strong>!</p>")
        );
    }
}
