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

    /// Build a derived view session over `base_id`, sharing the base's
    /// raw tables but applying `transforms`. Returns the new view id and
    /// any sessions evicted to make room.
    ///
    /// Errors with `ProfileNotFound` / `ProfileEvicted` if the base is
    /// not currently loaded (we don't auto-reload — that would block the
    /// caller on re-symbolication without warning).
    pub async fn create_view(
        &self,
        base_id: &str,
        name: Option<&str>,
        transforms: crate::profile::transforms::Transforms,
    ) -> Result<(String, Vec<EvictedSession>), ToolError> {
        let base = self.get_or_error(base_id).await?;
        let view_id = view_id_from(base_id, &transforms);
        let view_name = name
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}#view", base.name()));
        let session = ProfileSession::view(&base, view_id.clone(), view_name, transforms);
        let mut inner = self.inner.write().await;
        if inner.sessions.contains_key(&view_id) {
            inner.order.retain(|x| x != &view_id);
        }
        let mut evicted = Vec::new();
        while inner.sessions.len() >= self.capacity {
            let Some(victim_id) = inner.order.pop_front() else {
                break;
            };
            // Don't evict the base out from under a view we're about to
            // register — bases must outlive their derived views.
            if victim_id == base_id {
                inner.order.push_front(victim_id);
                break;
            }
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
        inner.evicted.remove(&view_id);
        inner.order.push_back(view_id.clone());
        inner.sessions.insert(view_id.clone(), Arc::new(session));
        Ok((view_id, evicted))
    }
}

fn view_id_from(base_id: &str, transforms: &crate::profile::transforms::Transforms) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    base_id.hash(&mut h);
    // Hash a compact, stable representation of transforms. We Debug-print
    // them: the Debug impl is derived and stable across releases on the
    // same struct shape; re-add an explicit hash if we ever need
    // cross-process compatibility (we don't today).
    format!("{transforms:?}").hash(&mut h);
    format!("{base_id}.v{:08x}", h.finish() as u32)
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

    #[tokio::test]
    async fn create_view_returns_deterministic_id() {
        let registry = SessionRegistry::new(2);
        let (base_id, _) = registry
            .load(
                std::path::Path::new("tests/fixtures/two_functions.json"),
                None,
            )
            .await
            .unwrap();
        let (view_id_1, _) = registry
            .create_view(&base_id, None, Default::default())
            .await
            .unwrap();
        let (view_id_2, _) = registry
            .create_view(&base_id, None, Default::default())
            .await
            .unwrap();
        assert_eq!(
            view_id_1, view_id_2,
            "same transforms should yield same view id"
        );
        assert_ne!(view_id_1, base_id);
    }

    #[tokio::test]
    async fn create_view_does_not_evict_its_base() {
        let registry = SessionRegistry::new(1);
        let (base_id, _) = registry
            .load(
                std::path::Path::new("tests/fixtures/two_functions.json"),
                None,
            )
            .await
            .unwrap();
        let (view_id, evicted) = registry
            .create_view(&base_id, None, Default::default())
            .await
            .unwrap();
        // capacity=1, but the base must remain so the view can read it.
        assert!(
            registry.get(&base_id).await.is_some(),
            "base must stay loaded under a view"
        );
        assert!(registry.get(&view_id).await.is_some());
        assert!(evicted.is_empty(), "no eviction expected; we keep the base");
    }
}
