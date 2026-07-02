use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use super::{CLEANUP_DELAY, PLUGIN_NAME};

static NEXT_CLEANUP_ID: AtomicU64 = AtomicU64::new(1);
static PENDING_CLEANUPS: LazyLock<Mutex<HashMap<String, PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn register_cleanup_path(path: PathBuf) -> String {
    let id = next_cleanup_id();
    PENDING_CLEANUPS.lock().unwrap().insert(id.clone(), path);

    let cleanup_id = id.clone();
    snb_core::task::spawn(async move {
        tokio::time::sleep(CLEANUP_DELAY).await;
        let Some(path) = PENDING_CLEANUPS.lock().unwrap().remove(&cleanup_id) else {
            return;
        };
        cleanup_path_now(path);
    });

    id
}

pub(super) fn cleanup_registered_path(cleanup_id: &str) {
    let Some(path) = PENDING_CLEANUPS.lock().unwrap().remove(cleanup_id) else {
        return;
    };
    cleanup_path_now(path);
}

pub(super) fn cleanup_path_now(path: PathBuf) {
    match fs::remove_dir_all(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            log::warn!("failed to clean up {}: {e}", path.display());
        }
    }
}

fn next_cleanup_id() -> String {
    let id = NEXT_CLEANUP_ID.fetch_add(1, Ordering::Relaxed);
    format!("{PLUGIN_NAME}:cleanup:{id}")
}
