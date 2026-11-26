//! Usage metering and billing export contracts for Rume.

pub mod meter;
pub mod sql;

pub use meter::{
    BillingSink, BoxFuture, InMemoryUsageMeter, MeteringError, UsageMeter, UsageRecord,
};
pub use sql::SqlUsageMeter;

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct MockBillingSink {
        batches: Mutex<Vec<Vec<UsageRecord>>>,
    }

    impl BillingSink for MockBillingSink {
        fn export_batch<'a>(
            &'a self,
            records: &'a [UsageRecord],
        ) -> BoxFuture<'a, Result<(), MeteringError>> {
            Box::pin(async move {
                self.batches.lock().unwrap().push(records.to_vec());
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn usage_meter_aggregates_per_tenant_and_flushes_to_billing_sink() {
        let meter = InMemoryUsageMeter::new();
        let sink = MockBillingSink::default();

        meter
            .record_usage("tenant-a", "api_calls", 10)
            .await
            .unwrap();
        meter
            .record_usage("tenant-a", "api_calls", 5)
            .await
            .unwrap();
        meter
            .record_usage("tenant-b", "api_calls", 7)
            .await
            .unwrap();

        assert_eq!(meter.get_usage("tenant-a", "api_calls").await.unwrap(), 15);
        assert_eq!(meter.get_usage("tenant-b", "api_calls").await.unwrap(), 7);
        assert_eq!(meter.get_usage("tenant-c", "api_calls").await.unwrap(), 0);

        let exported = meter.flush(&sink).await.unwrap();
        assert_eq!(exported, 2);

        {
            let batches = sink.batches.lock().unwrap();
            assert_eq!(batches.len(), 1);
            assert_eq!(batches[0].len(), 2);
            assert_eq!(batches[0][0].tenant_id, "tenant-a");
            assert_eq!(batches[0][0].quantity, 15);
            assert_eq!(batches[0][1].tenant_id, "tenant-b");
            assert_eq!(batches[0][1].quantity, 7);
        }

        // Counters are reset after flush; a second flush is a no-op.
        assert_eq!(meter.get_usage("tenant-a", "api_calls").await.unwrap(), 0);
        assert_eq!(meter.get_usage("tenant-b", "api_calls").await.unwrap(), 0);
        assert_eq!(meter.flush(&sink).await.unwrap(), 0);
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sql_usage_meter_roundtrip_on_sqlite() {
        use rume_data::persistence::SqliteBackend;
        use std::sync::Arc;

        let backend = Arc::new(SqliteBackend::in_memory().await.unwrap());
        let meter = SqlUsageMeter::new(backend);
        meter.ensure_schema().await.unwrap();

        meter.record_usage("t1", "tokens", 100).await.unwrap();
        meter.record_usage("t1", "tokens", 25).await.unwrap();
        assert_eq!(meter.get_usage("t1", "tokens").await.unwrap(), 125);

        let sink = MockBillingSink::default();
        assert_eq!(meter.flush(&sink).await.unwrap(), 1);
        assert_eq!(meter.get_usage("t1", "tokens").await.unwrap(), 0);
        assert_eq!(meter.flush(&sink).await.unwrap(), 0);
    }
}
