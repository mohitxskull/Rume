use rume::persistence::DatabaseTransaction;
use rume::persistence::error::PersistenceError;
use rume::persistence::row::{DbRow, FromRow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub created_at: i64,
}

impl FromRow for Item {
    fn from_row(row: &DbRow) -> Result<Self, PersistenceError> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            created_at: row.get("created_at")?,
        })
    }
}

pub struct ItemRepository;

impl ItemRepository {
    pub async fn create_in_tx<T: DatabaseTransaction>(
        tx: &T,
        item: &Item,
    ) -> Result<(), PersistenceError> {
        crate::queries::items::CREATE
            .execute(tx, &rume::params![&item.id, &item.name, item.created_at])
            .await
            .map(|_| ())
    }

    pub async fn find_in_tx<T: DatabaseTransaction>(
        tx: &T,
        id: &str,
    ) -> Result<Option<Item>, PersistenceError> {
        crate::queries::items::FIND
            .fetch_optional_tx_as(tx, &rume::params![id])
            .await
    }

    pub async fn list_in_tx<T: DatabaseTransaction>(tx: &T) -> Result<Vec<Item>, PersistenceError> {
        crate::queries::items::LIST.fetch_all_tx_as(tx, &[]).await
    }
}
