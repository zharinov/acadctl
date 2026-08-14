use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use acadctl_rpc::Document;
use tokio::sync::oneshot;

use crate::documents::{DocumentRegistry, DocumentTarget, NativeDocumentKey};
use crate::execution::output::OutputSink;
use crate::execution::value_bridge::NativeValueWriter;
use crate::execution::{Execution, NativeExecutionStep, Outcome as ExecutionOutcome, StepResult};
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
    quarantined: bool,
}

struct Job {
    request_id: u64,
    operation: Operation,
    native_target: Option<NativeDocumentKey>,
    completion: oneshot::Sender<Result<OperationOutcome, Error>>,
}

enum Operation {
    Open {
        path: String,
    },
    Save {
        id: String,
    },
    Close {
        id: String,
        discard: bool,
    },
    #[allow(
        dead_code,
        reason = "request admission stays private until the native proof gates pass"
    )]
    Execute {
        id: String,
        execution: Box<Execution>,
    },
}

enum OperationOutcome {
    Document(Document),
    Closed,
    #[allow(
        dead_code,
        reason = "request admission stays private until the native proof gates pass"
    )]
    Execution(ExecutionOutcome),
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

#[allow(
    dead_code,
    reason = "execution cancellation is private until the Execute RPC is exposed"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelResult {
    Accepted,
    TooLate,
    NotFound,
    Unavailable,
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
    NotQuiescent,
    UndoDisabled,
    ContextFailed(NativeFailure),
    ContextCleanupFailed(NativeFailure),
    ExecutionLeaseFailed(NativeFailure),
    ExecutionBridgeFailed(NativeFailure),
    ExecutionNotFinished,
    NativeStateUnknown,
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
            Self::NotQuiescent => formatter.write_str("The document is busy"),
            Self::UndoDisabled => {
                formatter.write_str("Undo recording is disabled for the document")
            }
            Self::ContextFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not establish document context")
            }
            Self::ContextCleanupFailed(failure) => failure.fmt_with_context(
                formatter,
                "Could not release the AutoCAD document context safely",
            ),
            Self::ExecutionLeaseFailed(failure) => failure.fmt_with_context(
                formatter,
                "Could not close the AutoCAD execution lease safely",
            ),
            Self::ExecutionBridgeFailed(failure) => {
                failure.fmt_with_context(formatter, "The AutoLISP execution bridge failed")
            }
            Self::ExecutionNotFinished => {
                formatter.write_str("The native execution ended without a terminal outcome")
            }
            Self::NativeStateUnknown => formatter.write_str(
                "AutoCAD's mutation context is unknown; restart AutoCAD before issuing another mutation",
            ),
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
                | Self::ContextFailed(_)
                | Self::ContextCleanupFailed(_)
                | Self::ExecutionLeaseFailed(_)
                | Self::ExecutionBridgeFailed(_)
                | Self::ExecutionNotFinished
                | Self::NativeStateUnknown
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
            quarantined: false,
        }
    }

    fn idle(&self) -> bool {
        self.active.is_none() && self.pending.is_empty() && !self.wake_pending
    }

    fn request_wake(&mut self) -> bool {
        if self.stopping
            || self.quarantined
            || self.active.is_some()
            || self.pending.is_empty()
            || self.wake_pending
        {
            return false;
        }
        self.wake_pending = true;
        true
    }
}

pub async fn open(path: String) -> Result<Document, Error> {
    match dispatch(Operation::Open { path }).await? {
        OperationOutcome::Document(document) => Ok(document),
        OperationOutcome::Closed | OperationOutcome::Execution(_) => Err(Error::OpenNotPublished),
    }
}

pub async fn save(id: String) -> Result<Document, Error> {
    match dispatch(Operation::Save { id }).await? {
        OperationOutcome::Document(document) => Ok(document),
        OperationOutcome::Closed | OperationOutcome::Execution(_) => Err(Error::SaveNotPublished),
    }
}

pub async fn close(id: String, discard: bool) -> Result<(), Error> {
    match dispatch(Operation::Close { id, discard }).await? {
        OperationOutcome::Closed => Ok(()),
        OperationOutcome::Document(_) | OperationOutcome::Execution(_) => {
            Err(Error::CloseNotPublished)
        }
    }
}

#[allow(
    dead_code,
    reason = "request admission stays private until the native proof gates pass"
)]
pub async fn execute(id: String, execution: Execution) -> Result<ExecutionOutcome, Error> {
    match dispatch(Operation::Execute {
        id,
        execution: Box::new(execution),
    })
    .await?
    {
        OperationOutcome::Execution(outcome) => Ok(outcome),
        OperationOutcome::Document(_) | OperationOutcome::Closed => {
            Err(Error::ExecutionNotFinished)
        }
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
        scheduler.quarantined = false;
    }
}

pub fn stop() {
    let stopped = SCHEDULER.lock().ok().map(|mut scheduler| {
        scheduler.stopping = true;
        scheduler.wake_pending = false;
        let active_output = scheduler.active.as_mut().and_then(|job| {
            if let Operation::Execute { execution, .. } = &mut job.operation {
                let _ = execution.request_cancel();
                Some(execution.output_sink())
            } else {
                None
            }
        });
        let pending = std::mem::take(&mut scheduler.pending)
            .into_iter()
            .map(|job| (job.completion, job.operation.output_sink()))
            .collect::<Vec<_>>();
        (active_output, pending)
    });
    if let Some((active_output, pending)) = stopped {
        if let Some(output) = active_output {
            output.stop();
        }
        for (completion, output) in pending {
            if let Some(output) = output {
                output.stop();
            }
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
        if scheduler.quarantined {
            return Err(Error::NativeStateUnknown);
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
            native_target: None,
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
        if scheduler.stopping || scheduler.quarantined || scheduler.active.is_some() {
            TakeDecision::Empty
        } else if let Some(mut job) = scheduler.pending.pop_front() {
            match prepare(&job.operation, &scheduler.documents) {
                Prepared::Immediate(outcome) => TakeDecision::Complete(job.completion, outcome),
                Prepared::Native(mut action) => {
                    action.request_id = job.request_id;
                    job.native_target = matches!(&job.operation, Operation::Execute { .. })
                        .then_some(NativeDocumentKey {
                            document_token: action.document_token,
                            database_token: action.database_token,
                        });
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
    let quarantine = matches!(
        result.kind,
        NativeActionResultKind::ContextCleanupFailed | NativeActionResultKind::ExecutionLeaseFailed
    );
    let (completion, pending) = {
        let Ok(mut scheduler) = SCHEDULER.lock() else {
            return;
        };
        let Some(mut job) = scheduler.active.take() else {
            return;
        };
        if job.request_id != request_id {
            scheduler.active = Some(job);
            return;
        }

        let output = job.operation.output_sink();
        let outcome = complete_operation(result, &mut job.operation, &scheduler.documents);
        let pending = if quarantine {
            scheduler.quarantined = true;
            scheduler.wake_pending = false;
            std::mem::take(&mut scheduler.pending)
                .into_iter()
                .map(|job| (job.completion, job.operation.output_sink()))
                .collect()
        } else {
            Vec::new()
        };
        ((job.completion, outcome, output), pending)
    };
    if let Some(output) = completion.2 {
        output.finish();
    }
    let _ = completion.0.send(completion.1);
    for (completion, output) in pending {
        if let Some(output) = output {
            output.stop();
        }
        let _ = completion.send(Err(Error::NativeStateUnknown));
    }
}

#[allow(
    dead_code,
    reason = "execution cancellation is private until the Execute RPC is exposed"
)]
pub fn cancel_execution(request_id: u64) -> CancelResult {
    enum Action {
        Active(OutputSink),
        Pending(OutputSink, oneshot::Sender<Result<OperationOutcome, Error>>),
        Result(CancelResult),
    }

    let action = {
        let Ok(mut scheduler) = SCHEDULER.lock() else {
            return CancelResult::Unavailable;
        };
        if let Some(job) = scheduler.active.as_mut()
            && job.request_id == request_id
        {
            match &mut job.operation {
                Operation::Execute { execution, .. } => {
                    if execution.request_cancel() {
                        Action::Active(execution.output_sink())
                    } else {
                        Action::Result(CancelResult::TooLate)
                    }
                }
                Operation::Open { .. } | Operation::Save { .. } | Operation::Close { .. } => {
                    Action::Result(CancelResult::NotFound)
                }
            }
        } else if let Some(index) = scheduler.pending.iter().position(|job| {
            job.request_id == request_id && matches!(job.operation, Operation::Execute { .. })
        }) {
            let output = match &mut scheduler.pending[index].operation {
                Operation::Execute { execution, .. } => execution
                    .cancel_before_start()
                    .then(|| execution.output_sink()),
                Operation::Open { .. } | Operation::Save { .. } | Operation::Close { .. } => {
                    unreachable!("only queued executions are selected")
                }
            };
            if let Some(output) = output {
                let job = scheduler.pending.remove(index).expect("queued job exists");
                Action::Pending(output, job.completion)
            } else {
                Action::Result(CancelResult::TooLate)
            }
        } else {
            Action::Result(CancelResult::NotFound)
        }
    };

    match action {
        Action::Active(output) => {
            output.request_cancel();
            CancelResult::Accepted
        }
        Action::Pending(output, completion) => {
            output.request_cancel();
            let _ = completion.send(Ok(OperationOutcome::Execution(ExecutionOutcome::Cancelled)));
            CancelResult::Accepted
        }
        Action::Result(result) => result,
    }
}

pub fn take_execution_step(execution_id: u64) -> NativeExecutionStep {
    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return NativeExecutionStep::invalid();
    };
    let Some(job) = scheduler.active.as_mut() else {
        return NativeExecutionStep::invalid();
    };
    if job.request_id != execution_id {
        return NativeExecutionStep::invalid();
    }
    match &mut job.operation {
        Operation::Execute { execution, .. } => execution.take_step(),
        Operation::Open { .. } | Operation::Save { .. } | Operation::Close { .. } => {
            NativeExecutionStep::invalid()
        }
    }
}

pub fn begin_println(document_token: usize, database_token: usize) -> NativeValueWriter {
    let lease = {
        let Ok(scheduler) = SCHEDULER.lock() else {
            return NativeValueWriter::inactive();
        };
        let Some(job) = scheduler.active.as_ref() else {
            return NativeValueWriter::inactive();
        };
        if job.native_target
            != Some(NativeDocumentKey {
                document_token,
                database_token,
            })
        {
            return NativeValueWriter::inactive();
        }
        match &job.operation {
            Operation::Execute { execution, .. } => execution.acquire_form_output(),
            Operation::Open { .. } | Operation::Save { .. } | Operation::Close { .. } => None,
        }
    };

    lease.map_or_else(NativeValueWriter::inactive, NativeValueWriter::println)
}

pub fn complete_execution_step(execution_id: u64, result: StepResult) -> bool {
    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return false;
    };
    let Some(job) = scheduler.active.as_mut() else {
        return false;
    };
    if job.request_id != execution_id {
        return false;
    }
    match &mut job.operation {
        Operation::Execute { execution, .. } => execution.complete_step(result),
        Operation::Open { .. } | Operation::Save { .. } | Operation::Close { .. } => false,
    }
}

pub fn abandon_execution(execution_id: u64, result: StepResult) -> bool {
    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return false;
    };
    let Some(job) = scheduler.active.as_mut() else {
        return false;
    };
    if job.request_id != execution_id {
        return false;
    }
    match &mut job.operation {
        Operation::Execute { execution, .. } => execution.abandon(result),
        Operation::Open { .. } | Operation::Save { .. } | Operation::Close { .. } => false,
    }
}

pub fn native_actions_need_wake() -> bool {
    SCHEDULER
        .lock()
        .is_ok_and(|mut scheduler| scheduler.request_wake())
}

pub fn wake_failed(status: i32) {
    let completions = SCHEDULER.lock().ok().map(|mut scheduler| {
        scheduler.wake_pending = false;
        std::mem::take(&mut scheduler.pending)
            .into_iter()
            .map(|job| (job.completion, job.operation.output_sink()))
            .collect::<Vec<_>>()
    });
    if let Some(completions) = completions {
        for (completion, output) in completions {
            if let Some(output) = output {
                output.stop();
            }
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
        Operation::Execute { id, execution } => match documents.find_by_id(id) {
            Some(_) if execution.outcome().is_some() => {
                Prepared::Immediate(Ok(OperationOutcome::Execution(
                    execution
                        .outcome()
                        .expect("terminal execution has an outcome")
                        .clone(),
                )))
            }
            Some(target) => Prepared::Native(native_action(
                NativeActionKind::RunExecution,
                Some(target.native_key),
                String::new(),
                false,
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
    operation: &mut Operation,
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
        Operation::Execute { execution, .. } => execution
            .take_outcome()
            .map(OperationOutcome::Execution)
            .ok_or(Error::ExecutionNotFinished),
    }
}

fn complete_operation(
    mut result: NativeActionResult,
    operation: &mut Operation,
    documents: &DocumentRegistry,
) -> Result<OperationOutcome, Error> {
    if matches!(
        result.kind,
        NativeActionResultKind::ContextCleanupFailed | NativeActionResultKind::ExecutionLeaseFailed
    ) && let Operation::Execute { execution, .. } = operation
        && execution.outcome().is_some()
    {
        let recorded = execution.record_terminal_failure(StepResult {
            kind: crate::execution::StepResultKind::NativeError,
            native_status: result.native_status,
            lisp_errno: 0,
            detail: std::mem::take(&mut result.native_detail),
        });
        debug_assert!(recorded);
        return finalize(operation, documents);
    }

    interpret(result, operation)?;
    finalize(operation, documents)
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
        NativeActionResultKind::NotQuiescent => Err(Error::NotQuiescent),
        NativeActionResultKind::UndoDisabled => Err(Error::UndoDisabled),
        NativeActionResultKind::ContextFailed => Err(Error::ContextFailed(failure)),
        NativeActionResultKind::ContextCleanupFailed => Err(Error::ContextCleanupFailed(failure)),
        NativeActionResultKind::ExecutionLeaseFailed => Err(Error::ExecutionLeaseFailed(failure)),
        NativeActionResultKind::ExecutionBridgeFailed => Err(Error::ExecutionBridgeFailed(failure)),
        kind => Err(Error::UnknownResult(kind.repr)),
    }
}

impl Operation {
    fn output_sink(&self) -> Option<OutputSink> {
        match self {
            Self::Execute { execution, .. } => Some(execution.output_sink()),
            Self::Open { .. } | Self::Save { .. } | Self::Close { .. } => None,
        }
    }

    fn document_id(&self) -> &str {
        match self {
            Self::Open { .. } => "",
            Self::Save { id } | Self::Close { id, .. } | Self::Execute { id, .. } => id,
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
    use crate::execution::ExecutionMode;
    use crate::execution::value_bridge::{ValueEvent, WriteResult};

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

    #[tokio::test]
    async fn drives_a_batch_through_one_native_execution_lease() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let execution = Execution::new(
            ExecutionMode::Exec,
            "batch.lsp".into(),
            "first\nsecond".into(),
        )
        .unwrap();
        let execution = execution.0;
        let pending = tokio::spawn(execute(id.clone(), execution));
        tokio::task::yield_now().await;

        let action = take();
        assert_eq!(action.kind, NativeActionKind::RunExecution);
        assert_eq!(action.document_token, 1);
        assert_eq!(action.database_token, 101);

        let begin = take_execution_step(action.request_id);
        assert_eq!(begin.kind(), crate::execution::StepKind::Begin);
        assert!(complete_execution_step(action.request_id, step_success()));

        let first = take_execution_step(action.request_id);
        assert_eq!(first.source(), "first");
        assert!(complete_execution_step(action.request_id, step_success()));
        let second = take_execution_step(action.request_id);
        assert_eq!(second.source(), "second");
        assert!(complete_execution_step(action.request_id, step_success()));

        let commit = take_execution_step(action.request_id);
        assert_eq!(commit.kind(), crate::execution::StepKind::Commit);
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Done
        );

        complete(action.request_id, result(NativeActionResultKind::Success));
        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Success);
        stop();
    }

    #[tokio::test]
    async fn routes_println_only_to_the_exact_active_form() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, mut output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let pending = tokio::spawn(execute(id, execution));
        tokio::task::yield_now().await;

        let action = take();
        assert!(!begin_println(1, 101).active());
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Begin
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Form
        );
        assert!(!begin_println(2, 101).active());
        assert!(!begin_println(1, 202).active());

        let mut writer = begin_println(1, 101);
        assert!(writer.active());
        assert_eq!(writer.write(ValueEvent::BeginString), WriteResult::Continue);
        assert_eq!(
            writer.write(ValueEvent::StringChunk("created: ")),
            WriteResult::Continue
        );
        assert_eq!(writer.write(ValueEvent::EndString), WriteResult::Continue);
        assert_eq!(writer.write(ValueEvent::Integer(3)), WriteResult::Continue);
        assert_eq!(writer.finish(), WriteResult::Continue);

        assert!(complete_execution_step(action.request_id, step_success()));
        assert!(!begin_println(1, 101).active());
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Commit
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Done
        );
        complete(action.request_id, result(NativeActionResultKind::Success));

        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Success);
        let mut rendered = String::new();
        while let Some(chunk) = output.next_chunk().await {
            rendered.push_str(&chunk);
        }
        assert_eq!(rendered, "created: 3\n");
        stop();
    }

    #[tokio::test]
    async fn rolls_back_a_malformed_active_value_stream() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let pending = tokio::spawn(execute(id, execution));
        tokio::task::yield_now().await;

        let action = take();
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Begin
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Form
        );

        let mut writer = begin_println(1, 101);
        assert_eq!(
            writer.write(ValueEvent::EndList),
            WriteResult::InvalidSequence
        );
        assert_eq!(writer.finish(), WriteResult::InvalidSequence);
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Rollback
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Done
        );
        complete(action.request_id, result(NativeActionResultKind::Success));

        assert_eq!(
            pending.await.unwrap().unwrap(),
            ExecutionOutcome::Failure(crate::execution::Failure {
                message: "the AutoLISP output bridge emitted an invalid value sequence".into(),
                form_index: Some(1),
                location: Some(crate::execution::SourceLocation {
                    source_name: "batch.lsp".into(),
                    line: 1,
                    column: 1,
                }),
                drawing_outcome: crate::execution::DrawingOutcome::RolledBack,
            })
        );
        stop();
    }

    #[tokio::test]
    async fn unfinished_writer_fails_its_own_form_checkpoint() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let pending = tokio::spawn(execute(id, execution));
        tokio::task::yield_now().await;

        let action = take();
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Begin
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Form
        );

        let mut writer = begin_println(1, 101);
        assert!(writer.active());
        assert!(complete_execution_step(action.request_id, step_success()));
        assert!(!writer.active());
        assert_eq!(writer.write(ValueEvent::Integer(9)), WriteResult::Inactive);
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Rollback
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Done
        );
        complete(action.request_id, result(NativeActionResultKind::Success));

        let Some(ExecutionOutcome::Failure(failure)) = pending.await.unwrap().ok() else {
            panic!("expected the unfinished writer to fail execution");
        };
        assert_eq!(
            failure.message,
            "the AutoLISP output bridge abandoned an unfinished value"
        );
        assert_eq!(failure.form_index, Some(1));
        assert_eq!(
            failure.drawing_outcome,
            crate::execution::DrawingOutcome::RolledBack
        );
        stop();
    }

    #[tokio::test]
    async fn queued_cancellation_removes_only_that_execution() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, true)]);
        let id = list().unwrap()[0].id.clone();

        let active = tokio::spawn(save(id.clone()));
        tokio::task::yield_now().await;
        let save_action = take();
        assert_eq!(save_action.kind, NativeActionKind::Save);

        let (execution, mut output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let pending = tokio::spawn(execute(id.clone(), execution));
        tokio::task::yield_now().await;
        let execution_id = SCHEDULER
            .lock()
            .unwrap()
            .pending
            .front()
            .expect("execution is queued behind save")
            .request_id;

        assert_eq!(cancel_execution(execution_id), CancelResult::Accepted);
        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Cancelled);
        assert_eq!(output.next_chunk().await, None);

        replace_documents(vec![document(1, 101, false)]);
        complete(
            save_action.request_id,
            result(NativeActionResultKind::Success),
        );
        assert!(active.await.unwrap().is_ok());
        assert!(!native_actions_need_wake());
        stop();
    }

    #[tokio::test]
    async fn dropping_a_queued_execution_waiter_keeps_the_job_and_output_alive() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, true)]);
        let id = list().unwrap()[0].id.clone();

        let active = tokio::spawn(save(id.clone()));
        tokio::task::yield_now().await;
        let save_action = take();
        assert_eq!(save_action.kind, NativeActionKind::Save);

        let (execution, mut output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let queued = tokio::spawn(execute(id, execution));
        tokio::task::yield_now().await;
        let execution_id = SCHEDULER
            .lock()
            .unwrap()
            .pending
            .front()
            .expect("execution is queued behind save")
            .request_id;

        queued.abort();
        assert!(queued.await.unwrap_err().is_cancelled());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), output.next_chunk())
                .await
                .is_err()
        );
        assert_eq!(cancel_execution(execution_id), CancelResult::Accepted);
        assert_eq!(output.next_chunk().await, None);

        replace_documents(vec![document(1, 101, false)]);
        complete(
            save_action.request_id,
            result(NativeActionResultKind::Success),
        );
        assert!(active.await.unwrap().is_ok());
        assert!(!native_actions_need_wake());
        stop();
    }

    #[tokio::test]
    async fn wake_failure_stops_a_pending_execution_output_stream() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, mut output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let pending = tokio::spawn(execute(id, execution));
        tokio::task::yield_now().await;

        wake_failed(42);

        assert_eq!(pending.await.unwrap(), Err(Error::ScheduleFailed(42)));
        assert_eq!(output.next_chunk().await, None);
        assert_eq!(take().kind, NativeActionKind::None);
        stop();
    }

    #[tokio::test]
    async fn active_cancellation_rolls_back_after_the_current_form() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, mut output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let pending = tokio::spawn(execute(id, execution));
        tokio::task::yield_now().await;

        let action = take();
        assert_eq!(action.kind, NativeActionKind::RunExecution);
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Begin
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Form
        );

        assert_eq!(cancel_execution(action.request_id), CancelResult::Accepted);
        assert_eq!(cancel_execution(action.request_id), CancelResult::Accepted);
        assert_eq!(output.next_chunk().await, None);
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Rollback
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Done
        );

        complete(action.request_id, result(NativeActionResultKind::Success));
        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Cancelled);
        stop();
    }

    #[tokio::test]
    async fn active_cancellation_before_the_first_form_uses_abort_not_rollback() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, mut output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let pending = tokio::spawn(execute(id, execution));
        tokio::task::yield_now().await;

        let action = take();
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Begin
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(cancel_execution(action.request_id), CancelResult::Accepted);
        assert_eq!(output.next_chunk().await, None);
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Abort
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Done
        );

        complete(action.request_id, result(NativeActionResultKind::Success));
        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Cancelled);
        stop();
    }

    #[tokio::test]
    async fn cancellation_after_commit_handoff_does_not_cancel_output() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, mut output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let pending = tokio::spawn(execute(id, execution));
        tokio::task::yield_now().await;

        let action = take();
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Begin
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Form
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Commit
        );

        assert_eq!(cancel_execution(action.request_id), CancelResult::TooLate);
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Done
        );
        complete(action.request_id, result(NativeActionResultKind::Success));

        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Success);
        assert_eq!(output.next_chunk().await, None);
        stop();
    }

    #[tokio::test]
    async fn shutdown_wakes_output_and_cancels_an_active_execution_safely() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, mut output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let pending = tokio::spawn(execute(id, execution));
        tokio::task::yield_now().await;

        let action = take();
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Begin
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Form
        );

        stop();
        assert_eq!(output.next_chunk().await, None);
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Rollback
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Done
        );
        complete(action.request_id, result(NativeActionResultKind::Success));
        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Cancelled);
    }

    #[tokio::test]
    async fn dropping_an_unpolled_execution_future_closes_output_without_admission() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, mut output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();

        let unpolled = execute(id, execution);
        drop(unpolled);

        assert_eq!(output.next_chunk().await, None);
        assert!(SCHEDULER.lock().unwrap().idle());
        stop();
    }

    #[tokio::test]
    async fn dropped_execution_waiter_does_not_release_its_native_lease() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, mut output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let executing = tokio::spawn(execute(id.clone(), execution));
        tokio::task::yield_now().await;

        let action = take();
        assert_eq!(action.kind, NativeActionKind::RunExecution);
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Begin
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        let form = take_execution_step(action.request_id);
        assert_eq!(form.source(), "form");

        executing.abort();
        assert!(executing.await.unwrap_err().is_cancelled());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), output.next_chunk())
                .await
                .is_err()
        );
        let later = tokio::spawn(close(id, true));
        tokio::task::yield_now().await;
        assert_eq!(take().kind, NativeActionKind::None);

        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Commit
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Done
        );
        complete(action.request_id, result(NativeActionResultKind::Success));
        assert_eq!(output.next_chunk().await, None);

        assert!(native_actions_need_wake());
        let close_action = take();
        assert_eq!(close_action.kind, NativeActionKind::Close);
        replace_documents(Vec::new());
        complete(
            close_action.request_id,
            result(NativeActionResultKind::Success),
        );
        assert!(later.await.unwrap().is_ok());
        stop();
    }

    #[tokio::test]
    async fn context_cleanup_failure_amends_a_terminal_execution_outcome() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let execution = Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "ok".into())
            .unwrap()
            .0;
        let pending = tokio::spawn(execute(id.clone(), execution));
        tokio::task::yield_now().await;

        let action = take();
        assert_eq!(action.kind, NativeActionKind::RunExecution);
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Begin
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Form
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Commit
        );
        assert!(complete_execution_step(action.request_id, step_success()));
        assert_eq!(
            take_execution_step(action.request_id).kind(),
            crate::execution::StepKind::Done
        );

        let blocked = tokio::spawn(save(id.clone()));
        tokio::task::yield_now().await;

        complete(
            action.request_id,
            NativeActionResult {
                kind: NativeActionResultKind::ContextCleanupFailed,
                native_status: 42,
                native_detail: "unlock failed".into(),
            },
        );

        assert_eq!(
            pending.await.unwrap().unwrap(),
            ExecutionOutcome::Failure(crate::execution::Failure {
                message: "unlock failed".into(),
                form_index: None,
                location: None,
                drawing_outcome: crate::execution::DrawingOutcome::Unknown,
            })
        );
        assert_eq!(blocked.await.unwrap(), Err(Error::NativeStateUnknown));
        assert_eq!(save(id).await, Err(Error::NativeStateUnknown));
        assert_eq!(take().kind, NativeActionKind::None);
        stop();
    }

    fn result(kind: NativeActionResultKind) -> NativeActionResult {
        NativeActionResult {
            kind,
            native_status: 0,
            native_detail: String::new(),
        }
    }

    fn step_success() -> StepResult {
        StepResult {
            kind: crate::execution::StepResultKind::Success,
            native_status: 0,
            lisp_errno: 0,
            detail: String::new(),
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
