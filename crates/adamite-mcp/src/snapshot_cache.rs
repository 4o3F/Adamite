use crate::McpError;
use adamite_access::Source;
use adamite_core::scan::ScanOutput;
use adamite_format::ScanTime;
use adamite_io::limits::ScanLimits;
use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::SystemTime,
};

/// Serializes snapshot decoding across all cache instances and paths in this process.
static LOAD_PERMIT: Mutex<()> = Mutex::new(());

type LoadResult = Result<Arc<ScanOutput>, McpError>;

/// A blocking, canonical-path LRU cache of shared decoded snapshots.
///
/// Concurrent loads of one path share their result, including failures. Failures are
/// removed from the cache so later requests retry. Active outputs may keep the cache
/// above capacity until another access can evict them. Decoding holds no cache lock.
#[derive(Debug)]
pub struct SnapshotCache {
    capacity: usize,
    state: Mutex<CacheState>,
}

#[derive(Debug, Default)]
struct CacheState {
    entries: VecDeque<CachedSnapshot>,
    loading: HashMap<PathBuf, Arc<PendingLoad>>,
}

#[derive(Debug)]
struct CachedSnapshot {
    path: PathBuf,
    fingerprint: Fingerprint,
    output: Arc<ScanOutput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    pub(crate) byte_len: u64,
    pub(crate) modified: SystemTime,
}

#[derive(Debug, Default)]
struct PendingLoad {
    result: Mutex<Option<LoadResult>>,
    ready: Condvar,
}

impl Default for SnapshotCache {
    fn default() -> Self {
        Self::new(2)
    }
}

impl SnapshotCache {
    /// Sets the retained entry capacity; zero shares in-flight loads without retaining results.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(CacheState::default()),
        }
    }

    /// Loads an `.adm` file or reuses its output when length and modification time agree.
    ///
    /// Symbolic links share the canonical target's cache entry. This method blocks and
    /// is suitable for a caller's blocking worker pool.
    ///
    /// # Errors
    /// Returns unsupported-source, permission, I/O, decoding or synchronization failures.
    pub fn load(&self, path: &Path) -> LoadResult {
        Self::check_extension(path)?;
        let canonical = fs::canonicalize(path).map_err(|source| McpError::io(path, source))?;
        let fingerprint = Fingerprint::read(&canonical)?;
        let pending = {
            let mut state = self.state.lock().map_err(McpError::poisoned)?;
            if let Some(pending) = state.loading.get(&canonical) {
                let pending = Arc::clone(pending);
                drop(state);
                return pending.wait();
            }
            if let Some(output) = state.hit(&canonical, fingerprint) {
                state.trim(self.capacity);
                return Ok(output);
            }
            let pending = Arc::new(PendingLoad::default());
            state
                .loading
                .insert(canonical.clone(), Arc::clone(&pending));
            pending
        };

        let slot = LoadSlot {
            cache: self,
            canonical,
            pending,
        };
        let result = Self::decode(path, &slot.canonical);
        slot.finish(result)
    }

    pub(crate) fn scan_time(
        &self,
        path: &Path,
        fingerprint: Fingerprint,
    ) -> Result<ScanTime, McpError> {
        let canonical = fs::canonicalize(path).map_err(|source| McpError::io(path, source))?;
        let state = self.state.lock().map_err(McpError::poisoned)?;
        Ok(ScanTime(
            state
                .entries
                .iter()
                .find(|entry| entry.path == canonical && entry.fingerprint == fingerprint)
                .and_then(|entry| entry.output.snapshot.volume().context().scanned_at_filetime),
        ))
    }

    pub(crate) fn check_extension(path: &Path) -> Result<(), McpError> {
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("adm"))
        {
            Ok(())
        } else {
            Err(McpError::UnsupportedSource {
                path: path.to_owned(),
            })
        }
    }

    fn decode(path: &Path, canonical: &Path) -> Result<(Arc<ScanOutput>, Fingerprint), McpError> {
        let _permit = LOAD_PERMIT.lock().map_err(McpError::poisoned)?;
        let fingerprint = Fingerprint::read(canonical)?;
        let output = Source::open_snapshot(path)
            .and_then(|source| source.load_observed(&ScanLimits::default(), |_| {}))
            .map_err(|source| McpError::source(path.to_owned(), source))?;
        Ok((Arc::new(output), fingerprint))
    }
}

impl CacheState {
    fn hit(&mut self, path: &Path, fingerprint: Fingerprint) -> Option<Arc<ScanOutput>> {
        let index = self.entries.iter().position(|entry| entry.path == path)?;
        let entry = self.entries.remove(index)?;
        if entry.fingerprint != fingerprint {
            return None;
        }
        let output = Arc::clone(&entry.output);
        self.entries.push_back(entry);
        Some(output)
    }

    fn trim(&mut self, capacity: usize) {
        while self.entries.len() > capacity {
            let Some(index) = self
                .entries
                .iter()
                .position(|entry| Arc::strong_count(&entry.output) == 1)
            else {
                break;
            };
            self.entries.remove(index);
        }
    }
}

impl Fingerprint {
    fn read(path: &Path) -> Result<Self, McpError> {
        let metadata = fs::metadata(path).map_err(|source| McpError::io(path, source))?;
        Ok(Self {
            byte_len: metadata.len(),
            modified: metadata
                .modified()
                .map_err(|source| McpError::io(path, source))?,
        })
    }
}

/// Publishes one in-flight load; dropping it unfinished (a panicking decoder) still wakes waiters.
struct LoadSlot<'a> {
    cache: &'a SnapshotCache,
    canonical: PathBuf,
    pending: Arc<PendingLoad>,
}

impl LoadSlot<'_> {
    fn finish(self, result: Result<(Arc<ScanOutput>, Fingerprint), McpError>) -> LoadResult {
        let mut state = self.cache.state.lock().map_err(McpError::poisoned)?;
        if let Ok((output, fingerprint)) = &result {
            if self.cache.capacity > 0 {
                state.entries.push_back(CachedSnapshot {
                    path: self.canonical.clone(),
                    fingerprint: *fingerprint,
                    output: Arc::clone(output),
                });
            }
            state.trim(self.cache.capacity);
        }
        drop(state);
        let result = result.map(|(output, _)| output);
        self.publish(result.clone());
        result
    }

    fn publish(&self, result: LoadResult) {
        let mut state = self
            .cache
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut slot = self
            .pending
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(result);
        }
        if state
            .loading
            .get(&self.canonical)
            .is_some_and(|current| Arc::ptr_eq(current, &self.pending))
        {
            state.loading.remove(&self.canonical);
        }
        self.pending.ready.notify_all();
    }
}

impl Drop for LoadSlot<'_> {
    fn drop(&mut self) {
        self.publish(Err(McpError::io(
            self.canonical.clone(),
            std::io::Error::other("snapshot load was abandoned"),
        )));
    }
}

impl PendingLoad {
    fn wait(&self) -> LoadResult {
        let mut result = self.result.lock().map_err(McpError::poisoned)?;
        loop {
            if let Some(result) = &*result {
                return result.clone();
            }
            result = self.ready.wait(result).map_err(McpError::poisoned)?;
        }
    }
}

#[cfg(test)]
mod tests;
