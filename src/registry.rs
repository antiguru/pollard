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

/// A session that was evicted from the cache. Retained so callers can
/// re-load by path without losing track of what was previously available.
#[derive(Debug, Clone)]
pub struct EvictedSession {
    pub profile_id: String,
    pub name: String,
    pub path: PathBuf,
}

struct Inner {
    /// Insertion order; the head is least-recently-touched.
    order: VecDeque<String>,
    sessions: std::collections::HashMap<String, Arc<ProfileSession>>,
    /// id -> evicted-session info, retained even after eviction so the LLM
    /// can re-load by path.
    evicted: std::collections::HashMap<String, EvictedSession>,
}

impl SessionRegistry {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                order: VecDeque::new(),
                sessions: Default::default(),
                evicted: Default::default(),
            })),
            capacity,
        }
    }

    /// Loads a profile, returning the new id and any sessions that were
    /// evicted to make room.
    pub async fn load(
        &self,
        path: &Path,
        name: Option<&str>,
    ) -> Result<(String, Vec<EvictedSession>), ToolError> {
        let session = ProfileSession::load(path, name).await?;
        let id = session.id().to_owned();
        let mut inner = self.inner.write().await;

        // Idempotent: re-loading the same id replaces the existing session.
        if inner.sessions.contains_key(&id) {
            inner.order.retain(|x| x != &id);
        }

        // Evict until under capacity.
        let mut evicted = Vec::new();
        while inner.sessions.len() >= self.capacity {
            let Some(victim_id) = inner.order.pop_front() else {
                break;
            };
            let Some(s) = inner.sessions.remove(&victim_id) else {
                continue;
            };
            let entry = EvictedSession {
                profile_id: victim_id.clone(),
                name: s.name().to_owned(),
                path: s.path().to_path_buf(),
            };
            inner.evicted.insert(victim_id, entry.clone());
            evicted.push(entry);
        }

        inner.evicted.remove(&id);
        inner.order.push_back(id.clone());
        inner.sessions.insert(id.clone(), Arc::new(session));
        Ok((id, evicted))
    }

    pub async fn get(&self, id: &str) -> Option<Arc<ProfileSession>> {
        let mut inner = self.inner.write().await;
        let s = inner.sessions.get(id).cloned()?;
        // Touch: move to end.
        inner.order.retain(|x| x != id);
        inner.order.push_back(id.to_owned());
        Some(s)
    }

    /// Like `get`, but returns a structured `ProfileEvicted` error (carrying
    /// the original path) when the id was previously loaded and then evicted,
    /// vs. `ProfileNotFound` when the id has never been seen.
    pub async fn get_or_error(&self, id: &str) -> Result<Arc<ProfileSession>, ToolError> {
        if let Some(s) = self.get(id).await {
            return Ok(s);
        }
        let inner = self.inner.read().await;
        if let Some(e) = inner.evicted.get(id) {
            return Err(ToolError::ProfileEvicted {
                profile_id: id.to_owned(),
                original_path: e.path.clone(),
            });
        }
        Err(ToolError::ProfileNotFound {
            profile_id: id.to_owned(),
        })
    }

    pub async fn unload(&self, id: &str) -> bool {
        let mut inner = self.inner.write().await;
        inner.order.retain(|x| x != id);
        inner.evicted.remove(id);
        inner.sessions.remove(id).is_some()
    }

    pub async fn list(&self) -> Vec<Arc<ProfileSession>> {
        let inner = self.inner.read().await;
        inner.sessions.values().cloned().collect()
    }

    /// All profiles that were evicted but are still tracked by id+path so
    /// they can be re-loaded.
    pub async fn list_evicted(&self) -> Vec<EvictedSession> {
        let inner = self.inner.read().await;
        inner.evicted.values().cloned().collect()
    }

    pub async fn evicted_path(&self, id: &str) -> Option<PathBuf> {
        let inner = self.inner.read().await;
        inner.evicted.get(id).map(|e| e.path.clone())
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
        let (id, evicted) = registry.load(&path, None).await.unwrap();
        assert!(evicted.is_empty());
        assert!(registry.get(&id).await.is_some());
    }

    #[tokio::test]
    async fn evicts_oldest_when_capacity_exceeded() {
        let registry = SessionRegistry::new(1);
        let (id1, evicted_first) = registry
            .load(
                std::path::Path::new("tests/fixtures/minimal_profile.json"),
                Some("first"),
            )
            .await
            .unwrap();
        assert!(evicted_first.is_empty());
        let (id2, evicted_second) = registry
            .load(
                std::path::Path::new("tests/fixtures/two_functions.json"),
                Some("second"),
            )
            .await
            .unwrap();
        assert_eq!(evicted_second.len(), 1);
        assert_eq!(evicted_second[0].profile_id, id1);
        assert_eq!(evicted_second[0].name, "first");
        assert!(
            registry.get(&id1).await.is_none(),
            "first should have been evicted"
        );
        assert!(registry.get(&id2).await.is_some());

        // The evicted entry is queryable via list_evicted so the LLM can re-load.
        let still_tracked = registry.list_evicted().await;
        assert_eq!(still_tracked.len(), 1);
        assert_eq!(still_tracked[0].profile_id, id1);
    }
}
