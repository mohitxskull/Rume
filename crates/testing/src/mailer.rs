//! Test-mode mailer capture sink for Rume.

use std::sync::{Arc, RwLock};

use rume_messaging::mailer::{BoxFuture, EmailMessage, Mailer, MailerError};

/// In-memory [`Mailer`] test double capturing outgoing [`EmailMessage`]s for assertions.
#[derive(Debug, Clone, Default)]
pub struct TestMailer {
    sent: Arc<RwLock<Vec<EmailMessage>>>,
}

impl TestMailer {
    /// Creates a new empty [`TestMailer`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of all captured [`EmailMessage`]s.
    pub fn sent(&self) -> Vec<EmailMessage> {
        self.sent.read().map(|g| g.clone()).unwrap_or_default()
    }

    /// Clears all captured [`EmailMessage`]s.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.sent.write() {
            guard.clear();
        }
    }

    /// Asserts that at least one email was sent to `recipient`, returning matching messages.
    pub fn assert_sent_to(&self, recipient: &str) -> Vec<EmailMessage> {
        let matching: Vec<EmailMessage> = self
            .sent()
            .into_iter()
            .filter(|msg| msg.to.iter().any(|addr| addr == recipient))
            .collect();
        assert!(
            !matching.is_empty(),
            "expected at least one email sent to '{recipient}', but none were recorded"
        );
        matching
    }
}

impl Mailer for TestMailer {
    fn send<'a>(&'a self, message: EmailMessage) -> BoxFuture<'a, Result<(), MailerError>> {
        Box::pin(async move {
            let mut guard = self
                .sent
                .write()
                .map_err(|_| MailerError::Send("TestMailer lock poisoned".into()))?;
            guard.push(message);
            Ok(())
        })
    }

    fn send_template<'a>(
        &'a self,
        from: &'a str,
        to: Vec<String>,
        template_name: &'a str,
        context: serde_json::Value,
    ) -> BoxFuture<'a, Result<(), MailerError>> {
        let msg = EmailMessage::new(from, to, template_name).with_text_body(context.to_string());
        self.send(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mailer_captures_sent_emails() {
        let mailer = TestMailer::new();
        let dyn_mailer: &dyn Mailer = &mailer;

        let msg = EmailMessage::new(
            "noreply@example.test",
            vec!["alice@example.test".to_string()],
            "Reset your password",
        )
        .with_text_body("Click here to reset");

        dyn_mailer.send(msg).await.expect("send");

        let all = mailer.sent();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].subject, "Reset your password");
        assert_eq!(all[0].text_body.as_deref(), Some("Click here to reset"));

        let alice_msgs = mailer.assert_sent_to("alice@example.test");
        assert_eq!(alice_msgs.len(), 1);

        mailer.clear();
        assert!(mailer.sent().is_empty());
    }
}
