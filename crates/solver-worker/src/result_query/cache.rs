use crate::{
    calculation_bundle::query_artifact::{QueryArtifactRef, QueryReader, transport},
    storage::ObjectStoreClient,
};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

#[derive(Clone)]
pub struct CacheLimits {
    pub disk_bytes: u64,
    pub memory_bytes: u64,
    pub entries: usize,
    pub preparations: usize,
}
impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            disk_bytes: 20 * 1024 * 1024 * 1024,
            memory_bytes: 2 * 1024 * 1024 * 1024,
            entries: 8,
            preparations: 2,
        }
    }
}

pub struct CachedArtifact {
    // Handles close before the temporary directory is released, including on Windows.
    pub reader: Mutex<QueryReader>,
    _directory: tempfile::TempDir,
    disk_bytes: u64,
    memory_bytes: u64,
}

enum EntryState {
    Preparing { disk: u64, memory: u64 },
    Ready(Arc<CachedArtifact>),
    Failed(Instant),
}
struct Entry {
    reference: QueryArtifactRef,
    used: u64,
    state: EntryState,
}
struct State {
    clock: u64,
    entries: BTreeMap<String, Entry>,
}

pub enum Lookup {
    Ready(Arc<CachedArtifact>),
    Preparing,
    Unavailable,
}

pub struct Cache {
    root: PathBuf,
    _lock: File,
    store: ObjectStoreClient,
    limits: CacheLimits,
    slots: Arc<Semaphore>,
    state: AsyncMutex<State>,
}

impl Cache {
    pub fn new(
        root: &Path,
        store: ObjectStoreClient,
        limits: CacheLimits,
    ) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            limits.entries > 0
                && limits.preparations > 0
                && limits.disk_bytes > 0
                && limits.memory_bytes > 0,
            "cache limits must be positive"
        );
        std::fs::create_dir_all(root)?;
        let marker = root.join(".query-cache-v1");
        if !marker.exists() {
            anyhow::ensure!(
                std::fs::read_dir(root)?.next().is_none(),
                "cache directory must be empty or owned by result-query v1"
            );
            std::fs::write(&marker, b"result-query-v1 ephemeral cache\n")?;
        }
        anyhow::ensure!(
            std::fs::read(&marker)? == b"result-query-v1 ephemeral cache\n",
            "cache ownership marker mismatch"
        );
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(".lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock)?;
        // Only this explicitly owned cache's temp directories are disposable on restart.
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("query-prep-") && entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())?;
            } else {
                anyhow::ensure!(
                    matches!(name.as_ref(), ".lock" | ".query-cache-v1"),
                    "unexpected cache directory content"
                );
            }
        }
        Ok(Arc::new(Self {
            root: root.canonicalize()?,
            _lock: lock,
            store,
            slots: Arc::new(Semaphore::new(limits.preparations)),
            limits,
            state: AsyncMutex::new(State {
                clock: 0,
                entries: BTreeMap::new(),
            }),
        }))
    }

    pub async fn get(self: &Arc<Self>, reference: QueryArtifactRef) -> anyhow::Result<Lookup> {
        reference.validate()?;
        let key = reference.relative_prefix();
        let mut state = self.state.lock().await;
        state.clock = state.clock.wrapping_add(1);
        let clock = state.clock;
        if let Some(entry) = state.entries.get_mut(&key) {
            anyhow::ensure!(
                entry.reference.manifest_byte_size == reference.manifest_byte_size,
                "cached manifest receipt mismatch"
            );
            entry.used = clock;
            match &entry.state {
                EntryState::Ready(artifact) => return Ok(Lookup::Ready(artifact.clone())),
                EntryState::Preparing { .. } => return Ok(Lookup::Preparing),
                EntryState::Failed(at) if at.elapsed() < Duration::from_secs(30) => {
                    return Ok(Lookup::Unavailable);
                }
                EntryState::Failed(_) => {
                    state.entries.remove(&key);
                }
            }
        }
        while state.entries.len() >= self.limits.entries {
            if !evict(&mut state, None) {
                return Ok(Lookup::Unavailable);
            }
        }
        state.entries.insert(
            key.clone(),
            Entry {
                reference: reference.clone(),
                used: clock,
                state: EntryState::Preparing { disk: 0, memory: 0 },
            },
        );
        drop(state);
        let cache = self.clone();
        tokio::spawn(async move {
            // A supervisor clears preparation state even if the task fails or panics.
            let worker = cache.clone();
            let task_key = key.clone();
            let task = tokio::spawn(async move { worker.prepare(&reference, &task_key).await });
            let result = task.await;
            let mut state = cache.state.lock().await;
            if let Some(entry) = state.entries.get_mut(&key) {
                entry.state = if let Ok(Ok(artifact)) = result {
                    EntryState::Ready(Arc::new(artifact))
                } else {
                    tracing::warn!("result query cache preparation failed");
                    EntryState::Failed(Instant::now())
                };
            }
        });
        Ok(Lookup::Preparing)
    }

    async fn prepare(
        &self,
        reference: &QueryArtifactRef,
        key: &str,
    ) -> anyhow::Result<CachedArtifact> {
        let permit = self.slots.clone().acquire_owned().await?;
        let directory = tempfile::Builder::new()
            .prefix("query-prep-")
            .tempdir_in(&self.root)?;
        let staging = directory.path().join("staging");
        std::fs::create_dir(&staging)?;
        let manifest = transport::download_manifest(&self.store, reference, &staging).await?;
        let disk = manifest
            .files
            .iter()
            .try_fold(reference.manifest_byte_size, |sum, file| {
                sum.checked_add(file.byte_size)
                    .ok_or_else(|| anyhow::anyhow!("cache disk size overflow"))
            })?;
        let metadata = manifest.files.iter().try_fold(0_u64, |sum, file| {
            sum.checked_add(file.plain_byte_size.unwrap_or(0))
                .ok_or_else(|| anyhow::anyhow!("cache metadata size overflow"))
        })?;
        // Conservative JSON/container/lookup allowance plus row offsets and fixed HDF5 caches.
        let memory = metadata
            .checked_mul(32)
            .and_then(|bytes| {
                bytes.checked_add(manifest.process_count as u64 * 256 + 32 * 1024 * 1024)
            })
            .ok_or_else(|| anyhow::anyhow!("cache memory size overflow"))?;
        self.reserve(key, disk, memory).await?;
        transport::download_files(&self.store, reference, &manifest, &staging).await?;
        let ready = directory.path().join("ready");
        std::fs::rename(&staging, &ready)?;
        let reference = reference.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let reader = QueryReader::open(&ready, &reference)?;
            Ok(CachedArtifact {
                reader: Mutex::new(reader),
                _directory: directory,
                disk_bytes: disk,
                memory_bytes: memory,
            })
        })
        .await?
    }

    async fn reserve(&self, key: &str, disk: u64, memory: u64) -> anyhow::Result<()> {
        anyhow::ensure!(
            disk <= self.limits.disk_bytes && memory <= self.limits.memory_bytes,
            "artifact exceeds cache capacity"
        );
        let mut state = self.state.lock().await;
        loop {
            let (used_disk, used_memory) = allocated(&state);
            if used_disk <= self.limits.disk_bytes - disk
                && used_memory <= self.limits.memory_bytes - memory
            {
                break;
            }
            anyhow::ensure!(
                evict(&mut state, Some(key)),
                "cache capacity pinned by active requests"
            );
        }
        state
            .entries
            .get_mut(key)
            .ok_or_else(|| anyhow::anyhow!("preparation entry lost"))?
            .state = EntryState::Preparing { disk, memory };
        Ok(())
    }
}

fn allocated(state: &State) -> (u64, u64) {
    state
        .entries
        .values()
        .fold((0, 0), |(disk, memory), entry| match &entry.state {
            EntryState::Preparing { disk: d, memory: m } => {
                (disk.saturating_add(*d), memory.saturating_add(*m))
            }
            EntryState::Ready(artifact) => (
                disk.saturating_add(artifact.disk_bytes),
                memory.saturating_add(artifact.memory_bytes),
            ),
            EntryState::Failed(_) => (disk, memory),
        })
}

fn evict(state: &mut State, except: Option<&str>) -> bool {
    let key = state
        .entries
        .iter()
        .filter(|(key, entry)| {
            except != Some(key.as_str())
                && match &entry.state {
                    EntryState::Ready(artifact) => Arc::strong_count(artifact) == 1,
                    EntryState::Failed(_) => true,
                    EntryState::Preparing { .. } => false,
                }
        })
        .min_by_key(|(_, entry)| entry.used)
        .map(|(key, _)| key.clone());
    if let Some(key) = key {
        state.entries.remove(&key);
        true
    } else {
        false
    }
}
