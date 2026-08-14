use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use acadctl_rpc::Document;
use tokio::sync::oneshot;

use crate::documents::{DocumentRegistry, DocumentTarget, NativeDocumentKey};
use crate::ffi::{NativeAction, NativeActionKind, NativeActionResult, NativeActionResultKind};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
static SCHEDULER: LazyLock<Mutex<Scheduler>> = LazyLock::new(|| Mutex::new(Scheduler::new()));

#[cfg(test)]
pub(crate) static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Scheduler {
    documents: DocumentRegistry,
    pending: VecDeque<Job>,
    active: Option<Job>,
    wake_pending: bool,
    stopping: bool,
}

struct Job {
    request_id: u64,
    operation: Operation,
    completion: oneshot::Sender<Result<OperationOutcome, Error>>,
}

enum Operation {
    Open { path: String },
    Save { id: String },
    Close { id: String, discard: bool },
}

enum OperationOutcome {
    Document(Document),
    Closed,
}

enum Prepared {
    Immediate(Result<OperationOutcome, Error>),
    Native(NativeAction),
}

enum TakeDecision {
    Action(NativeAction),
    Complete(
        oneshot::Sender<Result<OperationOutcome, Error>>,
        Result<OperationOutcome, Error>,
    ),
    Empty,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    StateUnavailable,
    ScheduleFailed(i32),
    Stopped,
    PluginStopping,
    DocumentNotFound(String),
    DocumentGone,
    DocumentChanged,
    Unnamed(String),
    ReadOnly(String),
    Dirty(String),
    NotDwg,
    OpenFailed(NativeFailure),
    LockFailed(NativeFailure),
    SaveFailed(NativeFailure),
    CloseFailed(NativeFailure),
    OpenNotPublished,
    SaveNotPublished,
    CloseNotPublished,
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
            Self::DocumentNotFound(id) => write!(formatter, "Document '{id}' is not open."),
            Self::DocumentGone => formatter.write_str("The document is no longer open"),
            Self::DocumentChanged => formatter
                .write_str("The document changed before AutoCAD could perform the operation"),
            Self::Unnamed(id) => write!(
                formatter,
                "Document '{id}' has no file name. Save As is not supported yet."
            ),
            Self::ReadOnly(id) => write!(formatter, "Document '{id}' is read-only."),
            Self::Dirty(id) => write!(
                formatter,
                "Document '{id}' has unsaved changes. Run `acadctl save {id}` first or use `acadctl close {id} --discard`."
            ),
            Self::NotDwg => formatter.write_str("Only DWG drawings can be saved"),
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
            Self::OpenNotPublished => formatter
                .write_str("AutoCAD opened the drawing but did not publish its document state"),
            Self::SaveNotPublished => {
                formatter.write_str("AutoCAD completed the save but still reports unsaved changes")
            }
            Self::CloseNotPublished => {
                formatter.write_str("AutoCAD completed the close but the document is still open")
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

impl Error {
    pub const fn is_internal(&self) -> bool {
        matches!(
            self,
            Self::StateUnavailable
                | Self::Stopped
                | Self::OpenNotPublished
                | Self::SaveNotPublished
                | Self::CloseNotPublished
        )
    }
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            documents: DocumentRegistry::new(),
            pending: VecDeque::new(),
            active: None,
            wake_pending: false,
            stopping: false,
        }
    }

    fn idle(&self) -> bool {
        self.active.is_none() && self.pending.is_empty() && !self.wake_pending
    }

    fn request_wake(&mut self) -> bool {
        if self.stopping || self.active.is_some() || self.pending.is_empty() || self.wake_pending {
            return false;
        }
        self.wake_pending = true;
        true
    }
}

pub async fn open(path: String) -> Result<Document, Error> {
    match dispatch(Operation::Open { path }).await? {
        OperationOutcome::Document(document) => Ok(document),
        OperationOutcome::Closed => Err(Error::OpenNotPublished),
    }
}

pub async fn save(id: String) -> Result<Document, Error> {
    match dispatch(Operation::Save { id }).await? {
        OperationOutcome::Document(document) => Ok(document),
        OperationOutcome::Closed => Err(Error::SaveNotPublished),
    }
}

pub async fn close(id: String, discard: bool) -> Result<(), Error> {
    match dispatch(Operation::Close { id, discard }).await? {
        OperationOutcome::Closed => Ok(()),
        OperationOutcome::Document(_) => Err(Error::CloseNotPublished),
    }
}

pub fn list() -> Result<Vec<Document>, Error> {
    SCHEDULER
        .lock()
        .map_err(|_| Error::StateUnavailable)
        .map(|scheduler| scheduler.documents.list())
}

pub fn replace_documents(documents: Vec<crate::ffi::NativeDocumentState>) {
    if let Ok(mut scheduler) = SCHEDULER.lock() {
        scheduler.documents.replace(documents);
    }
}

pub fn start() {
    if let Ok(mut scheduler) = SCHEDULER.lock() {
        scheduler.stopping = false;
    }
}

pub fn stop() {
    let completions = SCHEDULER.lock().ok().map(|mut scheduler| {
        scheduler.stopping = true;
        scheduler.wake_pending = false;
        scheduler
            .pending
            .drain(..)
            .map(|job| job.completion)
            .collect::<Vec<_>>()
    });
    if let Some(completions) = completions {
        for completion in completions {
            let _ = completion.send(Err(Error::PluginStopping));
        }
    }
}

async fn dispatch(operation: Operation) -> Result<OperationOutcome, Error> {
    let (completed, should_wake) = {
        let mut scheduler = SCHEDULER.lock().map_err(|_| Error::StateUnavailable)?;
        if scheduler.stopping {
            return Err(Error::PluginStopping);
        }
        if scheduler.idle()
            && let Prepared::Immediate(outcome) = prepare(&operation, &scheduler.documents)
        {
            return outcome;
        }

        let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let (completion, completed) = oneshot::channel();
        scheduler.pending.push_back(Job {
            request_id,
            operation,
            completion,
        });
        let should_wake = scheduler.request_wake();
        (completed, should_wake)
    };

    if should_wake {
        schedule_native_actions();
    }
    completed.await.map_err(|_| Error::Stopped)?
}

pub fn take() -> NativeAction {
    let decision = {
        let Ok(mut scheduler) = SCHEDULER.lock() else {
            return empty_action();
        };
        scheduler.wake_pending = false;
        if scheduler.stopping || scheduler.active.is_some() {
            TakeDecision::Empty
        } else if let Some(job) = scheduler.pending.pop_front() {
            match prepare(&job.operation, &scheduler.documents) {
                Prepared::Immediate(outcome) => TakeDecision::Complete(job.completion, outcome),
                Prepared::Native(mut action) => {
                    action.request_id = job.request_id;
                    scheduler.active = Some(job);
                    TakeDecision::Action(action)
                }
            }
        } else {
            TakeDecision::Empty
        }
    };

    match decision {
        TakeDecision::Action(action) => action,
        TakeDecision::Complete(completion, outcome) => {
            let _ = completion.send(outcome);
            empty_action()
        }
        TakeDecision::Empty => empty_action(),
    }
}

pub fn complete(request_id: u64, result: NativeActionResult) {
    let completion = {
        let Ok(mut scheduler) = SCHEDULER.lock() else {
            return;
        };
        let Some(job) = scheduler.active.take() else {
            return;
        };
        if job.request_id != request_id {
            scheduler.active = Some(job);
            return;
        }

        let outcome = interpret(result, &job.operation)
            .and_then(|()| finalize(&job.operation, &scheduler.documents));
        (job.completion, outcome)
    };
    let _ = completion.0.send(completion.1);
}

pub fn native_actions_need_wake() -> bool {
    SCHEDULER
        .lock()
        .is_ok_and(|mut scheduler| scheduler.request_wake())
}

pub fn wake_failed(status: i32) {
    let completions = SCHEDULER.lock().ok().map(|mut scheduler| {
        scheduler.wake_pending = false;
        scheduler
            .pending
            .drain(..)
            .map(|job| job.completion)
            .collect::<Vec<_>>()
    });
    if let Some(completions) = completions {
        for completion in completions {
            let _ = completion.send(Err(Error::ScheduleFailed(status)));
        }
    }
}

fn prepare(operation: &Operation, documents: &DocumentRegistry) -> Prepared {
    match operation {
        Operation::Open { path } => documents.find_by_path(path).map_or_else(
            || {
                Prepared::Native(native_action(
                    NativeActionKind::Open,
                    None,
                    path.clone(),
                    false,
                ))
            },
            |target| Prepared::Immediate(Ok(OperationOutcome::Document(target.document))),
        ),
        Operation::Save { id } => match documents.find_by_id(id) {
            Some(target) => prepare_save(id, target),
            None => Prepared::Immediate(Err(Error::DocumentNotFound(id.clone()))),
        },
        Operation::Close { id, discard } => match documents.find_by_id(id) {
            Some(target) if target.document.modified && !discard => {
                Prepared::Immediate(Err(Error::Dirty(id.clone())))
            }
            Some(target) => Prepared::Native(native_action(
                NativeActionKind::Close,
                Some(target.native_key),
                String::new(),
                *discard,
            )),
            None => Prepared::Immediate(Err(Error::DocumentNotFound(id.clone()))),
        },
    }
}

fn prepare_save(id: &str, target: DocumentTarget) -> Prepared {
    if !target.named {
        return Prepared::Immediate(Err(Error::Unnamed(id.to_owned())));
    }
    if target.document.read_only {
        return Prepared::Immediate(Err(Error::ReadOnly(id.to_owned())));
    }
    if !is_dwg(std::path::Path::new(&target.document.path)) {
        return Prepared::Immediate(Err(Error::NotDwg));
    }
    if !target.document.modified {
        return Prepared::Immediate(Ok(OperationOutcome::Document(target.document)));
    }
    Prepared::Native(native_action(
        NativeActionKind::Save,
        Some(target.native_key),
        String::new(),
        false,
    ))
}

fn finalize(
    operation: &Operation,
    documents: &DocumentRegistry,
) -> Result<OperationOutcome, Error> {
    match operation {
        Operation::Open { path } => documents
            .find_by_path(path)
            .map(|target| OperationOutcome::Document(target.document))
            .ok_or(Error::OpenNotPublished),
        Operation::Save { id } => {
            let target = documents
                .find_by_id(id)
                .ok_or_else(|| Error::DocumentNotFound(id.clone()))?;
            if target.document.modified {
                Err(Error::SaveNotPublished)
            } else {
                Ok(OperationOutcome::Document(target.document))
            }
        }
        Operation::Close { id, .. } => {
            if documents.find_by_id(id).is_none() {
                Ok(OperationOutcome::Closed)
            } else {
                Err(Error::CloseNotPublished)
            }
        }
    }
}

fn native_action(
    kind: NativeActionKind,
    target: Option<NativeDocumentKey>,
    path: String,
    discard: bool,
) -> NativeAction {
    let target = target.unwrap_or(NativeDocumentKey {
        document_token: 0,
        database_token: 0,
    });
    NativeAction {
        request_id: 0,
        kind,
        document_token: target.document_token,
        database_token: target.database_token,
        path,
        discard,
    }
}

fn interpret(result: NativeActionResult, operation: &Operation) -> Result<(), Error> {
    let failure = NativeFailure {
        status: result.native_status,
        detail: result.native_detail,
    };
    match result.kind {
        NativeActionResultKind::Success => Ok(()),
        NativeActionResultKind::DocumentGone => Err(Error::DocumentGone),
        NativeActionResultKind::DocumentChanged => Err(Error::DocumentChanged),
        NativeActionResultKind::Unnamed => Err(Error::Unnamed(operation.document_id().to_owned())),
        NativeActionResultKind::ReadOnly => {
            Err(Error::ReadOnly(operation.document_id().to_owned()))
        }
        NativeActionResultKind::Dirty => Err(Error::Dirty(operation.document_id().to_owned())),
        NativeActionResultKind::OpenFailed => Err(Error::OpenFailed(failure)),
        NativeActionResultKind::LockFailed => Err(Error::LockFailed(failure)),
        NativeActionResultKind::SaveFailed => Err(Error::SaveFailed(failure)),
        NativeActionResultKind::CloseFailed => Err(Error::CloseFailed(failure)),
        kind => Err(Error::UnknownResult(kind.repr)),
    }
}

impl Operation {
    fn document_id(&self) -> &str {
        match self {
            Self::Open { .. } => "",
            Self::Save { id } | Self::Close { id, .. } => id,
        }
    }
}

fn empty_action() -> NativeAction {
    native_action(NativeActionKind::None, None, String::new(), false)
}

fn is_dwg(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dwg"))
}

fn schedule_native_actions() {
    let status = wake_native_actions();
    if status != 0 {
        wake_failed(status);
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
            interpret(
                result(NativeActionResultKind::DocumentGone),
                &Operation::Save { id: "doc".into() },
            ),
            Err(Error::DocumentGone)
        );
        assert_eq!(
            interpret(
                result(NativeActionResultKind::DocumentChanged),
                &Operation::Save { id: "doc".into() },
            ),
            Err(Error::DocumentChanged)
        );
        assert_eq!(
            interpret(
                result(NativeActionResultKind::ReadOnly),
                &Operation::Save { id: "doc".into() },
            ),
            Err(Error::ReadOnly("doc".into()))
        );
    }

    #[tokio::test]
    async fn dropped_waiter_does_not_cancel_or_release_an_operation() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, true)]);
        let id = list().unwrap()[0].id.clone();

        let first = tokio::spawn(save(id.clone()));
        tokio::task::yield_now().await;
        let save_action = take();
        assert_eq!(save_action.kind, NativeActionKind::Save);

        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());

        let second = tokio::spawn(close(id.clone(), true));
        tokio::task::yield_now().await;
        assert_eq!(take().kind, NativeActionKind::None);

        replace_documents(vec![document(1, 101, false)]);
        complete(
            save_action.request_id,
            result(NativeActionResultKind::Success),
        );
        assert!(native_actions_need_wake());

        let close_action = take();
        assert_eq!(close_action.kind, NativeActionKind::Close);
        replace_documents(Vec::new());
        complete(
            close_action.request_id,
            result(NativeActionResultKind::Success),
        );
        assert!(second.await.unwrap().is_ok());
        stop();
    }

    #[tokio::test]
    async fn wake_failure_completes_every_job_waiting_on_that_wake() {
        let _test = TEST_LOCK.lock().await;
        reset(Vec::new());

        let first = tokio::spawn(open("/tmp/first.dwg".into()));
        let second = tokio::spawn(open("/tmp/second.dwg".into()));
        let third = tokio::spawn(open("/tmp/third.dwg".into()));
        tokio::task::yield_now().await;

        wake_failed(42);

        for job in [first, second, third] {
            assert_eq!(job.await.unwrap(), Err(Error::ScheduleFailed(42)));
        }
        assert_eq!(take().kind, NativeActionKind::None);
        stop();
    }

    #[tokio::test]
    async fn shutdown_rejects_pending_work_but_preserves_the_active_operation() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, true)]);
        let id = list().unwrap()[0].id.clone();

        let active = tokio::spawn(save(id.clone()));
        tokio::task::yield_now().await;
        let save_action = take();
        assert_eq!(save_action.kind, NativeActionKind::Save);

        let pending = tokio::spawn(close(id, true));
        tokio::task::yield_now().await;
        stop();

        assert_eq!(pending.await.unwrap(), Err(Error::PluginStopping));
        assert_eq!(take().kind, NativeActionKind::None);

        replace_documents(vec![document(1, 101, false)]);
        complete(
            save_action.request_id,
            result(NativeActionResultKind::Success),
        );
        assert!(active.await.unwrap().is_ok());
    }

    fn result(kind: NativeActionResultKind) -> NativeActionResult {
        NativeActionResult {
            kind,
            native_status: 0,
            native_detail: String::new(),
        }
    }

    fn document(
        token: usize,
        database_token: usize,
        modified: bool,
    ) -> crate::ffi::NativeDocumentState {
        crate::ffi::NativeDocumentState {
            token,
            database_token,
            name: "/tmp/house.dwg".into(),
            named: true,
            modified,
            read_only: false,
        }
    }

    fn reset(documents: Vec<crate::ffi::NativeDocumentState>) {
        stop();
        replace_documents(documents);
        start();
    }
}
