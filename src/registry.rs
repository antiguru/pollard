//! In-memory profile registry with LRU eviction.

#![allow(dead_code)]

use crate::error::ToolError;
use crate::session::ProfileSession;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct SessionRegistry {
    inner: Arc<RwLock<Inner>>,
    capacity: usize,
}

struct Inner {
    /// Insertion order; the head is least-recently-touched.
    order: VecDeque<String>,
    sessions: std::collections::HashMap<String, Arc<ProfileSession>>,
    /// id -> original path, retained even after eviction so the LLM can re-load.
    evicted_paths: std::collections::HashMap<String, PathBuf>,
}

impl SessionRegistry {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                order: VecDeque::new(),
                sessions: Default::default(),
                evicted_paths: Default::default(),
            })),
            capacity,
        }
    }

    pub async fn load(&self, path: &Path, name: Option<&str>) -> Result<String, ToolError> {
        let session = ProfileSession::load(path, name).await?;
        let id = session.id().to_owned();
        let mut inner = self.inner.write().await;

        // Idempotent: re-loading the same id replaces the existing session.
        if inner.sessions.contains_key(&id) {
            inner.order.retain(|x| x != &id);
        }

        // Evict until under capacity.
        while inner.sessions.len() >= self.capacity {
            if let Some(victim) = inner.order.pop_front() {
                if let Some(s) = inner.sessions.remove(&victim) {
                    inner.evicted_paths.insert(victim.clone(), s.path().to_path_buf());
                    eprintln!("pollard: evicted profile {} from cache", victim);
                }
            } else {
                break;
            }
        }

        inner.evicted_paths.remove(&id);
        inner.order.push_back(id.clone());
        inner.sessions.insert(id.clone(), Arc::new(session));
        Ok(id)
    }

    pub async fn get(&self, id: &str) -> Option<Arc<ProfileSession>> {
        let mut inner = self.inner.write().await;
        let s = inner.sessions.get(id).cloned()?;
        // Touch: move to end.
        inner.order.retain(|x| x != id);
        inner.order.push_back(id.to_owned());
        Some(s)
    }

    pub async fn unload(&self, id: &str) -> bool {
        let mut inner = self.inner.write().await;
        inner.order.retain(|x| x != id);
        inner.evicted_paths.remove(id);
        inner.sessions.remove(id).is_some()
    }

    pub async fn list(&self) -> Vec<Arc<ProfileSession>> {
        let inner = self.inner.read().await;
        inner.sessions.values().cloned().collect()
    }

    pub async fn evicted_path(&self, id: &str) -> Option<PathBuf> {
        let inner = self.inner.read().await;
        inner.evicted_paths.get(id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn registers_and_returns_profile() {
        let registry = SessionRegistry::new(2);
        let path: PathBuf = "tests/fixtures/minimal_profile.json".into();
        let id = registry.load(&path, None).await.unwrap();
        assert!(registry.get(&id).await.is_some());
    }

    #[tokio::test]
    async fn evicts_oldest_when_capacity_exceeded() {
        let registry = SessionRegistry::new(1);
        let id1 = registry
            .load(std::path::Path::new("tests/fixtures/minimal_profile.json"), Some("first"))
            .await
            .unwrap();
        let id2 = registry
            .load(std::path::Path::new("tests/fixtures/two_functions.json"), Some("second"))
            .await
            .unwrap();
        assert!(registry.get(&id1).await.is_none(), "first should have been evicted");
        assert!(registry.get(&id2).await.is_some());
    }
}
