use rume::jobs::{BoxFuture, Job, JobContext, JobError, JobHandler, RetryPolicy};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendNotificationJob {
    pub tenant_id: String,
    pub recipient: String,
    pub subject: String,
    pub body: String,
    pub idempotency_key: String,
}

impl Job for SendNotificationJob {
    const NAME: &'static str = "send_notification";

    fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy::new()
            .with_max_attempts(3)
            .with_initial_backoff(Duration::from_millis(50))
    }

    fn timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn idempotency_key(&self) -> Option<String> {
        Some(self.idempotency_key.clone())
    }

    fn tenant_id(&self) -> Option<String> {
        Some(self.tenant_id.clone())
    }
}

#[derive(Clone, Default)]
pub struct NotificationHandler {
    pub deliveries: Arc<AtomicUsize>,
    pub simulated_errors: Arc<AtomicUsize>,
    pub notify: Arc<tokio::sync::Notify>,
}

impl NotificationHandler {
    pub fn new() -> Self {
        Self::default()
    }
}

impl JobHandler<SendNotificationJob> for NotificationHandler {
    fn handle<'a>(
        &'a self,
        _ctx: &'a JobContext,
        job: SendNotificationJob,
    ) -> BoxFuture<'a, Result<(), JobError>> {
        Box::pin(async move {
            let errs = self.simulated_errors.load(Ordering::SeqCst);
            if errs > 0 {
                self.simulated_errors.fetch_sub(1, Ordering::SeqCst);
                return Err(JobError::Retryable(
                    "notification delivery service busy".to_string(),
                ));
            }

            tracing::info!(
                tenant_id = %job.tenant_id,
                recipient = %job.recipient,
                subject = %job.subject,
                "Notification dispatched successfully"
            );
            self.deliveries.fetch_add(1, Ordering::SeqCst);
            self.notify.notify_waiters();
            Ok(())
        })
    }
}
