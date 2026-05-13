//! process-local concurrency boundary for onboarding/settings state.
//!
//! storage.rs owns crash-safe file replacement. This store owns the
//! read-modify-write critical section so commands and the scheduler cannot
//! overwrite each other's fields with stale copies of state.json.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::storage;
use super::types::{OnboardingError, OnboardingState};

pub struct OnboardingStore {
    data_dir: PathBuf,
    lock: Mutex<()>,
}

impl OnboardingStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            lock: Mutex::new(()),
        }
    }

    #[allow(dead_code)]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn load(&self) -> OnboardingState {
        let _guard = self.lock.lock().expect("onboarding store poisoned");
        storage::load_or_default(&self.data_dir)
    }

    pub fn update<F>(&self, update: F) -> Result<OnboardingState, OnboardingError>
    where
        F: FnOnce(&mut OnboardingState),
    {
        self.access(|state| {
            update(state);
            (state.clone(), true)
        })
    }

    pub fn access<F, R>(&self, access: F) -> Result<R, OnboardingError>
    where
        F: FnOnce(&mut OnboardingState) -> (R, bool),
    {
        let _guard = self.lock.lock().expect("onboarding store poisoned");
        let mut state = storage::load_or_default(&self.data_dir);
        let (result, should_save) = access(&mut state);
        if should_save {
            storage::save(&self.data_dir, &state)?;
        }
        Ok(result)
    }

    pub fn reset(&self) -> Result<(), OnboardingError> {
        let _guard = self.lock.lock().expect("onboarding store poisoned");
        storage::reset(&self.data_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboarding::types::{OnboardingStep, PermissionKind, PermissionStatus};
    use std::sync::Arc;

    #[test]
    fn concurrent_field_updates_do_not_lose_each_other() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(OnboardingStore::new(tmp.path()));

        let a = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                store
                    .update(|s| {
                        s.last_step = OnboardingStep::Prefs;
                    })
                    .unwrap();
            })
        };
        let b = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                store
                    .update(|s| {
                        s.record_permission(
                            PermissionKind::LinuxHomeAcknowledged,
                            PermissionStatus::Granted,
                            42,
                        );
                    })
                    .unwrap();
            })
        };

        a.join().unwrap();
        b.join().unwrap();

        let state = store.load();
        assert_eq!(state.last_step, OnboardingStep::Prefs);
        assert_eq!(state.permissions.len(), 1);
    }
}
