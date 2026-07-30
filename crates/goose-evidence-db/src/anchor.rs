use std::sync::{Arc, Mutex};

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorState {
    Stable,
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorHeadRecord {
    pub state: AnchorState,
    pub checkpoint_sequence: u64,
    pub freeze_journal_sequence: u64,
    pub root_hash: [u8; 32],
    pub prepare_token: Option<[u8; 32]>,
}

#[derive(Debug, Error)]
pub enum AnchorStoreError {
    #[error("anchor_store_unavailable")]
    Unavailable,
    #[error("anchor_store_corrupt")]
    Corrupt,
}

pub trait AnchorStore: Send + Sync {
    fn load_head(&self) -> Result<Option<AnchorHeadRecord>, AnchorStoreError>;

    fn compare_and_swap_head(
        &self,
        expected: Option<&AnchorHeadRecord>,
        replacement: &AnchorHeadRecord,
    ) -> Result<bool, AnchorStoreError>;
}

#[derive(Debug, Default, Clone)]
pub struct MemoryAnchorStore {
    inner: Arc<Mutex<Option<AnchorHeadRecord>>>,
}

impl MemoryAnchorStore {
    pub fn new(initial: Option<AnchorHeadRecord>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(initial)),
        }
    }
}

impl AnchorStore for MemoryAnchorStore {
    fn load_head(&self) -> Result<Option<AnchorHeadRecord>, AnchorStoreError> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| AnchorStoreError::Unavailable)?;
        Ok(guard.clone())
    }

    fn compare_and_swap_head(
        &self,
        expected: Option<&AnchorHeadRecord>,
        replacement: &AnchorHeadRecord,
    ) -> Result<bool, AnchorStoreError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| AnchorStoreError::Unavailable)?;
        if guard.as_ref() != expected {
            return Ok(false);
        }
        *guard = Some(replacement.clone());
        Ok(true)
    }
}
