//! Application bootstrap and lifecycle.

use axum::Router;
use std::net::SocketAddr;
use tokio::net::TcpListener;

/// A configured Rume application.
pub struct Rume<S> {
    _state: std::marker::PhantomData<S>,
}

/// Builder for a [`Rume`] application.
pub struct RumeBuilder<S> {
    state: S,
    router: Router<S>,
    bind: SocketAddr,
}

impl<S> Rume<S>
where
    S: Clone + Send + Sync + 'static,
{
    /// Create a builder for an application with the given state.
    pub fn builder(state: S) -> RumeBuilder<S> {
        RumeBuilder::new(state)
    }
}

impl<S> RumeBuilder<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub fn new(state: S) -> Self {
        Self {
            state,
            router: Router::new(),
            bind: SocketAddr::from(([127, 0, 0, 1], 3000)),
        }
    }

    /// Replace the application router.
    pub fn router(mut self, router: Router<S>) -> Self {
        self.router = router;
        self
    }

    /// Set the address to bind.
    pub fn bind(mut self, addr: SocketAddr) -> Self {
        self.bind = addr;
        self
    }

    /// Run the application until a shutdown signal is received.
    pub async fn run(self) -> Result<(), std::io::Error>
    where
        S: Clone + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(self.bind).await?;
        println!("listening on http://{}", self.bind);
        axum::serve(listener, self.router.with_state(self.state))
            .with_graceful_shutdown(shutdown_signal())
            .await
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
