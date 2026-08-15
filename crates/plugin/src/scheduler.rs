use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use acadctl_rpc::Document;
use tokio::sync::{Notify, oneshot};

use crate::documents::{DocumentRegistry, DocumentTarget, NativeDocumentKey};
use crate::execution::output::{OutputSink, OutputStream};
use crate::execution::value_bridge::NativeValueWriter;
use crate::execution::{
    Execution, ExecutionOutcome, ExecutionStepResult, NativeExecutionStep, bound_diagnostic,
};
use crate::ffi::{
    NativeAction, NativeActionKind, NativeActionResult, NativeActionResultKind,
    NativeExecutionFinalizationObservation,
};

#[cfg(test)]
type ExecutionFinalizationObservation = NativeExecutionFinalizationObservation;

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);
static EXECUTION_RESERVATIONS: AtomicUsize = AtomicUsize::new(0);
static SCHEDULER: LazyLock<Mutex<MutationScheduler>> =
    LazyLock::new(|| Mutex::new(MutationScheduler::new()));
static TIMERS_CHANGED: Notify = Notify::const_new();

pub(crate) type MutationJobId = u64;

pub const EXECUTION_START_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_MUTATION_JOBS: usize = 32;
pub const MAX_ADMITTED_EXECUTIONS: usize = 8;
pub const MAX_ADMITTED_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const BUSY_RETRY_INITIAL: Duration = Duration::from_millis(50);
const BUSY_RETRY_MAX: Duration = Duration::from_millis(500);

impl NativeExecutionFinalizationObservation {
    fn native_state_unproved(&self) -> bool {
        self.undo_group_may_be_open
            || self.bridge_symbols_may_be_retained
            || self.staged_form_may_be_retained
            || self.value_writer_active
            || self.terminal_cleanup_failed
    }

    fn only_symbol_cleanup_unproved(&self) -> bool {
        (self.bridge_symbols_may_be_retained || self.terminal_cleanup_failed)
            && !self.undo_group_may_be_open
            && !self.staged_form_may_be_retained
            && !self.value_writer_active
    }
}

#[cfg(test)]
pub(crate) static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct MutationScheduler {
    documents: DocumentRegistry,
    pending: VecDeque<MutationJob>,
    active: Option<MutationJob>,
    wake_pending: bool,
    stopping: bool,
    quarantined: bool,
}

struct MutationJob {
    job_id: MutationJobId,
    operation: Operation,
    native_target: Option<NativeDocumentKey>,
    start_deadline: Option<Instant>,
    waiting_for_readiness: bool,
    retry_at: Option<Instant>,
    retry_delay: Duration,
    _execution_reservation: Option<ExecutionReservation>,
    completion: oneshot::Sender<Result<OperationOutcome, Error>>,
}

#[derive(Clone)]
pub struct ExecutionReservation {
    _inner: Arc<ExecutionReservationInner>,
}

struct ExecutionReservationInner;

impl Drop for ExecutionReservationInner {
    fn drop(&mut self) {
        let previous = EXECUTION_RESERVATIONS.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
    }
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
    History {
        id: String,
        direction: HistoryDirection,
    },
    Execute {
        id: String,
        execution: Box<Execution>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistoryDirection {
    Undo,
    Redo,
}

enum OperationOutcome {
    Document(Document),
    Closed,
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
        Option<OutputSink>,
    ),
}

pub struct ExecutionAdmission {
    job_id: MutationJobId,
    output: OutputStream,
    completion: ExecutionCompletion,
}

pub struct ExecutionCompletion {
    receiver: oneshot::Receiver<Result<OperationOutcome, Error>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelResult {
    Accepted,
    TooLate,
    NotFound,
    Unavailable,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    SchedulerStateUnavailable,
    ScheduleFailed(i32),
    Stopped,
    PluginStopping,
    DocumentNotFound(String),
    DocumentGone,
    DocumentGenerationChanged,
    Unnamed(String),
    ReadOnly(String),
    Dirty(String),
    NotDwg,
    OpenFailed(NativeFailure),
    LockFailed(NativeFailure),
    SaveFailed(NativeFailure),
    CloseFailed(NativeFailure),
    HistoryFailed {
        direction: HistoryDirection,
        failure: NativeFailure,
    },
    OpenNotPublished,
    SaveNotPublished,
    CloseNotPublished,
    NotQuiescent,
    UndoDisabled,
    DocumentContextFailed(NativeFailure),
    DocumentContextRestoreFailed(NativeFailure),
    ExecutionBridgeFinalizationFailed(NativeFailure),
    ExecutionBridgeSymbolsClearFailed(NativeFailure),
    ExecutionBridgeFailed(NativeFailure),
    ExecutionNotFinished,
    MutationCapacity,
    ExecutionCapacity,
    NativeMutationStateUnknown,
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
            Self::SchedulerStateUnavailable => formatter.write_str("mutation scheduler state is unavailable"),
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
            Self::DocumentGenerationChanged => formatter
                .write_str("The document was replaced before AutoCAD could perform the operation"),
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
            Self::HistoryFailed { direction, failure } => failure.fmt_with_context(
                formatter,
                match direction {
                    HistoryDirection::Undo => "Could not undo the drawing's last history step",
                    HistoryDirection::Redo => "Could not redo the drawing's next history step",
                },
            ),
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
            Self::DocumentContextFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not establish document context")
            }
            Self::DocumentContextRestoreFailed(failure) => failure.fmt_with_context(
                formatter,
                "Could not release the AutoCAD document context safely",
            ),
            Self::ExecutionBridgeFinalizationFailed(failure) => failure.fmt_with_context(
                formatter,
                "Could not release native execution state safely",
            ),
            Self::ExecutionBridgeSymbolsClearFailed(failure) => failure.fmt_with_context(
                formatter,
                "Could not clear the reserved AutoLISP execution bridge symbols",
            ),
            Self::ExecutionBridgeFailed(failure) => {
                failure.fmt_with_context(formatter, "The AutoLISP execution bridge failed")
            }
            Self::ExecutionNotFinished => {
                formatter.write_str("The native execution ended without a terminal outcome")
            }
            Self::MutationCapacity => {
                formatter.write_str("AutoCAD already has the maximum number of pending operations")
            }
            Self::ExecutionCapacity => formatter.write_str(
                "AutoCAD already has the maximum number or total size of execution requests",
            ),
            Self::NativeMutationStateUnknown => formatter.write_str(
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
            Self::SchedulerStateUnavailable
                | Self::Stopped
                | Self::OpenNotPublished
                | Self::SaveNotPublished
                | Self::CloseNotPublished
                | Self::DocumentContextFailed(_)
                | Self::DocumentContextRestoreFailed(_)
                | Self::ExecutionBridgeFinalizationFailed(_)
                | Self::ExecutionBridgeSymbolsClearFailed(_)
                | Self::ExecutionBridgeFailed(_)
                | Self::ExecutionNotFinished
                | Self::NativeMutationStateUnknown
        )
    }
}

impl MutationScheduler {
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

    fn execution_usage(&self) -> (usize, usize) {
        self.pending
            .iter()
            .chain(self.active.iter())
            .filter_map(|job| match &job.operation {
                Operation::Execute { execution, .. } => Some(execution.source_bytes()),
                Operation::Open { .. }
                | Operation::Save { .. }
                | Operation::Close { .. }
                | Operation::History { .. } => None,
            })
            .fold((0, 0), |(count, bytes), source_bytes| {
                (count + 1, bytes + source_bytes)
            })
    }

    fn mutation_job_count(&self) -> usize {
        self.pending.len() + usize::from(self.active.is_some())
    }

    fn request_wake(&mut self) -> bool {
        if self.stopping
            || self.quarantined
            || self.active.is_some()
            || self.pending.is_empty()
            || self
                .pending
                .front()
                .is_some_and(|job| job.waiting_for_readiness)
            || self.wake_pending
        {
            return false;
        }

        self.wake_pending = true;
        true
    }
}

pub async fn open(path: String) -> Result<Document, Error> {
    match submit_operation(Operation::Open { path }).await? {
        OperationOutcome::Document(document) => Ok(document),
        OperationOutcome::Closed | OperationOutcome::Execution(_) => Err(Error::OpenNotPublished),
    }
}

pub async fn save(id: String) -> Result<Document, Error> {
    match submit_operation(Operation::Save { id }).await? {
        OperationOutcome::Document(document) => Ok(document),
        OperationOutcome::Closed | OperationOutcome::Execution(_) => Err(Error::SaveNotPublished),
    }
}

pub async fn close(id: String, discard: bool) -> Result<(), Error> {
    match submit_operation(Operation::Close { id, discard }).await? {
        OperationOutcome::Closed => Ok(()),
        OperationOutcome::Document(_) | OperationOutcome::Execution(_) => {
            Err(Error::CloseNotPublished)
        }
    }
}

pub async fn undo(id: String) -> Result<Document, Error> {
    history(id, HistoryDirection::Undo).await
}

pub async fn redo(id: String) -> Result<Document, Error> {
    history(id, HistoryDirection::Redo).await
}

async fn history(id: String, direction: HistoryDirection) -> Result<Document, Error> {
    match submit_operation(Operation::History { id, direction }).await? {
        OperationOutcome::Document(document) => Ok(document),
        OperationOutcome::Closed | OperationOutcome::Execution(_) => Err(Error::DocumentGone),
    }
}

pub fn admit_execution(
    id: String,
    execution: Execution,
    output: OutputStream,
    reservation: ExecutionReservation,
) -> Result<ExecutionAdmission, Error> {
    let deadline = Instant::now() + EXECUTION_START_TIMEOUT;
    let (job_id, receiver, should_wake, immediate) = {
        let mut scheduler = SCHEDULER
            .lock()
            .map_err(|_| Error::SchedulerStateUnavailable)?;

        if scheduler.stopping {
            return Err(Error::PluginStopping);
        }

        if scheduler.quarantined {
            return Err(Error::NativeMutationStateUnknown);
        }

        let job_id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
        let (completion, receiver) = oneshot::channel();

        if execution.outcome().is_some() {
            let operation = Operation::Execute {
                id,
                execution: Box::new(execution),
            };

            let outcome = match prepare(&operation, &scheduler.documents) {
                Prepared::Immediate(outcome) => outcome,
                Prepared::Native(_) => Err(Error::ExecutionNotFinished),
            };

            let output = operation.output_sink();
            (job_id, receiver, false, Some((completion, outcome, output)))
        } else {
            if scheduler.mutation_job_count() >= MAX_MUTATION_JOBS {
                return Err(Error::MutationCapacity);
            }

            let (execution_count, source_bytes) = scheduler.execution_usage();

            if execution_count >= MAX_ADMITTED_EXECUTIONS
                || source_bytes.saturating_add(execution.source_bytes()) > MAX_ADMITTED_SOURCE_BYTES
            {
                return Err(Error::ExecutionCapacity);
            }

            scheduler.pending.push_back(MutationJob {
                job_id,
                operation: Operation::Execute {
                    id,
                    execution: Box::new(execution),
                },
                native_target: None,
                start_deadline: Some(deadline),
                waiting_for_readiness: false,
                retry_at: None,
                retry_delay: BUSY_RETRY_INITIAL,
                _execution_reservation: Some(reservation),
                completion,
            });
            let should_wake = scheduler.request_wake();
            (job_id, receiver, should_wake, None)
        }
    };

    if let Some((completion, outcome, output)) = immediate {
        if let Some(output) = output {
            output.finish();
        }

        let _ = completion.send(outcome);
    }

    if should_wake {
        schedule_native_actions();
    }

    TIMERS_CHANGED.notify_one();

    Ok(ExecutionAdmission {
        job_id,
        output,
        completion: ExecutionCompletion { receiver },
    })
}

pub fn try_reserve_execution() -> Option<ExecutionReservation> {
    EXECUTION_RESERVATIONS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current < MAX_ADMITTED_EXECUTIONS).then_some(current + 1)
        })
        .ok()
        .map(|_| ExecutionReservation {
            _inner: Arc::new(ExecutionReservationInner),
        })
}

impl ExecutionAdmission {
    pub fn into_parts(self) -> (MutationJobId, OutputStream, ExecutionCompletion) {
        (self.job_id, self.output, self.completion)
    }
}

impl ExecutionCompletion {
    pub async fn wait(self) -> Result<ExecutionOutcome, Error> {
        match self.receiver.await.map_err(|_| Error::Stopped)?? {
            OperationOutcome::Execution(outcome) => Ok(outcome),
            OperationOutcome::Document(_) | OperationOutcome::Closed => {
                Err(Error::ExecutionNotFinished)
            }
        }
    }
}

pub fn list() -> Result<Vec<Document>, Error> {
    SCHEDULER
        .lock()
        .map_err(|_| Error::SchedulerStateUnavailable)
        .map(|scheduler| scheduler.documents.list())
}

pub fn replace_document_snapshot(documents: Vec<crate::ffi::NativeDocumentSnapshot>) {
    if let Ok(mut scheduler) = SCHEDULER.lock() {
        scheduler.documents.replace_snapshot(documents);
    }
}

pub fn start() {
    if let Ok(mut scheduler) = SCHEDULER.lock() {
        scheduler.stopping = false;
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

    TIMERS_CHANGED.notify_one();
}

async fn submit_operation(operation: Operation) -> Result<OperationOutcome, Error> {
    let (completed, should_wake) = {
        let mut scheduler = SCHEDULER
            .lock()
            .map_err(|_| Error::SchedulerStateUnavailable)?;

        if scheduler.stopping {
            return Err(Error::PluginStopping);
        }

        if scheduler.quarantined {
            return Err(Error::NativeMutationStateUnknown);
        }

        if scheduler.mutation_job_count() >= MAX_MUTATION_JOBS {
            return Err(Error::MutationCapacity);
        }

        if scheduler.idle()
            && let Prepared::Immediate(outcome) = prepare(&operation, &scheduler.documents)
        {
            return outcome;
        }

        let job_id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
        let (completion, completed) = oneshot::channel();
        scheduler.pending.push_back(MutationJob {
            job_id,
            operation,
            native_target: None,
            start_deadline: None,
            waiting_for_readiness: false,
            retry_at: None,
            retry_delay: BUSY_RETRY_INITIAL,
            _execution_reservation: None,
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

pub fn take_native_action() -> NativeAction {
    let decision = {
        let Ok(mut scheduler) = SCHEDULER.lock() else {
            return empty_action();
        };

        scheduler.wake_pending = false;

        if scheduler.stopping || scheduler.quarantined || scheduler.active.is_some() {
            return empty_action();
        }

        let Some(mut job) = scheduler.pending.pop_front() else {
            return empty_action();
        };

        if job.waiting_for_readiness {
            scheduler.pending.push_front(job);

            return empty_action();
        }

        job.expire_if_due(Instant::now());

        match prepare(&job.operation, &scheduler.documents) {
            Prepared::Immediate(outcome) => {
                TakeDecision::Complete(job.completion, outcome, job.operation.output_sink())
            }
            Prepared::Native(mut action) => {
                action.job_id = job.job_id;
                job.native_target = (action.document_token != 0 || action.database_token != 0)
                    .then_some(NativeDocumentKey {
                        document_token: action.document_token,
                        database_token: action.database_token,
                    });
                scheduler.active = Some(job);
                TakeDecision::Action(action)
            }
        }
    };

    match decision {
        TakeDecision::Action(action) => action,
        TakeDecision::Complete(completion, outcome, output) => {
            if let Some(output) = output {
                output.finish();
            }

            let _ = completion.send(outcome);
            empty_action()
        }
    }
}

pub fn complete_native_action(job_id: MutationJobId, result: NativeActionResult) {
    let quarantine = native_result_requires_quarantine(result.kind);
    complete_native_action_with_quarantine(job_id, result, quarantine);
}

fn native_result_requires_quarantine(kind: NativeActionResultKind) -> bool {
    matches!(
        kind,
        NativeActionResultKind::DocumentContextRestoreFailed
            | NativeActionResultKind::ExecutionBridgeFinalizationFailed
            | NativeActionResultKind::ExecutionBridgeSymbolsClearFailed
    )
}

fn complete_native_action_with_quarantine(
    job_id: MutationJobId,
    mut result: NativeActionResult,
    quarantine: bool,
) {
    bound_diagnostic(&mut result.native_detail);
    let (completion, pending) = {
        let Ok(mut scheduler) = SCHEDULER.lock() else {
            return;
        };

        let Some(mut job) = scheduler.active.take() else {
            return;
        };

        if job.job_id != job_id {
            scheduler.active = Some(job);

            return;
        }

        let settled_before_start = if result.kind == NativeActionResultKind::NotQuiescent
            && job.execution_has_not_handed_off_form()
        {
            if job.finish_cancel_before_start()
                || job.expire_if_due(Instant::now())
                || job.execution_has_outcome()
            {
                true
            } else {
                job.native_target = None;
                job.defer_for_readiness(Instant::now());
                scheduler.pending.push_front(job);
                TIMERS_CHANGED.notify_one();

                return;
            }
        } else {
            false
        };

        let output = job.operation.output_sink();
        let native_target = job.native_target;
        let outcome = if settled_before_start {
            finalize(&mut job.operation, &scheduler.documents, native_target)
        } else {
            complete_operation(
                result,
                &mut job.operation,
                &scheduler.documents,
                native_target,
            )
        };

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

        let _ = completion.send(Err(Error::NativeMutationStateUnknown));
    }
}

pub(crate) fn complete_execution_native_action(
    job_id: MutationJobId,
    result: NativeActionResult,
    observation: NativeExecutionFinalizationObservation,
) {
    let decision = classify_execution_finalization(result, observation);
    complete_native_action_with_quarantine(job_id, decision.result, decision.quarantine);
}

struct ExecutionFinalizationDecision {
    result: NativeActionResult,
    quarantine: bool,
}

fn classify_execution_finalization(
    mut result: NativeActionResult,
    observation: NativeExecutionFinalizationObservation,
) -> ExecutionFinalizationDecision {
    let native_state_unproved = observation.native_state_unproved();
    let quarantine = native_state_unproved || native_result_requires_quarantine(result.kind);
    let preserve_symbol_failure = result.kind
        == NativeActionResultKind::ExecutionBridgeSymbolsClearFailed
        && observation.only_symbol_cleanup_unproved();

    if native_state_unproved
        && result.kind != NativeActionResultKind::DocumentContextRestoreFailed
        && !preserve_symbol_failure
    {
        result.kind = NativeActionResultKind::ExecutionBridgeFinalizationFailed;
    }

    ExecutionFinalizationDecision { result, quarantine }
}

pub fn cancel_execution(job_id: MutationJobId) -> CancelResult {
    enum Action {
        Active(OutputSink),
        Pending {
            output: OutputSink,
            completion: oneshot::Sender<Result<OperationOutcome, Error>>,
            should_wake: bool,
        },
        Result(CancelResult),
    }

    let action = {
        let Ok(mut scheduler) = SCHEDULER.lock() else {
            return CancelResult::Unavailable;
        };

        if let Some(job) = scheduler.active.as_mut()
            && job.job_id == job_id
        {
            match &mut job.operation {
                Operation::Execute { execution, .. } => {
                    if execution.request_cancel() {
                        Action::Active(execution.output_sink())
                    } else {
                        Action::Result(CancelResult::TooLate)
                    }
                }

                Operation::Open { .. }
                | Operation::Save { .. }
                | Operation::Close { .. }
                | Operation::History { .. } => Action::Result(CancelResult::NotFound),
            }
        } else if let Some(index) = scheduler.pending.iter().position(|job| {
            job.job_id == job_id && matches!(job.operation, Operation::Execute { .. })
        }) {
            let output = match &mut scheduler.pending[index].operation {
                Operation::Execute { execution, .. } => execution
                    .cancel_before_start()
                    .then(|| execution.output_sink()),
                Operation::Open { .. }
                | Operation::Save { .. }
                | Operation::Close { .. }
                | Operation::History { .. } => {
                    unreachable!("only queued executions are selected")
                }
            };

            if let Some(output) = output {
                let job = scheduler.pending.remove(index).expect("queued job exists");
                let should_wake = scheduler.request_wake();
                Action::Pending {
                    output,
                    completion: job.completion,
                    should_wake,
                }
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

        Action::Pending {
            output,
            completion,
            should_wake,
        } => {
            output.request_cancel();
            let _ = completion.send(Ok(OperationOutcome::Execution(ExecutionOutcome::Cancelled)));

            if should_wake {
                schedule_native_actions();
            }

            TIMERS_CHANGED.notify_one();
            CancelResult::Accepted
        }
        Action::Result(result) => result,
    }
}

pub async fn drive_timers() {
    loop {
        let changed = TIMERS_CHANGED.notified();

        match next_timer_deadline() {
            Some(deadline) => {
                tokio::select! {
                    _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                        process_due_timers(Instant::now());
                    }
                    _ = changed => {}
                }
            }
            None => changed.await,
        }
    }
}

fn next_timer_deadline() -> Option<Instant> {
    let scheduler = SCHEDULER.lock().ok()?;
    let execution_start = scheduler
        .pending
        .iter()
        .chain(scheduler.active.iter())
        .filter(|job| job.execution_start_pending())
        .filter_map(|job| job.start_deadline)
        .min();
    let retry = scheduler
        .pending
        .front()
        .filter(|job| job.waiting_for_readiness)
        .and_then(|job| job.retry_at);
    execution_start.into_iter().chain(retry).min()
}

fn process_due_timers(now: Instant) {
    let Some((expired, should_wake)) = SCHEDULER.lock().ok().map(|mut scheduler| {
        if let Some(active) = scheduler.active.as_mut() {
            active.expire_if_due(now);
        }

        let mut expired = Vec::new();
        let mut index = 0;

        while index < scheduler.pending.len() {
            let should_expire = scheduler.pending[index].deadline_is_due(now);

            if !should_expire {
                index += 1;
                continue;
            }

            let mut job = scheduler.pending.remove(index).expect("queued job exists");

            if job.expire_if_due(now) {
                let output = job.operation.output_sink();
                let outcome = finalize(&mut job.operation, &scheduler.documents, None);
                expired.push((job.completion, outcome, output));
            } else {
                scheduler.pending.insert(index, job);
                index += 1;
            }
        }

        if let Some(head) = scheduler.pending.front_mut()
            && head.waiting_for_readiness
            && head.retry_at.is_some_and(|retry_at| now >= retry_at)
        {
            head.waiting_for_readiness = false;
            head.retry_at = None;
        }

        let should_wake = scheduler.request_wake();
        (expired, should_wake)
    }) else {
        return;
    };

    for (completion, outcome, output) in expired {
        if let Some(output) = output {
            output.finish();
        }

        let _ = completion.send(outcome);
    }

    if should_wake {
        schedule_native_actions();
    }
}

pub fn native_state_may_be_ready() {
    let should_wake = SCHEDULER.lock().is_ok_and(|mut scheduler| {
        if let Some(job) = scheduler.pending.front_mut() {
            job.waiting_for_readiness = false;
            job.retry_at = None;
            job.retry_delay = BUSY_RETRY_INITIAL;
        }

        scheduler.request_wake()
    });

    if should_wake {
        schedule_native_actions();
    }

    TIMERS_CHANGED.notify_one();
}

pub fn take_execution_step(job_id: MutationJobId) -> NativeExecutionStep {
    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return NativeExecutionStep::invalid();
    };

    let Some(job) = scheduler.active.as_mut() else {
        return NativeExecutionStep::invalid();
    };

    if job.job_id != job_id {
        return NativeExecutionStep::invalid();
    }

    job.expire_if_due(Instant::now());

    match &mut job.operation {
        Operation::Execute { execution, .. } => execution.take_step(),
        Operation::Open { .. }
        | Operation::Save { .. }
        | Operation::Close { .. }
        | Operation::History { .. } => NativeExecutionStep::invalid(),
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
            Operation::Execute { execution, .. } => execution.acquire_println_output(),
            Operation::Open { .. }
            | Operation::Save { .. }
            | Operation::Close { .. }
            | Operation::History { .. } => None,
        }
    };

    lease.map_or_else(NativeValueWriter::inactive, NativeValueWriter::println)
}

pub fn begin_eval_value(
    job_id: MutationJobId,
    document_token: usize,
    database_token: usize,
) -> NativeValueWriter {
    let lease = {
        let Ok(scheduler) = SCHEDULER.lock() else {
            return NativeValueWriter::inactive();
        };

        let Some(job) = scheduler.active.as_ref() else {
            return NativeValueWriter::inactive();
        };

        if job.job_id != job_id
            || job.native_target
                != Some(NativeDocumentKey {
                    document_token,
                    database_token,
                })
        {
            return NativeValueWriter::inactive();
        }

        match &job.operation {
            Operation::Execute { execution, .. } => execution.acquire_eval_value_output(),
            Operation::Open { .. }
            | Operation::Save { .. }
            | Operation::Close { .. }
            | Operation::History { .. } => None,
        }
    };

    lease.map_or_else(NativeValueWriter::inactive, NativeValueWriter::eval_value)
}

pub fn complete_execution_step(job_id: MutationJobId, mut result: ExecutionStepResult) -> bool {
    bound_diagnostic(&mut result.detail);
    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return false;
    };

    let Some(job) = scheduler.active.as_mut() else {
        return false;
    };

    if job.job_id != job_id {
        return false;
    }

    match &mut job.operation {
        Operation::Execute { execution, .. } => execution.complete_step(result),
        Operation::Open { .. }
        | Operation::Save { .. }
        | Operation::Close { .. }
        | Operation::History { .. } => false,
    }
}

pub fn abandon_execution(job_id: MutationJobId, mut result: ExecutionStepResult) -> bool {
    bound_diagnostic(&mut result.detail);
    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return false;
    };

    let Some(job) = scheduler.active.as_mut() else {
        return false;
    };

    if job.job_id != job_id {
        return false;
    }

    match &mut job.operation {
        Operation::Execute { execution, .. } => execution.abandon(result),
        Operation::Open { .. }
        | Operation::Save { .. }
        | Operation::Close { .. }
        | Operation::History { .. } => false,
    }
}

pub fn try_claim_native_action_wake() -> bool {
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

    let Some(completions) = completions else {
        return;
    };

    for (completion, output) in completions {
        if let Some(output) = output {
            output.stop();
        }

        let _ = completion.send(Err(Error::ScheduleFailed(status)));
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
        Operation::History { id, direction } => match documents.find_by_id(id) {
            Some(target) => Prepared::Native(native_action(
                match direction {
                    HistoryDirection::Undo => NativeActionKind::Undo,
                    HistoryDirection::Redo => NativeActionKind::Redo,
                },
                Some(target.native_key),
                String::new(),
                false,
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
                NativeActionKind::QueueExecutionDriver,
                Some(target.native_key),
                String::new(),
                false,
            )),
            None => Prepared::Immediate(Err(Error::DocumentNotFound(id.clone()))),
        },
    }
}

fn prepare_save(id: &str, target: DocumentTarget) -> Prepared {
    if target.document.read_only {
        return Prepared::Immediate(Err(Error::ReadOnly(id.to_owned())));
    }

    let Some(file_path) = target.document.file_path.as_deref() else {
        return Prepared::Immediate(Err(Error::Unnamed(id.to_owned())));
    };

    if !is_dwg(std::path::Path::new(file_path)) {
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
    native_target: Option<NativeDocumentKey>,
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
        Operation::History { id, .. } => {
            let expected = native_target.ok_or(Error::DocumentGone)?;
            let target = documents.find_by_id(id).ok_or(Error::DocumentGone)?;

            if target.native_key != expected {
                Err(Error::DocumentGenerationChanged)
            } else {
                Ok(OperationOutcome::Document(target.document))
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
    native_target: Option<NativeDocumentKey>,
) -> Result<OperationOutcome, Error> {
    if matches!(
        operation,
        Operation::Execute { execution, .. } if execution.outcome().is_some()
    ) && !matches!(
        result.kind,
        NativeActionResultKind::DocumentContextRestoreFailed
            | NativeActionResultKind::ExecutionBridgeFinalizationFailed
            | NativeActionResultKind::ExecutionBridgeSymbolsClearFailed
            | NativeActionResultKind::ExecutionBridgeFailed
    ) {
        return finalize(operation, documents, native_target);
    }

    if result.kind == NativeActionResultKind::ExecutionBridgeSymbolsClearFailed
        && let Operation::Execute { execution, .. } = operation
        && matches!(execution.outcome(), Some(ExecutionOutcome::Failure(_)))
    {
        return finalize(operation, documents, native_target);
    }

    if matches!(
        result.kind,
        NativeActionResultKind::DocumentContextRestoreFailed
            | NativeActionResultKind::ExecutionBridgeFinalizationFailed
    ) && let Operation::Execute { execution, .. } = operation
        && execution.outcome().is_some()
    {
        let recorded = execution.record_bridge_finalization_failure(ExecutionStepResult {
            kind: crate::execution::ExecutionStepResultKind::NativeError,
            native_status: result.native_status,
            lisp_errno: 0,
            detail: std::mem::take(&mut result.native_detail),
            bridge_symbols_clear_status: 0,
        });
        debug_assert!(recorded);

        return finalize(operation, documents, native_target);
    }

    interpret(result, operation)?;
    finalize(operation, documents, native_target)
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
        job_id: 0,
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
        NativeActionResultKind::DocumentGenerationChanged => Err(Error::DocumentGenerationChanged),
        NativeActionResultKind::Unnamed => Err(Error::Unnamed(operation.document_id().to_owned())),
        NativeActionResultKind::ReadOnly => {
            Err(Error::ReadOnly(operation.document_id().to_owned()))
        }
        NativeActionResultKind::Dirty => Err(Error::Dirty(operation.document_id().to_owned())),
        NativeActionResultKind::OpenFailed => Err(Error::OpenFailed(failure)),
        NativeActionResultKind::LockFailed => Err(Error::LockFailed(failure)),
        NativeActionResultKind::SaveFailed => Err(Error::SaveFailed(failure)),
        NativeActionResultKind::CloseFailed => Err(Error::CloseFailed(failure)),
        NativeActionResultKind::HistoryFailed => {
            let Operation::History { direction, .. } = operation else {
                return Err(Error::UnknownResult(result.kind.repr));
            };

            Err(Error::HistoryFailed {
                direction: *direction,
                failure,
            })
        }
        NativeActionResultKind::NotQuiescent => Err(Error::NotQuiescent),
        NativeActionResultKind::UndoDisabled => Err(Error::UndoDisabled),
        NativeActionResultKind::DocumentContextFailed => Err(Error::DocumentContextFailed(failure)),
        NativeActionResultKind::DocumentContextRestoreFailed => {
            Err(Error::DocumentContextRestoreFailed(failure))
        }
        NativeActionResultKind::ExecutionBridgeFinalizationFailed => {
            Err(Error::ExecutionBridgeFinalizationFailed(failure))
        }
        NativeActionResultKind::ExecutionBridgeSymbolsClearFailed => {
            Err(Error::ExecutionBridgeSymbolsClearFailed(failure))
        }
        NativeActionResultKind::ExecutionBridgeFailed => Err(Error::ExecutionBridgeFailed(failure)),
        kind => Err(Error::UnknownResult(kind.repr)),
    }
}

impl MutationJob {
    fn deadline_is_due(&self, now: Instant) -> bool {
        self.start_deadline.is_some_and(|deadline| now >= deadline)
            && self.execution_start_pending()
    }

    fn expire_if_due(&mut self, now: Instant) -> bool {
        if !self.deadline_is_due(now) {
            return false;
        }

        match &mut self.operation {
            Operation::Execute { execution, .. } => execution.expire_before_start(format!(
                "execution did not start within {} seconds",
                EXECUTION_START_TIMEOUT.as_secs()
            )),
            Operation::Open { .. }
            | Operation::Save { .. }
            | Operation::Close { .. }
            | Operation::History { .. } => false,
        }
    }

    fn execution_has_not_handed_off_form(&self) -> bool {
        matches!(
            &self.operation,
            Operation::Execute { execution, .. } if !execution.has_handed_off_form()
        )
    }

    fn execution_start_pending(&self) -> bool {
        matches!(
            &self.operation,
            Operation::Execute { execution, .. } if execution.start_deadline_pending()
        )
    }

    fn execution_has_outcome(&self) -> bool {
        matches!(
            &self.operation,
            Operation::Execute { execution, .. } if execution.outcome().is_some()
        )
    }

    fn finish_cancel_before_start(&mut self) -> bool {
        match &mut self.operation {
            Operation::Execute { execution, .. }
                if !execution.has_handed_off_form() && execution.cancellation_requested() =>
            {
                execution.cancel_before_start()
            }

            Operation::Execute { .. }
            | Operation::Open { .. }
            | Operation::Save { .. }
            | Operation::Close { .. }
            | Operation::History { .. } => false,
        }
    }

    fn defer_for_readiness(&mut self, now: Instant) {
        self.waiting_for_readiness = true;
        self.retry_at = Some(now + self.retry_delay);
        self.retry_delay = self.retry_delay.saturating_mul(2).min(BUSY_RETRY_MAX);
    }
}

impl Operation {
    fn output_sink(&self) -> Option<OutputSink> {
        match self {
            Self::Execute { execution, .. } => Some(execution.output_sink()),
            Self::Open { .. } | Self::Save { .. } | Self::Close { .. } | Self::History { .. } => {
                None
            }
        }
    }

    fn document_id(&self) -> &str {
        match self {
            Self::Open { .. } => "",
            Self::Save { id }
            | Self::Close { id, .. }
            | Self::History { id, .. }
            | Self::Execute { id, .. } => id,
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
    fn execution_finalization_classification_uses_native_facts_in_rust() {
        let clean = classify_execution_finalization(
            result(NativeActionResultKind::Success),
            ExecutionFinalizationObservation::default(),
        );
        assert_eq!(clean.result.kind, NativeActionResultKind::Success);
        assert!(!clean.quarantine);

        for observation in [
            ExecutionFinalizationObservation {
                undo_group_may_be_open: true,
                ..ExecutionFinalizationObservation::default()
            },
            ExecutionFinalizationObservation {
                bridge_symbols_may_be_retained: true,
                ..ExecutionFinalizationObservation::default()
            },
            ExecutionFinalizationObservation {
                staged_form_may_be_retained: true,
                ..ExecutionFinalizationObservation::default()
            },
            ExecutionFinalizationObservation {
                value_writer_active: true,
                ..ExecutionFinalizationObservation::default()
            },
            ExecutionFinalizationObservation {
                terminal_cleanup_failed: true,
                ..ExecutionFinalizationObservation::default()
            },
        ] {
            let retained = classify_execution_finalization(
                result(NativeActionResultKind::ExecutionBridgeFailed),
                observation,
            );
            assert_eq!(
                retained.result.kind,
                NativeActionResultKind::ExecutionBridgeFinalizationFailed
            );
            assert!(retained.quarantine);
        }

        let restore = classify_execution_finalization(
            result(NativeActionResultKind::DocumentContextRestoreFailed),
            ExecutionFinalizationObservation {
                undo_group_may_be_open: true,
                ..ExecutionFinalizationObservation::default()
            },
        );
        assert_eq!(
            restore.result.kind,
            NativeActionResultKind::DocumentContextRestoreFailed
        );
        assert!(restore.quarantine);

        let retained_symbols = classify_execution_finalization(
            result(NativeActionResultKind::ExecutionBridgeSymbolsClearFailed),
            ExecutionFinalizationObservation {
                bridge_symbols_may_be_retained: true,
                ..ExecutionFinalizationObservation::default()
            },
        );
        assert_eq!(
            retained_symbols.result.kind,
            NativeActionResultKind::ExecutionBridgeSymbolsClearFailed
        );
        assert!(retained_symbols.quarantine);

        let symbols_and_undo = classify_execution_finalization(
            result(NativeActionResultKind::ExecutionBridgeSymbolsClearFailed),
            ExecutionFinalizationObservation {
                undo_group_may_be_open: true,
                bridge_symbols_may_be_retained: true,
                ..ExecutionFinalizationObservation::default()
            },
        );
        assert_eq!(
            symbols_and_undo.result.kind,
            NativeActionResultKind::ExecutionBridgeFinalizationFailed
        );
        assert!(symbols_and_undo.quarantine);
    }

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
                result(NativeActionResultKind::DocumentGenerationChanged),
                &Operation::Save { id: "doc".into() },
            ),
            Err(Error::DocumentGenerationChanged)
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
        let save_action = take_native_action();
        assert_eq!(save_action.kind, NativeActionKind::Save);

        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());

        let second = tokio::spawn(close(id.clone(), true));
        tokio::task::yield_now().await;
        assert_eq!(take_native_action().kind, NativeActionKind::None);

        replace_document_snapshot(vec![document(1, 101, false)]);
        complete_native_action(save_action.job_id, result(NativeActionResultKind::Success));
        assert!(try_claim_native_action_wake());

        let close_action = take_native_action();
        assert_eq!(close_action.kind, NativeActionKind::Close);
        replace_document_snapshot(Vec::new());
        complete_native_action(close_action.job_id, result(NativeActionResultKind::Success));
        assert!(second.await.unwrap().is_ok());
        stop();
    }

    #[tokio::test]
    async fn drawing_history_actions_share_the_fifo_and_exact_generation() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();

        let undo_waiter = tokio::spawn(undo(id.clone()));
        tokio::task::yield_now().await;
        let undo_action = take_native_action();
        assert_eq!(undo_action.kind, NativeActionKind::Undo);
        assert_eq!(undo_action.document_token, 1);
        assert_eq!(undo_action.database_token, 101);

        let redo_waiter = tokio::spawn(redo(id.clone()));
        tokio::task::yield_now().await;
        assert_eq!(take_native_action().kind, NativeActionKind::None);

        replace_document_snapshot(vec![document(1, 101, true)]);
        complete_native_action(undo_action.job_id, result(NativeActionResultKind::Success));
        assert!(undo_waiter.await.unwrap().unwrap().modified);

        let redo_action = take_native_action();
        assert_eq!(redo_action.kind, NativeActionKind::Redo);
        assert_eq!(redo_action.document_token, 1);
        assert_eq!(redo_action.database_token, 101);
        replace_document_snapshot(vec![document(1, 101, false)]);
        complete_native_action(redo_action.job_id, result(NativeActionResultKind::Success));
        assert!(!redo_waiter.await.unwrap().unwrap().modified);
        stop();
    }

    #[tokio::test]
    async fn drawing_history_fails_closed_on_missing_or_replaced_documents() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();

        assert_eq!(
            undo("absent".into()).await,
            Err(Error::DocumentNotFound("absent".into()))
        );

        let waiter = tokio::spawn(redo(id));
        tokio::task::yield_now().await;
        let action = take_native_action();
        assert_eq!(action.kind, NativeActionKind::Redo);
        replace_document_snapshot(vec![document(1, 201, false)]);
        complete_native_action(action.job_id, result(NativeActionResultKind::Success));
        assert_eq!(waiter.await.unwrap(), Err(Error::DocumentGenerationChanged));
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

        assert_eq!(take_native_action().kind, NativeActionKind::None);
        stop();
    }

    #[tokio::test]
    async fn shutdown_rejects_pending_work_but_preserves_the_active_operation() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, true)]);
        let id = list().unwrap()[0].id.clone();

        let active = tokio::spawn(save(id.clone()));
        tokio::task::yield_now().await;
        let save_action = take_native_action();
        assert_eq!(save_action.kind, NativeActionKind::Save);

        let pending = tokio::spawn(close(id, true));
        tokio::task::yield_now().await;
        stop();

        assert_eq!(pending.await.unwrap(), Err(Error::PluginStopping));
        assert_eq!(take_native_action().kind, NativeActionKind::None);

        replace_document_snapshot(vec![document(1, 101, false)]);
        complete_native_action(save_action.job_id, result(NativeActionResultKind::Success));
        assert!(active.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn keeps_one_mutation_job_active_across_a_batch() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) = Execution::new(
            ExecutionMode::Exec,
            "batch.lsp".into(),
            "first\nsecond".into(),
        )
        .unwrap();
        let (_output, pending) = spawn_test_execution(id.clone(), execution, output);
        tokio::task::yield_now().await;

        let action = take_native_action();
        assert_eq!(action.kind, NativeActionKind::QueueExecutionDriver);
        assert_eq!(action.document_token, 1);
        assert_eq!(action.database_token, 101);

        let begin = take_execution_step(action.job_id);
        assert_eq!(
            begin.kind(),
            crate::execution::ExecutionStepKind::BeginUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));

        let first = take_execution_step(action.job_id);
        assert_eq!(first.source(), "first");
        assert!(complete_execution_step(action.job_id, step_success()));
        let second = take_execution_step(action.job_id);
        assert_eq!(second.source(), "second");
        assert!(complete_execution_step(action.job_id, step_success()));

        let commit = take_execution_step(action.job_id);
        assert_eq!(
            commit.kind(),
            crate::execution::ExecutionStepKind::CommitUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );

        complete_native_action(action.job_id, result(NativeActionResultKind::Success));
        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Success);
        stop();
    }

    #[tokio::test]
    async fn routes_println_only_to_the_exact_active_form() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let (mut output, pending) = spawn_test_execution(id, execution, output);
        tokio::task::yield_now().await;

        let action = take_native_action();
        assert!(!begin_println(1, 101).active());
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::BeginUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::EvaluateForm
        );
        assert!(!begin_println(2, 101).active());
        assert!(!begin_println(1, 202).active());

        let mut writer = begin_println(1, 101);
        assert!(writer.active());
        assert_eq!(writer.write(ValueEvent::BeginString), WriteResult::Continue);
        assert_eq!(
            writer.write(ValueEvent::StringChunk("created: 3")),
            WriteResult::Continue
        );
        assert_eq!(writer.write(ValueEvent::EndString), WriteResult::Continue);
        assert_eq!(writer.finish(), WriteResult::Continue);

        assert!(complete_execution_step(action.job_id, step_success()));
        assert!(!begin_println(1, 101).active());
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::CommitUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );
        complete_native_action(action.job_id, result(NativeActionResultKind::Success));

        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Success);
        let mut rendered = String::new();

        while let Some(chunk) = output.next_chunk().await {
            rendered.push_str(&chunk);
        }

        assert_eq!(rendered, "created: 3\n");
        stop();
    }

    #[tokio::test]
    async fn routes_the_eval_value_only_after_commit_and_only_once() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Eval, "inspect.lsp".into(), "form".into()).unwrap();
        let (mut output, pending) = spawn_test_execution(id, execution, output);
        tokio::task::yield_now().await;

        let action = take_native_action();
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::BeginUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        let form = take_execution_step(action.job_id);
        assert_eq!(
            form.kind(),
            crate::execution::ExecutionStepKind::EvaluateForm
        );
        assert!(form.retain_value());
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::CommitUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::EmitEvalValue
        );

        assert!(!begin_println(1, 101).active());
        assert!(!begin_eval_value(action.job_id + 1, 1, 101).active());
        assert!(!begin_eval_value(action.job_id, 2, 101).active());
        assert!(!begin_eval_value(action.job_id, 1, 202).active());
        let mut writer = begin_eval_value(action.job_id, 1, 101);
        assert!(writer.active());
        assert!(!begin_eval_value(action.job_id, 1, 101).active());
        assert_eq!(writer.write(ValueEvent::Integer(12)), WriteResult::Continue);
        assert_eq!(writer.finish(), WriteResult::Continue);
        assert!(!begin_eval_value(action.job_id, 1, 101).active());

        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );
        complete_native_action(action.job_id, result(NativeActionResultKind::Success));

        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Success);
        let mut rendered = String::new();

        while let Some(chunk) = output.next_chunk().await {
            rendered.push_str(&chunk);
        }

        assert_eq!(rendered, "12\n");
        stop();
    }

    #[tokio::test]
    async fn rolls_back_a_malformed_active_value_stream() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let (_output, pending) = spawn_test_execution(id, execution, output);
        tokio::task::yield_now().await;

        let action = take_native_action();
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::BeginUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::EvaluateForm
        );

        let mut writer = begin_println(1, 101);
        assert_eq!(
            writer.write(ValueEvent::EndList),
            WriteResult::InvalidSequence
        );
        assert_eq!(writer.finish(), WriteResult::InvalidSequence);
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::RollbackUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );
        complete_native_action(action.job_id, result(NativeActionResultKind::Success));

        assert_eq!(
            pending.await.unwrap().unwrap(),
            ExecutionOutcome::Failure(crate::execution::ExecutionFailure {
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
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let (_output, pending) = spawn_test_execution(id, execution, output);
        tokio::task::yield_now().await;

        let action = take_native_action();
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::BeginUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::EvaluateForm
        );

        let mut writer = begin_println(1, 101);
        assert!(writer.active());
        assert!(complete_execution_step(action.job_id, step_success()));
        assert!(!writer.active());
        assert_eq!(writer.write(ValueEvent::Integer(9)), WriteResult::Inactive);
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::RollbackUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );
        complete_native_action(action.job_id, result(NativeActionResultKind::Success));

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
        let save_action = take_native_action();
        assert_eq!(save_action.kind, NativeActionKind::Save);

        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let (mut output, pending) = spawn_test_execution(id.clone(), execution, output);
        tokio::task::yield_now().await;
        let job_id = SCHEDULER
            .lock()
            .unwrap()
            .pending
            .front()
            .expect("execution is queued behind save")
            .job_id;

        assert_eq!(cancel_execution(job_id), CancelResult::Accepted);
        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Cancelled);
        assert_eq!(output.next_chunk().await, None);

        replace_document_snapshot(vec![document(1, 101, false)]);
        complete_native_action(save_action.job_id, result(NativeActionResultKind::Success));
        assert!(active.await.unwrap().is_ok());
        assert!(!try_claim_native_action_wake());
        stop();
    }

    #[tokio::test]
    async fn dropping_a_queued_execution_waiter_keeps_the_job_and_output_alive() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, true)]);
        let id = list().unwrap()[0].id.clone();

        let active = tokio::spawn(save(id.clone()));
        tokio::task::yield_now().await;
        let save_action = take_native_action();
        assert_eq!(save_action.kind, NativeActionKind::Save);

        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let (mut output, queued) = spawn_test_execution(id, execution, output);
        tokio::task::yield_now().await;
        let job_id = SCHEDULER
            .lock()
            .unwrap()
            .pending
            .front()
            .expect("execution is queued behind save")
            .job_id;

        queued.abort();
        assert!(queued.await.unwrap_err().is_cancelled());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), output.next_chunk())
                .await
                .is_err()
        );
        assert_eq!(cancel_execution(job_id), CancelResult::Accepted);
        assert_eq!(output.next_chunk().await, None);

        replace_document_snapshot(vec![document(1, 101, false)]);
        complete_native_action(save_action.job_id, result(NativeActionResultKind::Success));
        assert!(active.await.unwrap().is_ok());
        assert!(!try_claim_native_action_wake());
        stop();
    }

    #[tokio::test]
    async fn wake_failure_stops_a_pending_execution_output_stream() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let (mut output, pending) = spawn_test_execution(id, execution, output);
        tokio::task::yield_now().await;

        wake_failed(42);

        assert_eq!(pending.await.unwrap(), Err(Error::ScheduleFailed(42)));
        assert_eq!(output.next_chunk().await, None);
        assert_eq!(take_native_action().kind, NativeActionKind::None);
        stop();
    }

    #[tokio::test]
    async fn active_cancellation_rolls_back_after_the_current_form() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let (mut output, pending) = spawn_test_execution(id, execution, output);
        tokio::task::yield_now().await;

        let action = take_native_action();
        assert_eq!(action.kind, NativeActionKind::QueueExecutionDriver);
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::BeginUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::EvaluateForm
        );

        assert_eq!(cancel_execution(action.job_id), CancelResult::Accepted);
        assert_eq!(cancel_execution(action.job_id), CancelResult::Accepted);
        assert_eq!(output.next_chunk().await, None);
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::RollbackUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );

        complete_native_action(action.job_id, result(NativeActionResultKind::Success));
        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Cancelled);
        stop();
    }

    #[tokio::test]
    async fn active_cancellation_before_the_first_form_closes_the_empty_undo_group() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let (mut output, pending) = spawn_test_execution(id, execution, output);
        tokio::task::yield_now().await;

        let action = take_native_action();
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::BeginUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(cancel_execution(action.job_id), CancelResult::Accepted);
        assert_eq!(output.next_chunk().await, None);
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::CloseEmptyUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );

        complete_native_action(action.job_id, result(NativeActionResultKind::Success));
        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Cancelled);
        stop();
    }

    #[tokio::test]
    async fn cancellation_after_commit_handoff_does_not_cancel_output() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let (mut output, pending) = spawn_test_execution(id, execution, output);
        tokio::task::yield_now().await;

        let action = take_native_action();
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::BeginUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::EvaluateForm
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::CommitUndoGroup
        );

        assert_eq!(cancel_execution(action.job_id), CancelResult::TooLate);
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );
        complete_native_action(action.job_id, result(NativeActionResultKind::Success));

        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Success);
        assert_eq!(output.next_chunk().await, None);
        stop();
    }

    #[tokio::test]
    async fn shutdown_wakes_output_and_cancels_an_active_execution_safely() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let (mut output, pending) = spawn_test_execution(id, execution, output);
        tokio::task::yield_now().await;

        let action = take_native_action();
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::BeginUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::EvaluateForm
        );

        stop();
        assert_eq!(output.next_chunk().await, None);
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::RollbackUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );
        complete_native_action(action.job_id, result(NativeActionResultKind::Success));
        assert_eq!(pending.await.unwrap().unwrap(), ExecutionOutcome::Cancelled);
    }

    #[tokio::test]
    async fn dropped_execution_waiter_does_not_release_the_active_mutation_job() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let (mut output, executing) = spawn_test_execution(id.clone(), execution, output);
        tokio::task::yield_now().await;

        let action = take_native_action();
        assert_eq!(action.kind, NativeActionKind::QueueExecutionDriver);
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::BeginUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        let form = take_execution_step(action.job_id);
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
        assert_eq!(take_native_action().kind, NativeActionKind::None);

        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::CommitUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );
        complete_native_action(action.job_id, result(NativeActionResultKind::Success));
        assert_eq!(output.next_chunk().await, None);

        assert!(try_claim_native_action_wake());
        let close_action = take_native_action();
        assert_eq!(close_action.kind, NativeActionKind::Close);
        replace_document_snapshot(Vec::new());
        complete_native_action(close_action.job_id, result(NativeActionResultKind::Success));
        assert!(later.await.unwrap().is_ok());
        stop();
    }

    #[tokio::test]
    async fn document_context_restore_failure_amends_a_terminal_execution_outcome() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "ok".into()).unwrap();
        let (_output, pending) = spawn_test_execution(id.clone(), execution, output);
        tokio::task::yield_now().await;

        let action = take_native_action();
        assert_eq!(action.kind, NativeActionKind::QueueExecutionDriver);
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::BeginUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::EvaluateForm
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::CommitUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );

        let blocked = tokio::spawn(save(id.clone()));
        tokio::task::yield_now().await;

        complete_native_action(
            action.job_id,
            NativeActionResult {
                kind: NativeActionResultKind::DocumentContextRestoreFailed,
                native_status: 42,
                native_detail: "unlock failed".into(),
            },
        );

        assert_eq!(
            pending.await.unwrap().unwrap(),
            ExecutionOutcome::Failure(crate::execution::ExecutionFailure {
                message: "unlock failed".into(),
                form_index: None,
                location: None,
                drawing_outcome: crate::execution::DrawingOutcome::Unknown,
            })
        );
        assert_eq!(
            blocked.await.unwrap(),
            Err(Error::NativeMutationStateUnknown)
        );
        assert_eq!(save(id).await, Err(Error::NativeMutationStateUnknown));
        assert_eq!(take_native_action().kind, NativeActionKind::None);
        stop();
    }

    #[tokio::test]
    async fn start_does_not_clear_native_state_quarantine() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        {
            let mut scheduler = SCHEDULER.lock().unwrap();
            scheduler.stopping = true;
            scheduler.quarantined = true;
        }

        start();

        {
            let scheduler = SCHEDULER.lock().unwrap();
            assert!(!scheduler.stopping);
            assert!(scheduler.quarantined);
        }

        assert_eq!(save(id).await, Err(Error::NativeMutationStateUnknown));
        reset(Vec::new());
        stop();
    }

    #[tokio::test]
    async fn retained_execution_state_quarantines_without_erasing_commit_evidence() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Eval, "inspect.lsp".into(), "form".into()).unwrap();
        let (_output, pending) = spawn_test_execution(id.clone(), execution, output);
        tokio::task::yield_now().await;

        let action = take_native_action();
        assert_eq!(action.kind, NativeActionKind::QueueExecutionDriver);
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::BeginUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::EvaluateForm
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::CommitUndoGroup
        );
        assert!(complete_execution_step(action.job_id, step_success()));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::EmitEvalValue
        );

        let mut writer = begin_eval_value(action.job_id, 1, 101);
        assert_eq!(writer.write(ValueEvent::Integer(12)), WriteResult::Continue);
        assert_eq!(writer.finish(), WriteResult::Continue);
        assert!(complete_execution_step(
            action.job_id,
            ExecutionStepResult {
                kind: crate::execution::ExecutionStepResultKind::NativeError,
                native_status: 42,
                lisp_errno: 0,
                detail: "could not clear the retained AutoLISP value".into(),
                bridge_symbols_clear_status: 0,
            }
        ));
        assert_eq!(
            take_execution_step(action.job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );

        let blocked = tokio::spawn(save(id.clone()));
        tokio::task::yield_now().await;
        complete_execution_native_action(
            action.job_id,
            NativeActionResult {
                kind: NativeActionResultKind::ExecutionBridgeSymbolsClearFailed,
                native_status: 42,
                native_detail: "reserved execution bridge state remains".into(),
            },
            ExecutionFinalizationObservation {
                bridge_symbols_may_be_retained: true,
                terminal_cleanup_failed: true,
                ..ExecutionFinalizationObservation::default()
            },
        );

        assert_eq!(
            pending.await.unwrap().unwrap(),
            ExecutionOutcome::Failure(crate::execution::ExecutionFailure {
                message: "could not clear the retained AutoLISP value".into(),
                form_index: Some(1),
                location: Some(crate::execution::SourceLocation {
                    source_name: "inspect.lsp".into(),
                    line: 1,
                    column: 1,
                }),
                drawing_outcome: crate::execution::DrawingOutcome::Committed,
            })
        );
        assert_eq!(
            blocked.await.unwrap(),
            Err(Error::NativeMutationStateUnknown)
        );
        assert_eq!(save(id).await, Err(Error::NativeMutationStateUnknown));
        assert_eq!(take_native_action().kind, NativeActionKind::None);
        stop();
    }

    #[tokio::test]
    async fn queued_execution_expires_without_starting_a_form() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, true)]);
        let id = list().unwrap()[0].id.clone();
        let saving = tokio::spawn(save(id.clone()));
        tokio::task::yield_now().await;
        let save_action = take_native_action();
        assert_eq!(save_action.kind, NativeActionKind::Save);

        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let admission = admit_test_execution(id, execution, output).unwrap();
        let (job_id, mut output, completion) = admission.into_parts();
        {
            let mut scheduler = SCHEDULER.lock().unwrap();
            let job = scheduler
                .pending
                .iter_mut()
                .find(|job| job.job_id == job_id)
                .unwrap();
            job.start_deadline = Some(Instant::now() - Duration::from_millis(1));
        }

        process_due_timers(Instant::now());

        assert_eq!(output.next_chunk().await, None);
        assert_eq!(
            completion.wait().await.unwrap(),
            ExecutionOutcome::Failure(crate::execution::ExecutionFailure {
                message: "execution did not start within 5 seconds".into(),
                form_index: None,
                location: None,
                drawing_outcome: crate::execution::DrawingOutcome::NotStarted,
            })
        );
        assert_eq!(take_native_action().kind, NativeActionKind::None);

        replace_document_snapshot(vec![document(1, 101, false)]);
        complete_native_action(save_action.job_id, result(NativeActionResultKind::Success));
        assert!(saving.await.unwrap().is_ok());
        stop();
    }

    #[tokio::test]
    async fn busy_execution_waits_for_a_readiness_retry_without_spinning() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let admission = admit_test_execution(id, execution, output).unwrap();
        let (job_id, _output, completion) = admission.into_parts();

        let first = take_native_action();
        assert_eq!(first.kind, NativeActionKind::QueueExecutionDriver);
        complete_native_action(first.job_id, result(NativeActionResultKind::NotQuiescent));
        assert_eq!(take_native_action().kind, NativeActionKind::None);

        process_due_timers(Instant::now() + BUSY_RETRY_MAX);
        let retried = take_native_action();
        assert_eq!(retried.kind, NativeActionKind::QueueExecutionDriver);
        assert_eq!(retried.job_id, job_id);
        assert_eq!(cancel_execution(job_id), CancelResult::Accepted);
        assert_eq!(
            take_execution_step(job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );
        complete_native_action(job_id, result(NativeActionResultKind::Success));
        assert_eq!(
            completion.wait().await.unwrap(),
            ExecutionOutcome::Cancelled
        );
        stop();
    }

    #[tokio::test]
    async fn deadline_wins_while_the_busy_probe_is_in_flight() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let admission = admit_test_execution(id, execution, output).unwrap();
        let (job_id, _output, completion) = admission.into_parts();
        let action = take_native_action();
        assert_eq!(action.kind, NativeActionKind::QueueExecutionDriver);
        {
            let mut scheduler = SCHEDULER.lock().unwrap();
            scheduler.active.as_mut().unwrap().start_deadline =
                Some(Instant::now() - Duration::from_millis(1));
        }

        process_due_timers(Instant::now());
        complete_native_action(job_id, result(NativeActionResultKind::NotQuiescent));

        assert!(matches!(
            completion.wait().await.unwrap(),
            ExecutionOutcome::Failure(crate::execution::ExecutionFailure {
                drawing_outcome: crate::execution::DrawingOutcome::NotStarted,
                ..
            })
        ));
        assert_eq!(take_native_action().kind, NativeActionKind::None);
        stop();
    }

    #[tokio::test]
    async fn deadline_winner_survives_a_native_preflight_failure() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let admission = admit_test_execution(id, execution, output).unwrap();
        let (job_id, _output, completion) = admission.into_parts();
        assert_eq!(
            take_native_action().kind,
            NativeActionKind::QueueExecutionDriver
        );
        {
            let mut scheduler = SCHEDULER.lock().unwrap();
            scheduler.active.as_mut().unwrap().start_deadline =
                Some(Instant::now() - Duration::from_millis(1));
        }

        process_due_timers(Instant::now());
        complete_native_action(job_id, result(NativeActionResultKind::UndoDisabled));

        assert_eq!(
            completion.wait().await.unwrap(),
            ExecutionOutcome::Failure(crate::execution::ExecutionFailure {
                message: "execution did not start within 5 seconds".into(),
                form_index: None,
                location: None,
                drawing_outcome: crate::execution::DrawingOutcome::NotStarted,
            })
        );
        stop();
    }

    #[tokio::test]
    async fn deadline_winner_survives_a_failing_begin_step() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let admission = admit_test_execution(id, execution, output).unwrap();
        let (job_id, _output, completion) = admission.into_parts();
        assert_eq!(
            take_native_action().kind,
            NativeActionKind::QueueExecutionDriver
        );
        assert_eq!(
            take_execution_step(job_id).kind(),
            crate::execution::ExecutionStepKind::BeginUndoGroup
        );
        {
            let mut scheduler = SCHEDULER.lock().unwrap();
            scheduler.active.as_mut().unwrap().start_deadline =
                Some(Instant::now() - Duration::from_millis(1));
        }

        process_due_timers(Instant::now());
        assert!(complete_execution_step(
            job_id,
            ExecutionStepResult {
                kind: crate::execution::ExecutionStepResultKind::NativeError,
                native_status: 42,
                lisp_errno: 0,
                detail: "undo begin failed".into(),
                bridge_symbols_clear_status: 0,
            }
        ));
        assert_eq!(
            take_execution_step(job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );
        complete_native_action(job_id, result(NativeActionResultKind::Success));

        let ExecutionOutcome::Failure(failure) = completion.wait().await.unwrap() else {
            panic!("the execution start deadline must remain the terminal cause");
        };

        assert!(
            failure
                .message
                .starts_with("execution did not start within 5 seconds")
        );
        assert!(failure.message.contains("undo begin failed"));
        assert_eq!(
            failure.drawing_outcome,
            crate::execution::DrawingOutcome::NotStarted
        );
        stop();
    }

    #[tokio::test]
    async fn admitted_execution_survives_dropped_rpc_observers() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let admission = admit_test_execution(id, execution, output).unwrap();
        let (job_id, output, completion) = admission.into_parts();
        drop(output);
        drop(completion);

        let action = take_native_action();
        assert_eq!(action.kind, NativeActionKind::QueueExecutionDriver);
        assert_eq!(action.job_id, job_id);
        assert_eq!(cancel_execution(job_id), CancelResult::Accepted);
        assert_eq!(
            take_execution_step(job_id).kind(),
            crate::execution::ExecutionStepKind::Done
        );
        complete_native_action(job_id, result(NativeActionResultKind::Success));
        assert_eq!(take_native_action().kind, NativeActionKind::None);
        stop();
    }

    #[tokio::test]
    async fn execution_count_capacity_is_released_by_queued_cancellation() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let mut admissions = Vec::new();

        for _ in 0..MAX_ADMITTED_EXECUTIONS {
            let (execution, output) =
                Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
            admissions.push(admit_test_execution(id.clone(), execution, output).unwrap());
        }

        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        assert!(matches!(
            admit_test_execution(id.clone(), execution, output),
            Err(Error::ExecutionCapacity)
        ));

        for admission in admissions {
            let (job_id, _output, completion) = admission.into_parts();
            assert_eq!(cancel_execution(job_id), CancelResult::Accepted);
            assert_eq!(
                completion.wait().await.unwrap(),
                ExecutionOutcome::Cancelled
            );
        }

        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let replacement = admit_test_execution(id, execution, output).unwrap();
        let (job_id, _output, completion) = replacement.into_parts();
        assert_eq!(cancel_execution(job_id), CancelResult::Accepted);
        assert_eq!(
            completion.wait().await.unwrap(),
            ExecutionOutcome::Cancelled
        );
        stop();
    }

    #[tokio::test]
    async fn detached_execution_retains_its_shared_admission_reservation() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();
        let response_reservation = try_reserve_execution().unwrap();
        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let admission =
            admit_execution(id, execution, output, response_reservation.clone()).unwrap();
        let (job_id, output, completion) = admission.into_parts();
        drop(output);
        drop(completion);
        drop(response_reservation);

        let other_reservations = (1..MAX_ADMITTED_EXECUTIONS)
            .map(|_| try_reserve_execution().unwrap())
            .collect::<Vec<_>>();
        assert!(try_reserve_execution().is_none());

        assert_eq!(cancel_execution(job_id), CancelResult::Accepted);
        let replacement = try_reserve_execution().unwrap();
        drop(replacement);
        drop(other_reservations);
        stop();
    }

    #[tokio::test]
    async fn queued_cancel_and_deadline_have_one_serialized_winner() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, false)]);
        let id = list().unwrap()[0].id.clone();

        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let admission = admit_test_execution(id.clone(), execution, output).unwrap();
        let (expired_id, _output, expired_completion) = admission.into_parts();
        {
            let mut scheduler = SCHEDULER.lock().unwrap();
            scheduler
                .pending
                .iter_mut()
                .find(|job| job.job_id == expired_id)
                .unwrap()
                .start_deadline = Some(Instant::now() - Duration::from_millis(1));
        }

        process_due_timers(Instant::now());
        assert_eq!(cancel_execution(expired_id), CancelResult::NotFound);
        assert!(matches!(
            expired_completion.wait().await.unwrap(),
            ExecutionOutcome::Failure(crate::execution::ExecutionFailure {
                drawing_outcome: crate::execution::DrawingOutcome::NotStarted,
                ..
            })
        ));

        let (execution, output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        let admission = admit_test_execution(id, execution, output).unwrap();
        let (cancelled_id, _output, cancelled_completion) = admission.into_parts();
        assert_eq!(cancel_execution(cancelled_id), CancelResult::Accepted);
        process_due_timers(Instant::now() + EXECUTION_START_TIMEOUT);
        assert_eq!(
            cancelled_completion.wait().await.unwrap(),
            ExecutionOutcome::Cancelled
        );
        stop();
    }

    #[tokio::test]
    async fn mutation_job_capacity_bounds_disconnected_waiters() {
        let _test = TEST_LOCK.lock().await;
        reset(vec![document(1, 101, true)]);
        let id = list().unwrap()[0].id.clone();
        let active = tokio::spawn(save(id.clone()));
        tokio::task::yield_now().await;
        let action = take_native_action();
        assert_eq!(action.kind, NativeActionKind::Save);

        let mut queued = Vec::new();

        for _ in 1..MAX_MUTATION_JOBS {
            queued.push(tokio::spawn(save(id.clone())));
            tokio::task::yield_now().await;
        }

        assert_eq!(save(id).await, Err(Error::MutationCapacity));

        stop();
        complete_native_action(action.job_id, result(NativeActionResultKind::SaveFailed));
        assert!(matches!(active.await.unwrap(), Err(Error::SaveFailed(_))));

        for waiter in queued {
            assert_eq!(waiter.await.unwrap(), Err(Error::PluginStopping));
        }
    }

    fn admit_test_execution(
        id: String,
        execution: Execution,
        output: OutputStream,
    ) -> Result<ExecutionAdmission, Error> {
        let reservation = try_reserve_execution().ok_or(Error::ExecutionCapacity)?;
        admit_execution(id, execution, output, reservation)
    }

    fn spawn_test_execution(
        id: String,
        execution: Execution,
        output: OutputStream,
    ) -> (
        OutputStream,
        tokio::task::JoinHandle<Result<ExecutionOutcome, Error>>,
    ) {
        let admission = admit_test_execution(id, execution, output).unwrap();
        let (_, output, completion) = admission.into_parts();
        (output, tokio::spawn(completion.wait()))
    }

    fn result(kind: NativeActionResultKind) -> NativeActionResult {
        NativeActionResult {
            kind,
            native_status: 0,
            native_detail: String::new(),
        }
    }

    fn step_success() -> ExecutionStepResult {
        ExecutionStepResult {
            kind: crate::execution::ExecutionStepResultKind::Success,
            native_status: 0,
            lisp_errno: 0,
            detail: String::new(),
            bridge_symbols_clear_status: 0,
        }
    }

    fn document(
        document_token: usize,
        database_token: usize,
        modified: bool,
    ) -> crate::ffi::NativeDocumentSnapshot {
        crate::ffi::NativeDocumentSnapshot {
            document_token,
            database_token,
            name: "/tmp/house.dwg".into(),
            named: true,
            modified,
            read_only: false,
        }
    }

    fn reset(documents: Vec<crate::ffi::NativeDocumentSnapshot>) {
        stop();
        SCHEDULER.lock().unwrap().quarantined = false;
        replace_document_snapshot(documents);
        start();
    }
}
