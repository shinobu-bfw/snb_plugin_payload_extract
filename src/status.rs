use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use snb_core::context;
use snb_core::event::Event;

use super::output::{emit_content, text_item};
use super::{CommandRequest, PLUGIN_NAME};

struct StatusState {
    to: Option<String>,
    receiver: Option<String>,
    platform_id: Option<String>,
    pending_text: Option<String>,
    pending_delete: bool,
}

static NEXT_STATUS_ID: AtomicU64 = AtomicU64::new(1);
static PENDING_STATUS: LazyLock<Mutex<HashMap<String, StatusState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static STATUS_DELETE_ON_SENT: LazyLock<Mutex<HashMap<String, StatusHandle>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A status message that updates in place as work progresses and is deleted on
/// completion.
#[derive(Clone)]
pub(super) struct StatusHandle {
    local_id: String,
    to: Option<String>,
    receiver: Option<String>,
}

impl StatusHandle {
    pub(super) fn emit(request: &CommandRequest, text: impl Into<String>) -> Self {
        let local_id = next_status_id();
        PENDING_STATUS.lock().unwrap().insert(
            local_id.clone(),
            StatusState {
                to: request.to.clone(),
                receiver: request.receiver.clone(),
                platform_id: None,
                pending_text: None,
                pending_delete: false,
            },
        );
        emit_content(
            request,
            Some(local_id.clone()),
            None,
            vec![text_item(text.into(), None)],
        );
        Self {
            local_id,
            to: request.to.clone(),
            receiver: request.receiver.clone(),
        }
    }

    pub(super) fn update(&self, text: impl Into<String>) {
        let text = text.into();
        let platform_id = {
            let mut map = PENDING_STATUS.lock().unwrap();
            let Some(state) = map.get_mut(&self.local_id) else {
                return;
            };
            match &state.platform_id {
                Some(id) => id.clone(),
                None => {
                    state.pending_text = Some(text);
                    return;
                }
            }
        };
        self.emit_edit(&platform_id, text);
    }

    pub(super) fn finish(self) {
        let platform_id = {
            let mut map = PENDING_STATUS.lock().unwrap();
            let Some(state) = map.get_mut(&self.local_id) else {
                return;
            };
            match &state.platform_id {
                Some(id) => id.clone(),
                None => {
                    state.pending_text = None;
                    state.pending_delete = true;
                    return;
                }
            }
        };
        PENDING_STATUS.lock().unwrap().remove(&self.local_id);
        self.emit_delete(&platform_id);
    }

    fn emit_edit(&self, platform_id: &str, text: String) {
        let mut event = Event::message_edit(PLUGIN_NAME, platform_id, text, None);
        if let Some(message) = event.message.as_mut() {
            message.to = self.to.clone();
        }
        event.receiver = self.receiver.clone();
        context::bot().emit_event(event);
    }

    fn emit_delete(&self, platform_id: &str) {
        let mut event = Event::message_delete(PLUGIN_NAME, platform_id);
        if let Some(message) = event.message.as_mut() {
            message.to = self.to.clone();
        }
        event.receiver = self.receiver.clone();
        context::bot().emit_event(event);
    }
}

pub(super) fn resolve_status_message(local_id: &str, platform_id: &str) {
    let (handle, action) = {
        let mut map = PENDING_STATUS.lock().unwrap();
        let Some(state) = map.get_mut(local_id) else {
            return;
        };
        state.platform_id = Some(platform_id.to_string());
        let handle = StatusHandle {
            local_id: local_id.to_string(),
            to: state.to.clone(),
            receiver: state.receiver.clone(),
        };
        if state.pending_delete {
            (handle, StatusAction::Delete)
        } else if let Some(text) = state.pending_text.take() {
            (handle, StatusAction::Edit(text))
        } else {
            return;
        }
    };

    match action {
        StatusAction::Delete => {
            PENDING_STATUS.lock().unwrap().remove(local_id);
            handle.emit_delete(platform_id);
        }
        StatusAction::Edit(text) => handle.emit_edit(platform_id, text),
    }
}

enum StatusAction {
    Edit(String),
    Delete,
}

pub(super) fn delete_status_when_sent(sent_local_id: String, status: StatusHandle) {
    STATUS_DELETE_ON_SENT
        .lock()
        .unwrap()
        .insert(sent_local_id, status);
}

pub(super) fn finish_status_on_sent(sent_local_id: &str) {
    let handle = STATUS_DELETE_ON_SENT.lock().unwrap().remove(sent_local_id);
    if let Some(handle) = handle {
        handle.finish();
    }
}

fn next_status_id() -> String {
    let id = NEXT_STATUS_ID.fetch_add(1, Ordering::Relaxed);
    format!("{PLUGIN_NAME}:status:{id}")
}
