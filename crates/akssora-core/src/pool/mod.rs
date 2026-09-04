pub mod store;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::session::Session;
pub use store::{PoolEntryRecord, PoolEntryStatus, PoolStatsRecord, PoolStore};

pub trait PoolStrategy: Send + Sync {
    fn target_size(&self, template_id: &str) -> usize;
    fn should_replenish(&self, current_ready: usize, target: usize) -> bool {
        current_ready < target
    }
}

#[derive(Debug, Clone)]
pub struct FixedCapacityStrategy {
    default_capacity: usize,
    template_capacities: HashMap<String, usize>,
}

impl FixedCapacityStrategy {
    pub fn new(default_capacity: usize) -> Self {
        Self {
            default_capacity,
            template_capacities: HashMap::new(),
        }
    }

    pub fn with_template_capacity(
        mut self,
        template_id: impl Into<String>,
        capacity: usize,
    ) -> Self {
        self.template_capacities
            .insert(template_id.into(), capacity);
        self
    }
}

impl Default for FixedCapacityStrategy {
    fn default() -> Self {
        Self::new(2)
    }
}

impl PoolStrategy for FixedCapacityStrategy {
    fn target_size(&self, template_id: &str) -> usize {
        self.template_capacities
            .get(template_id)
            .copied()
            .unwrap_or(self.default_capacity)
    }
}

pub trait WarmVmCreator: Send + Sync {
    fn create_warm_snapshot<'a>(
        &'a self,
        entry_id: &'a str,
        template_id: &'a str,
        storage_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<PoolEntryRecord>> + Send + 'a>>;

    fn restore_warm_session<'a>(
        &'a self,
        entry: &'a PoolEntryRecord,
    ) -> Pin<Box<dyn Future<Output = Result<Session>> + Send + 'a>>;
}

pub struct FirecrackerWarmVmCreator {
    template_store: Option<Arc<crate::templates::TemplateStore>>,
}

impl FirecrackerWarmVmCreator {
    pub fn new(template_store: Option<Arc<crate::templates::TemplateStore>>) -> Self {
        Self { template_store }
    }
}

impl Default for FirecrackerWarmVmCreator {
    fn default() -> Self {
        Self::new(None)
    }
}

impl WarmVmCreator for FirecrackerWarmVmCreator {
    fn create_warm_snapshot<'a>(
        &'a self,
        entry_id: &'a str,
        template_id: &'a str,
        storage_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<PoolEntryRecord>> + Send + 'a>> {
        Box::pin(async move {
            let custom_rootfs = if template_id != "default" && !template_id.is_empty() {
                if let Some(store) = &self.template_store {
                    store.get(template_id)?.map(|r| r.rootfs_path)
                } else {
                    None
                }
            } else {
                None
            };

            let base_dir = storage_dir.join(entry_id);
            tokio::fs::create_dir_all(&base_dir).await?;

            let snapshot_path = base_dir.join("snapshot.snap");
            let mem_file_path = base_dir.join("snapshot.mem");

            let mut session = Session::with_rootfs(custom_rootfs).await?;
            let vsock_uds_path = session.vsock_uds_path.clone();

            // INFO: pause microVM before creating snapshot
            session.pause().await?;
            session
                .create_snapshot(snapshot_path.clone(), mem_file_path.clone())
                .await?;

            // INFO: terminate temporary boot microVM to free system resources
            let _ = session.end().await;

            let now = Utc::now();
            Ok(PoolEntryRecord {
                id: entry_id.to_string(),
                template_id: template_id.to_string(),
                snapshot_path,
                mem_file_path,
                vsock_uds_path,
                status: PoolEntryStatus::Ready,
                created_at: now,
                updated_at: now,
            })
        })
    }

    fn restore_warm_session<'a>(
        &'a self,
        entry: &'a PoolEntryRecord,
    ) -> Pin<Box<dyn Future<Output = Result<Session>> + Send + 'a>> {
        Box::pin(async move {
            let session_id = uuid::Uuid::new_v4();
            Session::from_snapshot(
                session_id,
                entry.snapshot_path.clone(),
                entry.mem_file_path.clone(),
                entry.vsock_uds_path.clone(),
            )
            .await
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PoolMetrics {
    pub hits: u64,
    pub misses: u64,
    pub total_requests: u64,
    pub hit_rate: f64,
    pub ready_count: usize,
    pub claimed_count: usize,
}

#[derive(Clone)]
pub struct WarmPool {
    store: Arc<PoolStore>,
    storage_dir: PathBuf,
    strategy: Arc<dyn PoolStrategy>,
    creator: Arc<dyn WarmVmCreator>,
    hits: Arc<AtomicU64>,
    misses: Arc<AtomicU64>,
    known_templates: Arc<RwLock<HashSet<String>>>,
    dynamic_capacities: Arc<RwLock<HashMap<String, usize>>>,
}

impl WarmPool {
    pub fn new(
        store: PoolStore,
        storage_dir: PathBuf,
        strategy: Arc<dyn PoolStrategy>,
        creator: Arc<dyn WarmVmCreator>,
    ) -> Self {
        let initial_metrics = store.get_metrics().unwrap_or_default();
        let hits = Arc::new(AtomicU64::new(initial_metrics.hits));
        let misses = Arc::new(AtomicU64::new(initial_metrics.misses));

        let mut known = HashSet::new();
        known.insert("default".to_string());

        Self {
            store: Arc::new(store),
            storage_dir,
            strategy,
            creator,
            hits,
            misses,
            known_templates: Arc::new(RwLock::new(known)),
            dynamic_capacities: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn default_pool() -> Result<Self> {
        let store = PoolStore::open_default()?;
        let storage_dir = std::env::temp_dir().join("akssora-pool-snapshots");
        let strategy = Arc::new(FixedCapacityStrategy::default());
        let creator = Arc::new(FirecrackerWarmVmCreator::default());
        Ok(Self::new(store, storage_dir, strategy, creator))
    }

    pub fn with_components(
        store: PoolStore,
        storage_dir: PathBuf,
        strategy: Arc<dyn PoolStrategy>,
        creator: Arc<dyn WarmVmCreator>,
    ) -> Self {
        Self::new(store, storage_dir, strategy, creator)
    }

    pub fn register_template(&self, template_id: impl Into<String>) {
        if let Ok(mut known) = self.known_templates.write() {
            known.insert(template_id.into());
        }
    }

    pub fn list_registered_templates(&self) -> Vec<String> {
        self.known_templates
            .read()
            .map(|t| t.iter().cloned().collect())
            .unwrap_or_else(|_| vec!["default".to_string()])
    }

    #[tracing::instrument(skip(self))]
    pub async fn claim(&self, template_id: Option<&str>) -> Result<Option<Session>> {
        let t_id = template_id.unwrap_or("default");
        self.register_template(t_id);

        let available = self.store.list_available(t_id)?;

        if let Some(entry) = available.into_iter().next() {
            self.store
                .update_status(&entry.id, PoolEntryStatus::Claimed)?;

            let session = self.creator.restore_warm_session(&entry).await?;

            let new_hits = self.hits.fetch_add(1, Ordering::SeqCst) + 1;
            let current_misses = self.misses.load(Ordering::SeqCst);
            let _ = self.store.save_metrics(&PoolStatsRecord {
                hits: new_hits,
                misses: current_misses,
            });

            // INFO: trigger background replenishment after claiming instance
            let pool = self.clone();
            let template_string = t_id.to_string();
            tokio::spawn(async move {
                let _ = pool.replenish(&template_string).await;
            });

            Ok(Some(session))
        } else {
            let new_misses = self.misses.fetch_add(1, Ordering::SeqCst) + 1;
            let current_hits = self.hits.load(Ordering::SeqCst);
            let _ = self.store.save_metrics(&PoolStatsRecord {
                hits: current_hits,
                misses: new_misses,
            });

            // INFO: trigger async replenishment when pool misses
            let pool = self.clone();
            let template_string = t_id.to_string();
            tokio::spawn(async move {
                let _ = pool.replenish(&template_string).await;
            });

            Ok(None)
        }
    }

    // INFO: returns target capacity accounting for runtime scaling overrides
    pub fn target_capacity(&self, template_id: &str) -> usize {
        if let Ok(caps) = self.dynamic_capacities.read()
            && let Some(&cap) = caps.get(template_id)
        {
            cap
        } else {
            self.strategy.target_size(template_id)
        }
    }

    #[tracing::instrument(skip(self))]
    pub async fn replenish(&self, template_id: &str) -> Result<()> {
        let current_ready = self.store.list_available(template_id)?.len();
        let target = self.target_capacity(template_id);

        if self.strategy.should_replenish(current_ready, target) {
            let needed = target.saturating_sub(current_ready);
            for _ in 0..needed {
                let entry_id = uuid::Uuid::new_v4().to_string();
                let record = self
                    .creator
                    .create_warm_snapshot(&entry_id, template_id, &self.storage_dir)
                    .await?;
                self.store.insert(&record)?;
            }
        }

        Ok(())
    }

    // INFO: dynamically scale warm pool standby capacity at runtime
    #[tracing::instrument(skip(self))]
    pub async fn scale(
        &self,
        template_id: Option<&str>,
        target_capacity: usize,
    ) -> Result<PoolMetrics> {
        let t_id = template_id.unwrap_or("default");
        self.register_template(t_id);

        if let Ok(mut caps) = self.dynamic_capacities.write() {
            caps.insert(t_id.to_string(), target_capacity);
        }

        let current_ready = self.store.list_available(t_id)?;
        if current_ready.len() < target_capacity {
            let needed = target_capacity - current_ready.len();
            for _ in 0..needed {
                let entry_id = uuid::Uuid::new_v4().to_string();
                let record = self
                    .creator
                    .create_warm_snapshot(&entry_id, t_id, &self.storage_dir)
                    .await?;
                self.store.insert(&record)?;
            }
        } else if current_ready.len() > target_capacity {
            let excess = current_ready.len() - target_capacity;
            for entry in current_ready.into_iter().take(excess) {
                let _ = self.store.update_status(&entry.id, PoolEntryStatus::Claimed);
                let _ = tokio::fs::remove_dir_all(self.storage_dir.join(&entry.id)).await;
            }
        }

        Ok(self.metrics())
    }

    #[tracing::instrument(skip(self))]
    pub async fn replenish_all(&self) -> Result<()> {
        let templates = self.list_registered_templates();
        for template_id in templates {
            self.replenish(&template_id).await?;
        }
        Ok(())
    }

    pub fn start_replenishment_loop(
        self: &Arc<Self>,
        interval: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let pool = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let _ = pool.replenish_all().await;
            }
        })
    }

    pub fn hit_rate(&self) -> f64 {
        let hits = self.hits.load(Ordering::SeqCst);
        let misses = self.misses.load(Ordering::SeqCst);
        let total = hits + misses;
        if total > 0 {
            hits as f64 / total as f64
        } else {
            0.0
        }
    }

    pub fn metrics(&self) -> PoolMetrics {
        let hits = self.hits.load(Ordering::SeqCst);
        let misses = self.misses.load(Ordering::SeqCst);
        let total_requests = hits + misses;
        let hit_rate = if total_requests > 0 {
            hits as f64 / total_requests as f64
        } else {
            0.0
        };

        let all_entries = self.store.list().unwrap_or_default();
        let ready_count = all_entries
            .iter()
            .filter(|e| e.status == PoolEntryStatus::Ready)
            .count();
        let claimed_count = all_entries
            .iter()
            .filter(|e| e.status == PoolEntryStatus::Claimed)
            .count();

        PoolMetrics {
            hits,
            misses,
            total_requests,
            hit_rate,
            ready_count,
            claimed_count,
        }
    }

    pub fn store(&self) -> &Arc<PoolStore> {
        &self.store
    }
}
