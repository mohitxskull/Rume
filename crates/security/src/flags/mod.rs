//! Feature flag evaluation and persistence for Rume.

pub mod eval;
pub mod sql;
pub mod store;

pub use eval::{
    EvaluationContext, FeatureFlag, RolloutPercentage, TargetingRule, bucket_percentage,
    evaluate_flag,
};
pub use sql::SqlFeatureFlags;
pub use store::{BoxFuture, FeatureFlagStore, FlagError, InMemoryFeatureFlags};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_flag_targeting_and_sha256_rollout_are_deterministic() {
        // Explicit tenant/user rules override a 0% rollout.
        let flag = FeatureFlag::new("beta_dashboard", true)
            .with_rollout(0)
            .with_rule(TargetingRule::Tenant("tenant-vip".into()))
            .with_rule(TargetingRule::User("user-beta".into()));

        assert!(evaluate_flag(
            &flag,
            &EvaluationContext::new().with_tenant("tenant-vip")
        ));
        assert!(evaluate_flag(
            &flag,
            &EvaluationContext::new().with_user("user-beta")
        ));
        assert!(!evaluate_flag(
            &flag,
            &EvaluationContext::new()
                .with_tenant("tenant-other")
                .with_user("user-other")
        ));

        // Disabled flag always returns false even with matching rules.
        let disabled = FeatureFlag::new("beta_dashboard", false)
            .with_rollout(100)
            .with_rule(TargetingRule::User("user-beta".into()));
        assert!(!evaluate_flag(
            &disabled,
            &EvaluationContext::new().with_user("user-beta")
        ));

        // Deterministic SHA-256 rollout: repeated evaluations for the same
        // (flag, subject) always agree.
        let rollout_flag = FeatureFlag::new("gradual_rollout", true).with_rollout(35);
        for i in 0..50 {
            let subject = format!("user-{i}");
            let ctx = EvaluationContext::new().with_user(&subject);
            let first = evaluate_flag(&rollout_flag, &ctx);
            for _ in 0..10 {
                assert_eq!(evaluate_flag(&rollout_flag, &ctx), first);
            }
        }
        assert_eq!(
            bucket_percentage("gradual_rollout", "user-42"),
            bucket_percentage("gradual_rollout", "user-42")
        );
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sql_feature_flags_roundtrip_on_sqlite() {
        use rume_data::persistence::SqliteBackend;
        use std::sync::Arc;

        let backend = Arc::new(SqliteBackend::in_memory().await.unwrap());
        let store = SqlFeatureFlags::new(backend);
        store.ensure_schema().await.unwrap();

        let global = FeatureFlag::new("dark_mode", true)
            .with_rollout(0)
            .with_rule(TargetingRule::User("alice".into()));
        store.upsert_flag(global.clone()).await.unwrap();

        let loaded = store.get_flag("dark_mode", None).await.unwrap().unwrap();
        assert_eq!(loaded, global);

        assert!(
            store
                .evaluate("dark_mode", &EvaluationContext::new().with_user("alice"))
                .await
                .unwrap()
        );
        assert!(
            !store
                .evaluate("dark_mode", &EvaluationContext::new().with_user("bob"))
                .await
                .unwrap()
        );

        // Tenant-specific override takes precedence over global flag.
        let tenant_override = FeatureFlag::new("dark_mode", true)
            .with_tenant_id("acme")
            .with_rollout(100);
        store.upsert_flag(tenant_override.clone()).await.unwrap();

        assert!(
            store
                .evaluate(
                    "dark_mode",
                    &EvaluationContext::new()
                        .with_tenant("acme")
                        .with_user("bob")
                )
                .await
                .unwrap()
        );

        let tenant_flags = store.list_flags(Some("acme")).await.unwrap();
        assert_eq!(tenant_flags, vec![tenant_override]);

        assert!(store.delete_flag("dark_mode", Some("acme")).await.unwrap());
        assert!(
            store
                .get_flag("dark_mode", Some("acme"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(store.delete_flag("dark_mode", None).await.unwrap());
        assert!(!store.delete_flag("dark_mode", None).await.unwrap());
    }
}
