//! A loaded, symbolicated profile, ready to query.

use crate::error::ToolError;
use crate::profile::{Profile, load_from_path};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[allow(dead_code)]
pub struct ProfileSession {
    id: String,
    name: String,
    path: PathBuf,
    profile: Arc<Profile>,
    /// Fraction of frames that did not symbolicate. 0.0–100.0.
    unsymbolicated_pct: f32,
}

#[allow(dead_code)]
impl ProfileSession {
    pub async fn load(path: &Path, name: Option<&str>) -> Result<Self, ToolError> {
        let abs = path
            .canonicalize()
            .map_err(|_| ToolError::FileNotFound { path: path.to_path_buf() })?;

        let mut raw = load_from_path(&abs)?;
        // Best-effort symbolication: resolve unsymbolicated frames (e.g. from
        // macOS samply recordings) before constructing the read-only Profile.
        crate::profile::symbolicate::symbolicate(&mut raw).await.ok();
        let profile = Arc::new(Profile::from_raw(raw));

        // For v1 we treat the profile as already-symbolicated by samply itself
        // (samply runs symbolication during recording). `wholesym` integration
        // for re-symbolicating an unsymbolicated profile is deferred — see spec
        // §"Architecture: Failure surface vs. samply".
        let unsymbolicated_pct = compute_unsymbolicated_pct(&profile);

        let id = profile_id_from_path(&abs);
        let name = name
            .map(str::to_owned)
            .unwrap_or_else(|| {
                abs.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.trim_end_matches(".json").to_owned())
                    .unwrap_or_else(|| id.clone())
            });

        Ok(Self {
            id,
            name,
            path: abs,
            profile,
            unsymbolicated_pct,
        })
    }

    #[allow(dead_code)]
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    #[allow(dead_code)]
    pub fn shared_profile(&self) -> Arc<Profile> {
        Arc::clone(&self.profile)
    }

    #[allow(dead_code)]
    pub fn unsymbolicated_pct(&self) -> f32 {
        self.unsymbolicated_pct
    }
}

#[allow(dead_code)]
fn profile_id_from_path(path: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    let v = h.finish();
    format!("{:08x}", v as u32)
}

#[allow(dead_code)]
fn compute_unsymbolicated_pct(profile: &Profile) -> f32 {
    let mut total: u64 = 0;
    let mut unsymbolicated: u64 = 0;
    for thread in profile.threads() {
        let handle = thread.handle();
        let raw = thread.raw();
        for s in raw.samples.stack.iter().flatten() {
            for frame_idx in profile.walk_stack(handle, *s) {
                total += 1;
                let info = profile.frame_info(handle, frame_idx);
                let is_unsym = info
                    .as_ref()
                    .is_none_or(|i| i.function_name.is_empty() || i.function_name == "0x0");
                if is_unsym {
                    unsymbolicated += 1;
                }
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        100.0 * unsymbolicated as f32 / total as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loads_and_describes_synthetic_profile() {
        // Reuses the test helper module via a dev-dep path workaround:
        // since helpers/ is in tests/, this unit test inlines a minimal
        // builder. Fuller scenarios live in integration tests.
        let raw_json = include_str!("../tests/fixtures/minimal_profile.json");
        let tmp = tempfile::NamedTempFile::with_suffix(".json").unwrap();
        std::fs::write(tmp.path(), raw_json).unwrap();
        let session = ProfileSession::load(tmp.path(), Some("test")).await.unwrap();
        assert_eq!(session.name(), "test");
        assert!(session.profile().threads().count() >= 1);
    }
}
