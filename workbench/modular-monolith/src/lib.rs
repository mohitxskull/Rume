//! Scenario 2: A modular monolith with independent business domains.
//!
//! Demonstrates:
//! - Clear domain boundaries (`catalog` and `orders` modules).
//! - Explicit module dependency declaration (`orders` depends on `catalog`).
//! - Decoupled inter-module communication via asynchronous events (`OrderPlacedEvent`).
//! - Topological startup order and reverse-order shutdown hooks.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use rume::app::Rume;
use rume::app::builder::RumeBuilder;
use rume::events::{Event, EventBus};
use rume::module::{BoxFuture, Module, ModuleContext, ModuleHookResult};
use rume::routing::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

// ------------------------------------------------------------------------------------------------
// Inter-Module Domain Events
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderPlacedEvent {
    pub order_id: String,
    pub product_id: String,
    pub quantity: u32,
}

impl Event for OrderPlacedEvent {
    const TYPE: &'static str = "orders.order_placed";
}

// ------------------------------------------------------------------------------------------------
// Domain 1: Catalog Module
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Product {
    pub id: String,
    pub name: String,
    pub stock: u32,
    pub price_cents: u64,
}

#[derive(Clone)]
pub struct CatalogState {
    pub products: Arc<RwLock<BTreeMap<String, Product>>>,
    pub boot_log: Arc<RwLock<Vec<String>>>,
}

pub struct CatalogModule {
    state: CatalogState,
    event_bus: Arc<EventBus>,
    event_notify: Option<Arc<tokio::sync::Notify>>,
}

impl CatalogModule {
    pub fn new(event_bus: Arc<EventBus>, boot_log: Arc<RwLock<Vec<String>>>) -> Self {
        let mut initial = BTreeMap::new();
        initial.insert(
            "prod_laptop".to_string(),
            Product {
                id: "prod_laptop".into(),
                name: "Workstation Pro".into(),
                stock: 10,
                price_cents: 199900,
            },
        );
        initial.insert(
            "prod_mouse".to_string(),
            Product {
                id: "prod_mouse".into(),
                name: "Wireless Precision Mouse".into(),
                stock: 50,
                price_cents: 4900,
            },
        );

        Self {
            state: CatalogState {
                products: Arc::new(RwLock::new(initial)),
                boot_log,
            },
            event_bus,
            event_notify: None,
        }
    }

    /// Attaches an optional notification trigger signaled when an order event has been processed.
    pub fn with_event_notify(mut self, notify: Arc<tokio::sync::Notify>) -> Self {
        self.event_notify = Some(notify);
        self
    }
}

impl Module for CatalogModule {
    fn name(&self) -> &str {
        "catalog"
    }

    fn dependencies(&self) -> Vec<&str> {
        Vec::new()
    }

    fn on_start<'a>(&'a self, _ctx: &'a ModuleContext) -> BoxFuture<'a, ModuleHookResult> {
        let boot_log = Arc::clone(&self.state.boot_log);
        let products = Arc::clone(&self.state.products);
        let event_bus = Arc::clone(&self.event_bus);
        let event_notify = self.event_notify.clone();

        Box::pin(async move {
            boot_log.write().await.push("catalog:started".to_string());

            // Decoupled listener: subscribe to OrderPlacedEvent to automatically decrement inventory
            event_bus.subscribe_fn::<OrderPlacedEvent, _, _>(move |event: &OrderPlacedEvent| {
                let products_clone = Arc::clone(&products);
                let pid = event.product_id.clone();
                let qty = event.quantity;
                let notify = event_notify.clone();
                async move {
                    {
                        let mut lock = products_clone.write().await;
                        if let Some(p) = lock.get_mut(&pid) {
                            p.stock = p.stock.saturating_sub(qty);
                        }
                    }
                    if let Some(n) = notify {
                        n.notify_one();
                    }
                    Ok(())
                }
            });

            Ok(())
        })
    }

    fn on_shutdown<'a>(&'a self, _ctx: &'a ModuleContext) -> BoxFuture<'a, ()> {
        let boot_log = Arc::clone(&self.state.boot_log);
        Box::pin(async move {
            boot_log.write().await.push("catalog:stopped".to_string());
        })
    }

    fn register_routes(&self, router: Router) -> Router {
        let catalog_router = Router::new()
            .get("/catalog/products", list_products)
            .get("/catalog/products/{id}", get_product)
            .with_state(self.state.clone());

        router.merge(catalog_router)
    }
}

async fn list_products(State(state): State<CatalogState>) -> Json<Vec<Product>> {
    let prods = state.products.read().await;
    Json(prods.values().cloned().collect())
}

async fn get_product(
    State(state): State<CatalogState>,
    Path(id): Path<String>,
) -> Result<Json<Product>, StatusCode> {
    let prods = state.products.read().await;
    prods
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

// ------------------------------------------------------------------------------------------------
// Domain 2: Orders Module
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Order {
    pub id: String,
    pub product_id: String,
    pub quantity: u32,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateOrderPayload {
    pub product_id: String,
    pub quantity: u32,
}

#[derive(Clone)]
pub struct OrdersState {
    pub orders: Arc<RwLock<BTreeMap<String, Order>>>,
    pub event_bus: Arc<EventBus>,
    pub boot_log: Arc<RwLock<Vec<String>>>,
}

pub struct OrdersModule {
    state: OrdersState,
}

impl OrdersModule {
    pub fn new(event_bus: Arc<EventBus>, boot_log: Arc<RwLock<Vec<String>>>) -> Self {
        Self {
            state: OrdersState {
                orders: Arc::new(RwLock::new(BTreeMap::new())),
                event_bus,
                boot_log,
            },
        }
    }
}

impl Module for OrdersModule {
    fn name(&self) -> &str {
        "orders"
    }

    fn dependencies(&self) -> Vec<&str> {
        vec!["catalog"]
    }

    fn on_start<'a>(&'a self, _ctx: &'a ModuleContext) -> BoxFuture<'a, ModuleHookResult> {
        let boot_log = Arc::clone(&self.state.boot_log);
        Box::pin(async move {
            boot_log.write().await.push("orders:started".to_string());
            Ok(())
        })
    }

    fn on_shutdown<'a>(&'a self, _ctx: &'a ModuleContext) -> BoxFuture<'a, ()> {
        let boot_log = Arc::clone(&self.state.boot_log);
        Box::pin(async move {
            boot_log.write().await.push("orders:stopped".to_string());
        })
    }

    fn register_routes(&self, router: Router) -> Router {
        let orders_router = Router::new()
            .post("/orders", create_order)
            .get("/orders", list_orders)
            .with_state(self.state.clone());

        router.merge(orders_router)
    }
}

async fn create_order(
    State(state): State<OrdersState>,
    Json(payload): Json<CreateOrderPayload>,
) -> (StatusCode, Json<Order>) {
    let order_id = format!("ord_{}", uuid::Uuid::new_v4().simple());
    let order = Order {
        id: order_id.clone(),
        product_id: payload.product_id.clone(),
        quantity: payload.quantity,
        status: "confirmed".into(),
    };

    state
        .orders
        .write()
        .await
        .insert(order_id.clone(), order.clone());

    // Publish inter-module decoupled domain event synchronously
    let _ = state
        .event_bus
        .publish_sync(&OrderPlacedEvent {
            order_id,
            product_id: payload.product_id,
            quantity: payload.quantity,
        })
        .await;

    (StatusCode::CREATED, Json(order))
}

async fn list_orders(State(state): State<OrdersState>) -> Json<Vec<Order>> {
    let orders = state.orders.read().await;
    Json(orders.values().cloned().collect())
}

// ------------------------------------------------------------------------------------------------
// Modular Monolith Application Assembly
// ------------------------------------------------------------------------------------------------

pub fn build_modular_monolith(
    event_bus: Arc<EventBus>,
    boot_log: Arc<RwLock<Vec<String>>>,
) -> RumeBuilder<(), (), ()> {
    build_modular_monolith_instrumented(event_bus, boot_log, None)
}

pub fn build_modular_monolith_instrumented(
    event_bus: Arc<EventBus>,
    boot_log: Arc<RwLock<Vec<String>>>,
    event_notify: Option<Arc<tokio::sync::Notify>>,
) -> RumeBuilder<(), (), ()> {
    let mut catalog = CatalogModule::new(Arc::clone(&event_bus), Arc::clone(&boot_log));
    if let Some(notify) = event_notify {
        catalog = catalog.with_event_notify(notify);
    }
    Rume::new()
        .register_module(catalog)
        .register_module(OrdersModule::new(event_bus, boot_log))
}
