use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use acadctl_rpc::{DocId, DrawingPath};
use tokio::sync::oneshot;

use crate::doc::{Doc, DocRegistry, NativeDocKey};
use crate::exec::output::{OutputSink, OutputStream};
use crate::exec::{Exec, ExecOutcome, bound_diagnostic};
use crate::ffi::{NativeActionResult, NativeActionResultKind, NativeExecFinalizationObservation};

use super::error::Error;
use super::native::{
    NativeAction, classify_execution_finalization, complete_operation,
    native_result_requires_quarantine, schedule_native_actions,
};
use super::operation::{
    HistoryDirection, Operation, OperationOutcome, Prepared, finalize, prepare,
};
use super::timer::{BUSY_RETRY_INITIAL, BUSY_RETRY_MAX, EXECUTION_START_TIMEOUT, notify_changed};

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);
static EXECUTION_RESERVATIONS: AtomicUsize = AtomicUsize::new(0);
pub(super) static SCHEDULER: LazyLock<Mutex<MutationScheduler>> =
    LazyLock::new(|| Mutex::new(MutationScheduler::new()));

pub(crate) type MutationJobId = u64;

pub const MAX_MUTATION_JOBS: usize = 32;
pub const MAX_ADMITTED_EXECUTIONS: usize = 8;
pub const MAX_ADMITTED_SOURCE_BYTES: usize = 32 * 1024 * 1024;

#[cfg(test)]
pub(crate) static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(super) struct MutationScheduler {
    pub(super) documents: DocRegistry,
    pub(super) pending: VecDeque<MutationJob>,
    pub(super) active: Option<MutationJob>,
    pub(super) wake_pending: bool,
    pub(super) stopping: bool,
    pub(super) quarantined: bool,
}

pub(super) struct MutationJob {
    pub(super) job_id: MutationJobId,
    pub(super) operation: Operation,
    pub(super) native_target: Option<NativeDocKey>,
    pub(super) start_deadline: Option<Instant>,
    pub(super) waiting_for_readiness: bool,
    pub(super) retry_at: Option<Instant>,
    pub(super) retry_delay: Duration,
    pub(super) _execution_reservation: Option<ExecReservation>,
    pub(super) completion: oneshot::Sender<Result<OperationOutcome, Error>>,
}

#[derive(Clone)]
pub struct ExecReservation {
    _inner: Arc<ExecReservationInner>,
}

struct ExecReservationInner;

impl Drop for ExecReservationInner {
    fn drop(&mut self) {
        let previous = EXECUTION_RESERVATIONS.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
    }
}

enum TakeDecision {
    Action(NativeAction),
    Complete(
        oneshot::Sender<Result<OperationOutcome, Error>>,
        Result<OperationOutcome, Error>,
        Option<OutputSink>,
    ),
}

pub struct ExecAdmission {
    job_id: MutationJobId,
    output: OutputStream,
    completion: ExecCompletion,
}

pub struct ExecCompletion {
    receiver: oneshot::Receiver<Result<OperationOutcome, Error>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelResult {
    Accepted,
    TooLate,
    NotFound,
    Unavailable,
}

impl MutationScheduler {
    const fn new() -> Self {
        Self {
            documents: DocRegistry::new(),
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

    pub(super) fn request_wake(&mut self) -> bool {
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

pub async fn open(path: DrawingPath) -> Result<Doc, Error> {
    match submit_operation(Operation::Open { path }).await? {
        OperationOutcome::Doc(document) => Ok(document),
        OperationOutcome::Closed | OperationOutcome::Exec(_) => Err(Error::OpenNotPublished),
    }
}

pub async fn save(id: DocId) -> Result<Doc, Error> {
    match submit_operation(Operation::Save { id }).await? {
        OperationOutcome::Doc(document) => Ok(document),
        OperationOutcome::Closed | OperationOutcome::Exec(_) => Err(Error::SaveNotPublished),
    }
}

pub async fn close(id: DocId, discard: bool) -> Result<(), Error> {
    match submit_operation(Operation::Close { id, discard }).await? {
        OperationOutcome::Closed => Ok(()),
        OperationOutcome::Doc(_) | OperationOutcome::Exec(_) => Err(Error::CloseNotPublished),
    }
}

pub async fn undo(id: DocId) -> Result<Doc, Error> {
    history(id, HistoryDirection::Undo).await
}

pub async fn redo(id: DocId) -> Result<Doc, Error> {
    history(id, HistoryDirection::Redo).await
}

async fn history(id: DocId, direction: HistoryDirection) -> Result<Doc, Error> {
    match submit_operation(Operation::History { id, direction }).await? {
        OperationOutcome::Doc(document) => Ok(document),
        OperationOutcome::Closed | OperationOutcome::Exec(_) => Err(Error::DocGone),
    }
}

pub fn admit_execution(
    id: DocId,
    execution: Exec,
    output: OutputStream,
    reservation: ExecReservation,
) -> Result<ExecAdmission, Error> {
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
                Prepared::Native(_) => Err(Error::ExecNotFinished),
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
                return Err(Error::ExecCapacity);
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

    notify_changed();

    Ok(ExecAdmission {
        job_id,
        output,
        completion: ExecCompletion { receiver },
    })
}

pub fn try_reserve_execution() -> Option<ExecReservation> {
    EXECUTION_RESERVATIONS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current < MAX_ADMITTED_EXECUTIONS).then_some(current + 1)
        })
        .ok()
        .map(|_| ExecReservation {
            _inner: Arc::new(ExecReservationInner),
        })
}

impl ExecAdmission {
    pub fn into_parts(self) -> (MutationJobId, OutputStream, ExecCompletion) {
        (self.job_id, self.output, self.completion)
    }
}

impl ExecCompletion {
    pub async fn wait(self) -> Result<ExecOutcome, Error> {
        match self.receiver.await.map_err(|_| Error::Stopped)?? {
            OperationOutcome::Exec(outcome) => Ok(outcome),
            OperationOutcome::Doc(_) | OperationOutcome::Closed => Err(Error::ExecNotFinished),
        }
    }
}

pub fn list() -> Result<Vec<Doc>, Error> {
    SCHEDULER
        .lock()
        .map_err(|_| Error::SchedulerStateUnavailable)
        .map(|scheduler| scheduler.documents.list())
}

pub fn replace_document_snapshot(documents: Vec<crate::ffi::NativeDocSnapshot>) {
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

    notify_changed();
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
            return NativeAction::idle();
        };

        scheduler.wake_pending = false;

        if scheduler.stopping || scheduler.quarantined || scheduler.active.is_some() {
            return NativeAction::idle();
        }

        let Some(mut job) = scheduler.pending.pop_front() else {
            return NativeAction::idle();
        };

        if job.waiting_for_readiness {
            scheduler.pending.push_front(job);

            return NativeAction::idle();
        }

        job.expire_if_due(Instant::now());

        match prepare(&job.operation, &scheduler.documents) {
            Prepared::Immediate(outcome) => {
                TakeDecision::Complete(job.completion, outcome, job.operation.output_sink())
            }
            Prepared::Native(command) => {
                job.native_target = command.target();
                let action = NativeAction::issue(job.job_id, command);
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
            NativeAction::idle()
        }
    }
}

pub fn complete_native_action(job_id: MutationJobId, result: NativeActionResult) {
    let quarantine = native_result_requires_quarantine(result.kind);
    complete_native_action_with_quarantine(job_id, result, quarantine);
}

fn complete_native_action_with_quarantine(
    job_id: MutationJobId,
    mut result: NativeActionResult,
    quarantine: bool,
) {
    bound_diagnostic(&mut result.native_detail);

    let ((completion, outcome, output), rejected) = {
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
                notify_changed();

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

        let rejected = if quarantine {
            scheduler.quarantined = true;
            scheduler.wake_pending = false;
            std::mem::take(&mut scheduler.pending)
                .into_iter()
                .map(|job| (job.completion, job.operation.output_sink()))
                .collect()
        } else {
            Vec::new()
        };

        ((job.completion, outcome, output), rejected)
    };

    if let Some(output) = output {
        output.finish();
    }

    let _ = completion.send(outcome);

    for (completion, output) in rejected {
        if let Some(output) = output {
            output.stop();
        }

        let _ = completion.send(Err(Error::NativeMutationStateUnknown));
    }
}

pub(crate) fn complete_execution_native_action(
    job_id: MutationJobId,
    result: NativeActionResult,
    observation: NativeExecFinalizationObservation,
) {
    let decision = classify_execution_finalization(result, observation);
    complete_native_action_with_quarantine(job_id, decision.result, decision.quarantine);
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
            let _ = completion.send(Ok(OperationOutcome::Exec(ExecOutcome::Cancelled)));

            if should_wake {
                schedule_native_actions();
            }

            notify_changed();
            CancelResult::Accepted
        }
        Action::Result(result) => result,
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

impl MutationJob {
    pub(super) fn deadline_is_due(&self, now: Instant) -> bool {
        self.start_deadline.is_some_and(|deadline| now >= deadline)
            && self.execution_start_pending()
    }

    pub(super) fn expire_if_due(&mut self, now: Instant) -> bool {
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

    pub(super) fn execution_start_pending(&self) -> bool {
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

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
