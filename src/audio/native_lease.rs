//! A timed-out OS worker retains its endpoint until the actual stream is dropped.
//! This guard is owned by the blocking worker, never by its cancellable future.
use super::DeviceDirection;
use anyhow::{Result, ensure};
use std::{
    collections::HashSet,
    sync::{Mutex, OnceLock},
};

static OWNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

pub(super) struct DeviceLease(String);
impl DeviceLease {
    pub(super) fn acquire(direction: DeviceDirection, persistent_id: &str) -> Result<Self> {
        Self::acquire_class(direction, persistent_id, "original")
    }

    /// Shared-mode translated output must never reserve original routing's
    /// worker slot. Each class still owns one worker until its driver closes.
    pub(super) fn acquire_translated_output(persistent_id: &str) -> Result<Self> {
        Self::acquire_class(DeviceDirection::Output, persistent_id, "translation")
    }

    fn acquire_class(direction: DeviceDirection, persistent_id: &str, class: &str) -> Result<Self> {
        let key = format!("{class}:{direction:?}:{persistent_id}");
        let mut owned = OWNED
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        ensure!(
            owned.insert(key.clone()),
            "the previous native audio worker still owns this endpoint; waiting for the driver to close it"
        );
        Ok(Self(key))
    }
}
impl Drop for DeviceLease {
    fn drop(&mut self) {
        OWNED
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_and_translated_output_have_independent_exclusive_worker_slots() {
        let id = "test-independent-output-classes";
        let translation = DeviceLease::acquire_translated_output(id).unwrap();
        let original = DeviceLease::acquire(DeviceDirection::Output, id).unwrap();
        assert!(DeviceLease::acquire_translated_output(id).is_err());
        assert!(DeviceLease::acquire(DeviceDirection::Output, id).is_err());
        let capture = DeviceLease::acquire(DeviceDirection::Input, id).unwrap();
        drop(translation);
        let translation = DeviceLease::acquire_translated_output(id).unwrap();
        assert!(DeviceLease::acquire(DeviceDirection::Output, id).is_err());
        drop(original);
        assert!(DeviceLease::acquire(DeviceDirection::Output, id).is_ok());
        assert!(DeviceLease::acquire_translated_output(id).is_err());
        drop((translation, capture));
    }

    #[tokio::test]
    async fn aborting_async_waiter_does_not_release_a_still_running_os_worker() {
        let id = "test-stalled-driver";
        let lease = DeviceLease::acquire(DeviceDirection::Output, id).unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let (started, running) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _lease = lease;
            started.send(()).unwrap();
            wait.recv().unwrap();
        });
        running.await.unwrap();
        // Once spawn_blocking has started, aborting its handle cannot stop the
        // OS call. A subsequent route must not open a duplicate stream.
        worker.abort();
        assert!(DeviceLease::acquire(DeviceDirection::Output, id).is_err());
        let input = DeviceLease::acquire(DeviceDirection::Input, id).unwrap();
        release.send(()).unwrap();
        worker.await.unwrap();
        assert!(DeviceLease::acquire(DeviceDirection::Output, id).is_ok());
        drop(input);
    }
}
