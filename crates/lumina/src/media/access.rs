use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{ensure, Result};
use uuid::Uuid;

use super::{MediaResultRef, VerifiedMediaContent};

pub const MAX_MEDIA_ACCESS_TICKETS: usize = 256;
pub const MAX_MEDIA_ACCESS_LIFETIME: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaAccessPurpose {
    Preview,
    Open,
    Reveal,
    CopyPath,
    SaveAs,
}

pub struct MediaAccessTicket {
    pub ticket: String,
    pub expires_in_ms: u64,
}

impl fmt::Debug for MediaAccessTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MediaAccessTicket")
            .field("ticket", &"[redacted]")
            .field("expires_in_ms", &self.expires_in_ms)
            .finish()
    }
}

pub struct MediaAccessRequest<'a> {
    pub ticket: &'a str,
    pub session_id: &'a str,
    pub result_id: &'a str,
    pub purpose: MediaAccessPurpose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaTicketError;

impl fmt::Display for MediaTicketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Media access is unavailable")
    }
}

impl std::error::Error for MediaTicketError {}

#[derive(Clone)]
pub struct MediaAccessStore {
    inner: Arc<Inner>,
}

/// A connection owns this handle. Session activation and transport authentication
/// must be checked by the caller; possessing a store does not establish either.
pub struct MediaAccessOwner {
    store: MediaAccessStore,
    id: Uuid,
    alive: Arc<AtomicBool>,
}

pub struct MediaAccessDisconnectGuard {
    store: MediaAccessStore,
    owner_id: Uuid,
    alive: Arc<AtomicBool>,
}

/// An already-authorized response owns this lease until completion or cancellation.
pub struct MediaContentLease {
    entry: Arc<Entry>,
    result: MediaResultRef,
}

impl MediaContentLease {
    pub fn result(&self) -> &MediaResultRef {
        &self.result
    }

    pub fn read_range(&self, offset: u64, length: usize) -> Result<Vec<u8>> {
        self.entry
            .content
            .lock()
            .map_err(|_| MediaTicketError)?
            .read_range(offset, length)
    }
}

pub struct MediaLocalActionLease {
    _entry: Arc<Entry>,
    result: MediaResultRef,
    path: std::path::PathBuf,
}

impl MediaLocalActionLease {
    pub fn result(&self) -> &MediaResultRef {
        &self.result
    }

    /// The authenticated native action must retain this lease through completion.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

struct Inner {
    entries: Mutex<HashMap<Uuid, Arc<Entry>>>,
    allocated: Arc<AtomicUsize>,
    capacity: usize,
    lifetime: Duration,
    closed: AtomicBool,
    now: Box<dyn Fn() -> Instant + Send + Sync>,
}

struct Entry {
    owner_id: Uuid,
    owner: Weak<AtomicBool>,
    session_id: String,
    result_id: String,
    purpose: MediaAccessPurpose,
    expires_at: Instant,
    revoked: AtomicBool,
    content: Mutex<VerifiedMediaContent>,
    _slot: TicketSlot,
}

struct TicketSlot(Arc<AtomicUsize>);

impl Entry {
    fn available(&self, now: Instant) -> bool {
        !self.revoked.load(Ordering::Acquire)
            && self
                .owner
                .upgrade()
                .is_some_and(|owner| owner.load(Ordering::Acquire))
            && now < self.expires_at
    }

    fn matches(&self, request: &MediaAccessRequest<'_>) -> bool {
        self.session_id == request.session_id
            && self.result_id == request.result_id
            && self.purpose == request.purpose
    }
}

impl Drop for TicketSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl MediaAccessStore {
    pub fn new(capacity: usize, lifetime: Duration) -> Result<Self> {
        Self::with_clock(capacity, lifetime, Instant::now)
    }

    fn with_clock(
        capacity: usize,
        lifetime: Duration,
        now: impl Fn() -> Instant + Send + Sync + 'static,
    ) -> Result<Self> {
        ensure!(
            (1..=MAX_MEDIA_ACCESS_TICKETS).contains(&capacity),
            "Media access capacity is outside its bounds"
        );
        ensure!(
            lifetime >= Duration::from_millis(1) && lifetime <= MAX_MEDIA_ACCESS_LIFETIME,
            "Media access lifetime is outside its bounds"
        );
        Ok(Self {
            inner: Arc::new(Inner {
                entries: Mutex::new(HashMap::new()),
                allocated: Arc::new(AtomicUsize::new(0)),
                capacity,
                lifetime,
                closed: AtomicBool::new(false),
                now: Box::new(now),
            }),
        })
    }

    pub fn create_owner(&self) -> MediaAccessOwner {
        MediaAccessOwner {
            store: self.clone(),
            id: Uuid::new_v4(),
            alive: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Call periodically while the store is idle to release expired file guards.
    /// Every access also checks expiry; timer delays cannot extend authorization.
    pub fn reap_expired(&self) {
        let now = (self.inner.now)();
        self.remove_where(|_, entry| !entry.available(now));
    }

    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::Release);
        self.remove_where(|_, _| true);
    }

    pub fn claim_content(&self, request: &MediaAccessRequest<'_>) -> Result<MediaContentLease> {
        if !matches!(
            request.purpose,
            MediaAccessPurpose::Preview | MediaAccessPurpose::SaveAs
        ) {
            return Err(MediaTicketError.into());
        }
        let entry = self.resolve(request)?;
        let result = {
            let content = entry.content.lock().map_err(|_| MediaTicketError)?;
            if !entry.available((self.inner.now)()) || self.inner.closed.load(Ordering::Acquire) {
                return Err(MediaTicketError.into());
            }
            content.result().clone()
        };
        Ok(MediaContentLease { entry, result })
    }

    pub fn claim_local_action(
        &self,
        request: &MediaAccessRequest<'_>,
    ) -> Result<MediaLocalActionLease> {
        if !matches!(
            request.purpose,
            MediaAccessPurpose::Open | MediaAccessPurpose::Reveal | MediaAccessPurpose::CopyPath
        ) {
            return Err(MediaTicketError.into());
        }
        let entry = self.resolve(request)?;
        let (result, path) = {
            let content = entry.content.lock().map_err(|_| MediaTicketError)?;
            if !entry.available((self.inner.now)()) || self.inner.closed.load(Ordering::Acquire) {
                return Err(MediaTicketError.into());
            }
            // Native actions are one-shot. The active response retains the Arc,
            // so removing the ticket cannot release its filesystem protection.
            let id = Uuid::parse_str(request.ticket).map_err(|_| MediaTicketError)?;
            self.remove_where(|candidate, current| {
                *candidate == id && std::ptr::eq(current, &*entry)
            });
            (content.result().clone(), content.path().to_owned())
        };
        Ok(MediaLocalActionLease {
            _entry: entry,
            result,
            path,
        })
    }

    pub fn read_range(
        &self,
        request: &MediaAccessRequest<'_>,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>> {
        if !matches!(
            request.purpose,
            MediaAccessPurpose::Preview | MediaAccessPurpose::SaveAs
        ) {
            return Err(MediaTicketError.into());
        }
        let entry = self.resolve(request)?;
        let mut content = entry.content.lock().map_err(|_| MediaTicketError)?;
        if !entry.available((self.inner.now)()) {
            return Err(MediaTicketError.into());
        }
        content.read_range(offset, length)
    }

    /// Only the authenticated local host should call this for a desktop action.
    /// Complete the synchronous action inside the closure: a returned path or
    /// future does not carry authorization or keep the file guard alive.
    /// This primitive does not implement cross-process completion receipts.
    pub fn with_local_path<T>(
        &self,
        request: &MediaAccessRequest<'_>,
        action: impl FnOnce(&Path) -> Result<T>,
    ) -> Result<T> {
        if !matches!(
            request.purpose,
            MediaAccessPurpose::Open | MediaAccessPurpose::Reveal | MediaAccessPurpose::CopyPath
        ) {
            return Err(MediaTicketError.into());
        }
        let entry = self.resolve(request)?;
        let content = entry.content.lock().map_err(|_| MediaTicketError)?;
        if !entry.available((self.inner.now)()) {
            return Err(MediaTicketError.into());
        }
        action(content.path())
    }

    fn resolve(&self, request: &MediaAccessRequest<'_>) -> Result<Arc<Entry>> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(MediaTicketError.into());
        }
        self.reap_expired();
        let id = Uuid::parse_str(request.ticket).map_err(|_| MediaTicketError)?;
        let entries = self.inner.entries.lock().map_err(|_| MediaTicketError)?;
        let entry = entries.get(&id).ok_or(MediaTicketError)?;
        if !entry.matches(request) || !entry.available((self.inner.now)()) {
            return Err(MediaTicketError.into());
        }
        Ok(Arc::clone(entry))
    }

    fn remove_where(&self, predicate: impl Fn(&Uuid, &Entry) -> bool) {
        let removed = {
            // Cleanup must still revoke capabilities if a previous operation
            // panicked; no external action runs while this mutex is held.
            let mut entries = self
                .inner
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let ids: Vec<_> = entries
                .iter()
                .filter_map(|(id, entry)| predicate(id, entry).then_some(*id))
                .collect();
            ids.into_iter()
                .filter_map(|id| entries.remove(&id))
                .inspect(|entry| entry.revoked.store(true, Ordering::Release))
                .collect::<Vec<_>>()
        };
        // The final Arc may close several file/directory handles. Drop it after
        // releasing the map mutex, including during owner teardown.
        drop(removed);
    }
}

impl MediaAccessOwner {
    pub fn issue(
        &self,
        content: VerifiedMediaContent,
        session_id: &str,
        result_id: &str,
        purpose: MediaAccessPurpose,
    ) -> Result<MediaAccessTicket> {
        if content.result().session_id != session_id || content.result().result_id != result_id {
            return Err(MediaTicketError.into());
        }
        self.store.reap_expired();
        let mut entries = self
            .store
            .inner
            .entries
            .lock()
            .map_err(|_| MediaTicketError)?;
        if !self.alive.load(Ordering::Acquire) || self.store.inner.closed.load(Ordering::Acquire) {
            return Err(MediaTicketError.into());
        }
        ensure!(
            self.store.inner.allocated.load(Ordering::Acquire) < self.store.inner.capacity,
            "Media access ticket capacity reached"
        );
        let mut id = Uuid::new_v4();
        while entries.contains_key(&id) {
            id = Uuid::new_v4();
        }
        let expires_at = (self.store.inner.now)() + self.store.inner.lifetime;
        self.store.inner.allocated.fetch_add(1, Ordering::AcqRel);
        entries.insert(
            id,
            Arc::new(Entry {
                owner_id: self.id,
                owner: Arc::downgrade(&self.alive),
                session_id: session_id.to_owned(),
                result_id: result_id.to_owned(),
                purpose,
                expires_at,
                revoked: AtomicBool::new(false),
                content: Mutex::new(content),
                _slot: TicketSlot(Arc::clone(&self.store.inner.allocated)),
            }),
        );
        Ok(MediaAccessTicket {
            ticket: id.to_string(),
            expires_in_ms: self.store.inner.lifetime.as_millis() as u64,
        })
    }

    /// Unknown, expired, and other owners' tickets are indistinguishable no-ops.
    pub fn release(&self, ticket: &str) {
        let Ok(ticket) = Uuid::parse_str(ticket) else {
            return;
        };
        self.store
            .remove_where(|id, entry| *id == ticket && entry.owner_id == self.id);
    }

    pub fn revoke_session(&self, session_id: &str) {
        self.store
            .remove_where(|_, entry| entry.owner_id == self.id && entry.session_id == session_id);
    }

    pub fn close(&self) {
        self.alive.store(false, Ordering::Release);
        self.store
            .remove_where(|_, entry| entry.owner_id == self.id);
    }

    pub fn disconnect_guard(&self) -> MediaAccessDisconnectGuard {
        MediaAccessDisconnectGuard {
            store: self.store.clone(),
            owner_id: self.id,
            alive: Arc::clone(&self.alive),
        }
    }
}

impl Drop for MediaAccessOwner {
    fn drop(&mut self) {
        self.close();
    }
}

impl Drop for MediaAccessDisconnectGuard {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        self.store
            .remove_where(|_, entry| entry.owner_id == self.owner_id);
    }
}

#[cfg(all(test, windows))]
#[path = "access_tests.rs"]
mod tests;
