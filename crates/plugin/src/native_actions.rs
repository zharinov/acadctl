use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use tokio::sync::oneshot;

use crate::documents::NativeDocumentKey;
use crate::ffi::{NativeAction, NativeActionKind, NativeActionResult, NativeActionResultKind};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
static ACTIONS: LazyLock<Mutex<ActionQueue>> = LazyLock::new(|| Mutex::new(ActionQueue::new()));

struct ActionQueue {
    pending: VecDeque<NativeAction>,
    completions: HashMap<u64, oneshot::Sender<Result<(), Error>>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    StateUnavailable,
    ScheduleFailed(i32),
    Stopped,
    PluginStopping,
    DocumentGone,
    DocumentChanged,
    Unnamed,
    ReadOnly,
    Dirty,
    OpenFailed(NativeFailure),
    LockFailed(NativeFailure),
    SaveFailed(NativeFailure),
    CloseFailed(NativeFailure),
    UnknownResult(u8),
}

#[derive(Debug, PartialEq, Eq)]
pub struct NativeFailure {
    status: i32,
    detail: String,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StateUnavailable => formatter.write_str("native action state is unavailable"),
            Self::ScheduleFailed(status) => {
                write!(
                    formatter,
                    "AutoCAD could not schedule the operation (status {status})"
                )
            }
            Self::Stopped => formatter.write_str("the native operation stopped before completion"),
            Self::PluginStopping => formatter.write_str("the acadctl plugin is stopping"),
            Self::DocumentGone => formatter.write_str("The document is no longer open"),
            Self::DocumentChanged => formatter
                .write_str("The document changed before AutoCAD could perform the operation"),
            Self::Unnamed => formatter.write_str("The document has no file name"),
            Self::ReadOnly => formatter.write_str("The document is read-only"),
            Self::Dirty => formatter.write_str("The document has unsaved changes"),
            Self::OpenFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not open the drawing")
            }
            Self::LockFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not lock the document")
            }
            Self::SaveFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not save the document")
            }
            Self::CloseFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not close the document")
            }
            Self::UnknownResult(kind) => {
                write!(
                    formatter,
                    "AutoCAD returned an unknown native result ({kind})"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

impl NativeFailure {
    fn fmt_with_context(&self, formatter: &mut fmt::Formatter<'_>, context: &str) -> fmt::Result {
        if self.detail.is_empty() {
            write!(formatter, "{context} (ObjectARX status {})", self.status)
        } else {
            write!(formatter, "{context}: {}", self.detail)
        }
    }
}

impl ActionQueue {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            completions: HashMap::new(),
        }
    }
}

pub async fn open(path: String) -> Result<(), Error> {
    dispatch(NativeAction {
        request_id: 0,
        kind: NativeActionKind::Open,
        document_token: 0,
        database_token: 0,
        path,
        discard: false,
    })
    .await
}

pub async fn save(target: NativeDocumentKey) -> Result<(), Error> {
    dispatch(NativeAction {
        request_id: 0,
        kind: NativeActionKind::Save,
        document_token: target.document_token,
        database_token: target.database_token,
        path: String::new(),
        discard: false,
    })
    .await
}

pub async fn close(target: NativeDocumentKey, discard: bool) -> Result<(), Error> {
    dispatch(NativeAction {
        request_id: 0,
        kind: NativeActionKind::Close,
        document_token: target.document_token,
        database_token: target.database_token,
        path: String::new(),
        discard,
    })
    .await
}

async fn dispatch(mut action: NativeAction) -> Result<(), Error> {
    let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    action.request_id = request_id;
    let (completion, completed) = oneshot::channel();
    {
        let mut actions = ACTIONS.lock().map_err(|_| Error::StateUnavailable)?;
        actions.completions.insert(request_id, completion);
        actions.pending.push_back(action);
    }

    let wake_status = wake_native_actions();
    if wake_status != 0 {
        cancel(request_id);
        return Err(Error::ScheduleFailed(wake_status));
    }

    completed.await.map_err(|_| Error::Stopped)?
}

pub fn take() -> NativeAction {
    ACTIONS
        .lock()
        .ok()
        .and_then(|mut actions| actions.pending.pop_front())
        .unwrap_or_else(empty_action)
}

pub fn complete(request_id: u64, result: NativeActionResult) {
    let completion = ACTIONS
        .lock()
        .ok()
        .and_then(|mut actions| actions.completions.remove(&request_id));
    if let Some(completion) = completion {
        let _ = completion.send(interpret(result));
    }
}

pub fn cancel_all() {
    let completions = ACTIONS.lock().ok().map(|mut actions| {
        actions.pending.clear();
        std::mem::take(&mut actions.completions)
    });
    if let Some(completions) = completions {
        for completion in completions.into_values() {
            let _ = completion.send(Err(Error::PluginStopping));
        }
    }
}

fn interpret(result: NativeActionResult) -> Result<(), Error> {
    let failure = NativeFailure {
        status: result.native_status,
        detail: result.native_detail,
    };

    match result.kind {
        NativeActionResultKind::Success => Ok(()),
        NativeActionResultKind::DocumentGone => Err(Error::DocumentGone),
        NativeActionResultKind::DocumentChanged => Err(Error::DocumentChanged),
        NativeActionResultKind::Unnamed => Err(Error::Unnamed),
        NativeActionResultKind::ReadOnly => Err(Error::ReadOnly),
        NativeActionResultKind::Dirty => Err(Error::Dirty),
        NativeActionResultKind::OpenFailed => Err(Error::OpenFailed(failure)),
        NativeActionResultKind::LockFailed => Err(Error::LockFailed(failure)),
        NativeActionResultKind::SaveFailed => Err(Error::SaveFailed(failure)),
        NativeActionResultKind::CloseFailed => Err(Error::CloseFailed(failure)),
        kind => Err(Error::UnknownResult(kind.repr)),
    }
}

fn cancel(request_id: u64) {
    if let Ok(mut actions) = ACTIONS.lock() {
        actions
            .pending
            .retain(|action| action.request_id != request_id);
        actions.completions.remove(&request_id);
    }
}

fn empty_action() -> NativeAction {
    NativeAction {
        request_id: 0,
        kind: NativeActionKind::None,
        document_token: 0,
        database_token: 0,
        path: String::new(),
        discard: false,
    }
}

#[cfg(test)]
fn result(kind: NativeActionResultKind) -> NativeActionResult {
    NativeActionResult {
        kind,
        native_status: 0,
        native_detail: String::new(),
    }
}

#[cfg(not(test))]
fn wake_native_actions() -> i32 {
    unsafe extern "C" {
        fn acadctl_wake_native_actions() -> i32;
    }

    unsafe { acadctl_wake_native_actions() }
}

#[cfg(test)]
fn wake_native_actions() -> i32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_native_guard_outcomes_as_types() {
        assert_eq!(
            interpret(result(NativeActionResultKind::DocumentGone)),
            Err(Error::DocumentGone)
        );
        assert_eq!(
            interpret(result(NativeActionResultKind::DocumentChanged)),
            Err(Error::DocumentChanged)
        );
        assert_eq!(
            interpret(result(NativeActionResultKind::Unnamed)),
            Err(Error::Unnamed)
        );
        assert_eq!(
            interpret(result(NativeActionResultKind::ReadOnly)),
            Err(Error::ReadOnly)
        );
        assert_eq!(
            interpret(result(NativeActionResultKind::Dirty)),
            Err(Error::Dirty)
        );
    }

    #[tokio::test]
    async fn sends_actions_to_the_native_bridge_and_returns_the_result() {
        cancel_all();
        let pending = tokio::spawn(open("/tmp/house.dwg".into()));
        tokio::task::yield_now().await;

        let action = take();
        assert_eq!(action.kind, NativeActionKind::Open);
        assert_eq!(action.path, "/tmp/house.dwg");
        complete(action.request_id, result(NativeActionResultKind::Success));

        assert!(pending.await.unwrap().is_ok());

        let pending = tokio::spawn(save(NativeDocumentKey {
            document_token: 42,
            database_token: 84,
        }));
        tokio::task::yield_now().await;

        let action = take();
        assert_eq!(action.kind, NativeActionKind::Save);
        assert_eq!(action.document_token, 42);
        assert_eq!(action.database_token, 84);
        complete(
            action.request_id,
            NativeActionResult {
                kind: NativeActionResultKind::SaveFailed,
                native_status: 42,
                native_detail: "save failed".into(),
            },
        );

        let error = pending.await.unwrap().unwrap_err();
        assert_eq!(
            error,
            Error::SaveFailed(NativeFailure {
                status: 42,
                detail: "save failed".into(),
            })
        );
        assert_eq!(
            error.to_string(),
            "Could not save the document: save failed"
        );
    }
}
