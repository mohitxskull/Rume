//! Milestone F end-to-end integration tests through the `rume` facade.
//!
//! Exercises:
//! 1. `ApplicationMode::Task` with `Rume::new()`, module lifecycle (`on_start` / `on_shutdown`),
//!    task execution with `AppContext`, clean shutdown without binding a listener, and
//!    unknown-task error handling.
//! 2. `ApplicationMode::OpenApi` and `RouteRegistry::to_openapi` / `generate_openapi` with
//!    `#[derive(HasOpenApiSchema)]` + path parameter matchers producing valid OpenAPI `3.1.0` JSON.

use std::sync::{Arc, Mutex};

use garde::Validate;
use rume::module::{BoxFuture, ModuleContext, ModuleHookResult};
use rume::openapi::OpenApiInfo;
use rume::prelude::*;
use serde::Deserialize;

struct LifecycleTrackerModule {
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl Module for LifecycleTrackerModule {
    fn name(&self) -> &'static str {
        "tracker"
    }

    fn on_start<'a>(&'a self, _ctx: &'a ModuleContext) -> BoxFuture<'a, ModuleHookResult> {
        let events = Arc::clone(&self.events);
        Box::pin(async move {
            events.lock().unwrap().push("on_start");
            Ok(())
        })
    }

    fn on_shutdown<'a>(&'a self, _ctx: &'a ModuleContext) -> BoxFuture<'a, ()> {
        let events = Arc::clone(&self.events);
        Box::pin(async move {
            events.lock().unwrap().push("on_shutdown");
        })
    }
}

#[tokio::test]
async fn milestone_f_task_mode_executes_with_context_and_clean_shutdown() {
    let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let task_events = Arc::clone(&events);

    let result = Rume::new()
        .module(LifecycleTrackerModule {
            events: Arc::clone(&events),
        })
        .task(
            "sync_users",
            "Synchronize user records",
            move |ctx| async move {
                assert_eq!(ctx.availability.state(), AvailabilityState::Ready);
                task_events.lock().unwrap().push("task:sync_users");
            },
        )
        .mode(ApplicationMode::Task)
        .task_name("sync_users")
        // Pending signal proves Task mode exits without waiting for a shutdown signal
        // and without binding an HTTP listener.
        .run_with_signal(std::future::pending())
        .await;

    assert!(result.is_ok(), "task mode failed: {result:?}");
    assert_eq!(
        *events.lock().unwrap(),
        vec!["on_start", "task:sync_users", "on_shutdown"]
    );

    // Unknown task returns NotFound and still runs shutdown hooks.
    let err_events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let err_res = Rume::new()
        .module(LifecycleTrackerModule {
            events: Arc::clone(&err_events),
        })
        .mode(ApplicationMode::Task)
        .task_name("missing_task")
        .run_with_signal(std::future::pending())
        .await;

    let err = err_res.expect_err("unknown task must return an error");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(*err_events.lock().unwrap(), vec!["on_start", "on_shutdown"]);
}

#[derive(Debug, Deserialize, Validate, rume::HasOpenApiSchema)]
#[serde(rename_all = "camelCase")]
struct CreateOrderPayload {
    #[garde(length(min = 3, max = 40))]
    customer_id: String,
    #[garde(range(min = 1))]
    quantity: u32,
}

#[tokio::test]
async fn milestone_f_openapi_spec_exports_routes_and_schemas() {
    let info = OpenApiInfo::new("Orders Service", "1.2.0").with_description("Milestone F E2E API");

    // 1. Verify `ApplicationMode::OpenApi` runs preflight and completes cleanly without binding.
    let mode_res = Rume::new()
        .openapi_info(info.clone())
        .route_spec(
            RouteSpec::post("/orders").with_body::<ValidatedJson<CreateOrderPayload>>(),
            |ValidatedJson(_payload): ValidatedJson<CreateOrderPayload>| async { "created" },
        )
        .route_spec(
            RouteSpec::get("/orders/{id}").where_param("id", Matcher::Uuid),
            || async { "order" },
        )
        .mode(ApplicationMode::OpenApi)
        .run_with_signal(std::future::pending())
        .await;
    assert!(mode_res.is_ok(), "openapi mode failed: {mode_res:?}");

    // 2. Verify `RouteRegistry::to_openapi` / `OpenApiSpec` JSON structure.
    let router: rume::routing::Router<()> = rume::routing::Router::new()
        .route_spec(
            RouteSpec::post("/orders").with_body::<ValidatedJson<CreateOrderPayload>>(),
            |ValidatedJson(_payload): ValidatedJson<CreateOrderPayload>| async { "created" },
        )
        .route_spec(
            RouteSpec::get("/orders/{id}").where_param("id", Matcher::Uuid),
            || async { "order" },
        )
        .openapi("/openapi.json", info);

    let app = TestApp::new(router);
    let resp = app.get("/openapi.json").await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);

    let spec_json: serde_json::Value =
        serde_json::from_str(&TestApp::body_text(resp).await).expect("valid OpenAPI JSON");

    assert_eq!(spec_json["openapi"], "3.1.0");
    assert_eq!(spec_json["info"]["title"], "Orders Service");
    assert_eq!(spec_json["info"]["version"], "1.2.0");

    let body_schema = &spec_json["paths"]["/orders"]["post"]["requestBody"]["content"]["application/json"]
        ["schema"];
    assert_eq!(body_schema["type"], "object");
    assert_eq!(body_schema["properties"]["customerId"]["type"], "string");
    assert_eq!(body_schema["properties"]["customerId"]["minLength"], 3);
    assert_eq!(body_schema["properties"]["quantity"]["type"], "integer");

    let id_param = &spec_json["paths"]["/orders/{id}"]["get"]["parameters"][0];
    assert_eq!(id_param["name"], "id");
    assert_eq!(id_param["in"], "path");
    assert_eq!(id_param["required"], true);
    assert_eq!(id_param["schema"]["format"], "uuid");
}
