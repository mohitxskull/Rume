//! SMTP transactional mailer backed by `lettre`.

use std::sync::Arc;

use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use secrecy::{ExposeSecret, SecretString};

use super::template::TemplateRenderer;
use super::{BoxFuture, EmailMessage, Mailer, MailerError};

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
}

impl SmtpConfig {
    /// Creates a new [`SmtpConfig`] targeting `host:port` with STARTTLS enabled.
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            username: None,
            password: None,
            use_tls: false,
            starttls: true,
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

        builder = builder.port(config.port);

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

fn build_lettre_message(msg: &EmailMessage) -> Result<Message, MailerError> {
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
