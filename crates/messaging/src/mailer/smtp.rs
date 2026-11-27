//! SMTP transactional mailer backed by `lettre`.

use std::sync::Arc;
use std::time::Duration;

use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use secrecy::{ExposeSecret, SecretString};

use super::template::TemplateRenderer;
use super::{BoxFuture, EmailMessage, Mailer, MailerError};

/// Default SMTP network timeout applied to [`AsyncSmtpTransport`].
const DEFAULT_SMTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Configuration for [`SmtpMailer`].
#[derive(Debug, Clone)]
pub struct SmtpConfig {
    /// SMTP server hostname.
    pub host: String,
    /// SMTP server port.
    pub port: u16,
    /// Optional SMTP authentication username.
    pub username: Option<String>,
    /// Optional SMTP authentication password.
    pub password: Option<SecretString>,
    /// Use implicit TLS (SMTPS) instead of STARTTLS/plaintext.
    pub use_tls: bool,
    /// Use STARTTLS when `use_tls` is false.
    pub starttls: bool,
    /// Network timeout for SMTP connect/send operations (default: 30s).
    pub timeout: Duration,
}

impl SmtpConfig {
    /// Creates a new [`SmtpConfig`] targeting `host:port` with STARTTLS enabled and a 30s timeout.
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            username: None,
            password: None,
            use_tls: false,
            starttls: true,
            timeout: DEFAULT_SMTP_TIMEOUT,
        }
    }

    /// Configures SMTP authentication credentials.
    pub fn with_credentials(mut self, username: impl Into<String>, password: SecretString) -> Self {
        self.username = Some(username.into());
        self.password = Some(password);
        self
    }

    /// Enables or disables implicit TLS.
    pub fn with_tls(mut self, use_tls: bool) -> Self {
        self.use_tls = use_tls;
        self
    }

    /// Enables or disables STARTTLS.
    pub fn with_starttls(mut self, starttls: bool) -> Self {
        self.starttls = starttls;
        self
    }

    /// Configures the SMTP network timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// SMTP [`Mailer`] backed by `lettre::AsyncSmtpTransport` and optional [`TemplateRenderer`].
#[derive(Debug, Clone)]
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    renderer: Option<Arc<TemplateRenderer>>,
}

impl SmtpMailer {
    /// Builds an [`SmtpMailer`] from `config`.
    pub fn new(config: SmtpConfig) -> Result<Self, MailerError> {
        let mut builder = if config.use_tls {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)
                .map_err(|e| MailerError::Send(e.to_string()))?
        } else if config.starttls {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
                .map_err(|e| MailerError::Send(e.to_string()))?
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host)
        };

        builder = builder.port(config.port).timeout(Some(config.timeout));

        if let (Some(username), Some(password)) = (config.username, config.password) {
            builder = builder.credentials(Credentials::new(
                username,
                password.expose_secret().to_string(),
            ));
        }

        Ok(Self {
            transport: builder.build(),
            renderer: None,
        })
    }

    /// Attaches a [`TemplateRenderer`] for [`Mailer::send_template`].
    pub fn with_renderer(mut self, renderer: Arc<TemplateRenderer>) -> Self {
        self.renderer = Some(renderer);
        self
    }
}

fn contains_header_control_chars(value: &str) -> bool {
    value.contains('\r') || value.contains('\n') || value.contains('\0')
}

fn build_lettre_message(msg: &EmailMessage) -> Result<Message, MailerError> {
    if msg.to.is_empty() {
        return Err(MailerError::InvalidMessage(
            "email message must specify at least one recipient".into(),
        ));
    }
    if contains_header_control_chars(&msg.subject) {
        return Err(MailerError::InvalidMessage(
            "email subject must not contain CR, LF, or NUL characters".into(),
        ));
    }
    if contains_header_control_chars(&msg.from) {
        return Err(MailerError::InvalidMessage(
            "email from address must not contain CR, LF, or NUL characters".into(),
        ));
    }
    for addr in &msg.to {
        if contains_header_control_chars(addr) {
            return Err(MailerError::InvalidMessage(
                "email recipient address must not contain CR, LF, or NUL characters".into(),
            ));
        }
    }

    let from: Mailbox = msg
        .from
        .parse()
        .map_err(|e| MailerError::InvalidAddress(format!("from '{}': {e}", msg.from)))?;

    let mut builder = Message::builder().from(from).subject(&msg.subject);

    for addr in &msg.to {
        let to: Mailbox = addr
            .parse()
            .map_err(|e| MailerError::InvalidAddress(format!("to '{addr}': {e}")))?;
        builder = builder.to(to);
    }

    let email = match (&msg.text_body, &msg.html_body) {
        (Some(text), Some(html)) => builder.multipart(MultiPart::alternative_plain_html(
            text.clone(),
            html.clone(),
        )),
        (Some(text), None) => builder.singlepart(SinglePart::plain(text.clone())),
        (None, Some(html)) => builder.singlepart(SinglePart::html(html.clone())),
        (None, None) => builder.singlepart(SinglePart::plain(String::new())),
    }
    .map_err(|e| MailerError::Build(e.to_string()))?;

    Ok(email)
}

impl Mailer for SmtpMailer {
    fn send<'a>(&'a self, message: EmailMessage) -> BoxFuture<'a, Result<(), MailerError>> {
        Box::pin(async move {
            let email = build_lettre_message(&message)?;
            self.transport
                .send(email)
                .await
                .map(|_| ())
                .map_err(|e| MailerError::Send(e.to_string()))
        })
    }

    fn send_template<'a>(
        &'a self,
        from: &'a str,
        to: Vec<String>,
        template_name: &'a str,
        context: serde_json::Value,
    ) -> BoxFuture<'a, Result<(), MailerError>> {
        Box::pin(async move {
            let renderer = self.renderer.as_ref().ok_or_else(|| {
                MailerError::Template("no TemplateRenderer configured on SmtpMailer".into())
            })?;
            let rendered = renderer.render(template_name, &context)?;
            let mut msg = EmailMessage::new(from, to, rendered.subject);
            msg.text_body = rendered.text_body;
            msg.html_body = rendered.html_body;
            self.send(msg).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_lettre_message_rejects_empty_recipients_and_crlf_headers() {
        let empty_to = EmailMessage::new("sender@example.com", vec![], "Hello");
        let err = build_lettre_message(&empty_to).expect_err("empty recipients must fail");
        assert!(
            matches!(err, MailerError::InvalidMessage(_)),
            "expected InvalidMessage, got {err:?}"
        );

        let crlf_subject = EmailMessage::new(
            "sender@example.com",
            vec!["user@example.com".into()],
            "Hello\r\nBcc: evil@example.com",
        );
        let err = build_lettre_message(&crlf_subject).expect_err("CRLF subject must fail");
        assert!(
            matches!(err, MailerError::InvalidMessage(_)),
            "expected InvalidMessage, got {err:?}"
        );

        let nul_subject = EmailMessage::new(
            "sender@example.com",
            vec!["user@example.com".into()],
            "Hello\0World",
        );
        assert!(matches!(
            build_lettre_message(&nul_subject),
            Err(MailerError::InvalidMessage(_))
        ));

        let crlf_from = EmailMessage::new(
            "sender@example.com\nX-Injected: yes",
            vec!["user@example.com".into()],
            "Clean",
        );
        assert!(matches!(
            build_lettre_message(&crlf_from),
            Err(MailerError::InvalidMessage(_))
        ));

        let valid = EmailMessage::new(
            "sender@example.com",
            vec!["user@example.com".into()],
            "Clean Subject",
        )
        .with_text_body("hello");
        assert!(build_lettre_message(&valid).is_ok());

        // Default timeout is 30s and configurable via with_timeout.
        let cfg = SmtpConfig::new("localhost", 25);
        assert_eq!(cfg.timeout, Duration::from_secs(30));
        let custom = cfg.with_timeout(Duration::from_secs(5));
        assert_eq!(custom.timeout, Duration::from_secs(5));
    }
}
