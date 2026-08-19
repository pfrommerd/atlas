use std::{fmt, sync::Arc};

use async_trait::async_trait;
use jj_lib::{
    object_id::ObjectId,
    op_heads_store::{OpHeadsStore, OpHeadsStoreError, OpHeadsStoreLock},
    op_store::OperationId,
};

use crate::{RepositoryId, atlas_op_store::CheckoutObjectStore, repository::CheckoutId};

/// Checkout-local operation heads stored by the daemon. Updates deliberately
/// do not take a process-wide lock: adding the new head and removing the
/// observed heads is one database transaction, so concurrent updates preserve
/// both results for jj's normal divergent-operation resolution.
pub struct AtlasOpHeadsStore {
    repository_id: RepositoryId,
    checkout_id: CheckoutId,
    objects: Arc<dyn CheckoutObjectStore>,
}

impl AtlasOpHeadsStore {
    pub const NAME: &'static str = "atlas";

    pub fn new(
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        objects: Arc<dyn CheckoutObjectStore>,
    ) -> Self {
        Self {
            repository_id,
            checkout_id,
            objects,
        }
    }
}

impl fmt::Debug for AtlasOpHeadsStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AtlasOpHeadsStore")
            .field("repository_id", &self.repository_id)
            .field("checkout_id", &self.checkout_id)
            .finish()
    }
}

struct ConcurrentCompatibleLock;
impl OpHeadsStoreLock for ConcurrentCompatibleLock {}

#[async_trait]
impl OpHeadsStore for AtlasOpHeadsStore {
    fn name(&self) -> &str {
        Self::NAME
    }

    async fn update_op_heads(
        &self,
        old_ids: &[OperationId],
        new_id: &OperationId,
    ) -> Result<(), OpHeadsStoreError> {
        let old_ids: Vec<_> = old_ids.iter().map(|id| id.as_bytes().to_vec()).collect();
        self.objects
            .update_op_heads(
                self.repository_id,
                self.checkout_id,
                &old_ids,
                new_id.as_bytes(),
            )
            .await
            .map_err(|source| OpHeadsStoreError::Write {
                new_op_id: new_id.clone(),
                source,
            })
    }

    async fn get_op_heads(&self) -> Result<Vec<OperationId>, OpHeadsStoreError> {
        let heads = self
            .objects
            .op_heads(self.repository_id, self.checkout_id)
            .await
            .map_err(OpHeadsStoreError::Read)?;
        if heads.is_empty() {
            return Err(OpHeadsStoreError::Read(
                "Atlas checkout has no operation head".into(),
            ));
        }
        Ok(heads.into_iter().map(OperationId::new).collect())
    }

    async fn lock(&self) -> Result<Box<dyn OpHeadsStoreLock + '_>, OpHeadsStoreError> {
        Ok(Box::new(ConcurrentCompatibleLock))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::RepositoryDatabase;

    #[tokio::test]
    async fn concurrent_updates_preserve_divergent_heads() {
        let directory = tempfile::tempdir().unwrap();
        let database =
            Arc::new(RepositoryDatabase::open(directory.path().join("repositories.redb")).unwrap());
        let repository_id = uuid::Uuid::new_v4();
        let checkout_id = CheckoutId(uuid::Uuid::new_v4());
        let root = OperationId::new(vec![0; 32]);
        database
            .update_checkout_op_heads(repository_id, checkout_id, &[], root.as_bytes())
            .unwrap();
        let store = Arc::new(AtlasOpHeadsStore::new(repository_id, checkout_id, database));
        let first = OperationId::new(vec![1; 32]);
        let second = OperationId::new(vec![2; 32]);
        let (first_result, second_result) = tokio::join!(
            store.update_op_heads(std::slice::from_ref(&root), &first),
            store.update_op_heads(std::slice::from_ref(&root), &second),
        );
        first_result.unwrap();
        second_result.unwrap();
        let mut heads = store.get_op_heads().await.unwrap();
        heads.sort();
        assert_eq!(heads, vec![first, second]);
    }
}
