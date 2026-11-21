use super::*;

/// An RAII transaction guard that automatically rolls back the transaction on Drop
/// unless explicitly committed.
///
/// For SQLite transactions (or any driver dropped on a `#[tokio::test(flavor = "multi_thread")]`
/// runtime), `Drop` completes rollback synchronously (5s timeout). Synchronous drop rollback
/// barriers for Postgres/MySQL transactions require `#[tokio::test(flavor = "multi_thread")]`
/// or calling `.rollback().await` / `.close().await` explicitly.
pub struct TestTransactionFixture<T: DatabaseTransaction + 'static> {
    tx: Option<T>,
    committed: bool,
    #[cfg(feature = "postgres")]
    pending_rollbacks: Option<Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>>,
}

impl<T: DatabaseTransaction + 'static> TestTransactionFixture<T> {
    /// Creates a new fixture wrapping an active database transaction.
    pub fn new(tx: T) -> Self {
        Self {
            tx: Some(tx),
            committed: false,
            #[cfg(feature = "postgres")]
            pending_rollbacks: None,
        }
    }

    #[cfg(feature = "postgres")]
    pub(super) fn with_pending_rollbacks(
        tx: T,
        pending_rollbacks: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
    ) -> Self {
        Self {
            tx: Some(tx),
            committed: false,
            pending_rollbacks: Some(pending_rollbacks),
        }
    }

    /// Returns a reference to the active transaction.
    pub fn tx(&self) -> &T {
        self.tx.as_ref().expect("transaction active")
    }

    /// Atomically commits the transaction, consuming the fixture and preventing rollback on Drop.
    pub async fn commit(mut self) -> Result<(), PersistenceError> {
        if let Some(tx) = self.tx.take() {
            self.committed = true;
            tx.commit().await?;
        }
        Ok(())
    }

    /// Discards and rolls back the transaction explicitly, awaiting completion.
    pub async fn rollback(mut self) -> Result<(), PersistenceError> {
        if let Some(tx) = self.tx.take() {
            tx.rollback().await?;
        }
        Ok(())
    }

    /// Discards and rolls back the transaction explicitly, awaiting completion.
    pub async fn close(self) -> Result<(), PersistenceError> {
        self.rollback().await
    }
}

impl<T: DatabaseTransaction + 'static> std::ops::Deref for TestTransactionFixture<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.tx()
    }
}

impl<T: DatabaseTransaction + 'static> std::fmt::Debug for TestTransactionFixture<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestTransactionFixture")
            .field("committed", &self.committed)
            .finish()
    }
}

impl<T: DatabaseTransaction + 'static> Drop for TestTransactionFixture<T> {
    fn drop(&mut self) {
        if !self.committed
            && let Some(tx) = self.tx.take()
        {
            let timeout = std::time::Duration::from_secs(5);
            match tokio::runtime::Handle::try_current() {
                Ok(handle)
                    if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread =>
                {
                    // Why: Multi-thread Tokio supports `block_in_place` so other worker threads
                    // keep polling the I/O reactor while this thread waits synchronously for
                    // rollback (up to 5s).
                    tokio::task::block_in_place(|| {
                        let _ = handle.block_on(tokio::time::timeout(timeout, tx.rollback()));
                    });
                }
                _ if tx.dialect() == "sqlite" => {
                    // Why: `sqlx-sqlite` runs queries over channels on its own dedicated OS worker
                    // thread rather than Tokio's socket reactor, so awaiting `tx.rollback()` on a
                    // joined helper thread's runtime finishes synchronously without deadlocking
                    // `epoll_wait` on `current_thread` tests or outside Tokio.
                    let _ = std::thread::spawn(move || {
                        if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            rt.block_on(async move {
                                let _ = tokio::time::timeout(timeout, tx.rollback()).await;
                            });
                        }
                    })
                    .join();
                }
                Ok(handle) => {
                    // Why: Outer-reactor TCP sockets (`postgres`, `mysql`) on a `current_thread`
                    // runtime cannot be driven by a joined helper thread because blocking the sole
                    // test thread halts `epoll_wait` and deadlocks. Spawn the rollback on `handle`
                    // and record its `AbortHandle` on any owning `PostgresSchemaFixture` so that if
                    // the fixture is dropped immediately (`drop(tx); drop(fixture);` without an
                    // intervening `.await`), `PostgresSchemaFixture::drop` aborts the unpolled task
                    // and terminates the backend session on the server before synchronously
                    // executing `DROP SCHEMA ... CASCADE`.
                    let jh = handle.spawn(async move {
                        let _ = tx.rollback().await;
                    });
                    #[cfg(feature = "postgres")]
                    if let Some(ref pending) = self.pending_rollbacks
                        && let Ok(mut list) = pending.lock()
                    {
                        list.push(jh.abort_handle());
                    }
                    #[cfg(not(feature = "postgres"))]
                    drop(jh);
                }
                Err(_) => {
                    // Outside any Tokio runtime, TCP sockets have no active reactor to drive
                    // `rollback().await`; dropping `tx` falls back to SQLx's connection cleanup.
                }
            }
        }
    }
}
