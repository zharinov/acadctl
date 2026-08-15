use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use acadctl_rpc::{DocId, DrawingPath};
use tokio::sync::{Notify, oneshot};

use crate::doc::{Doc, DocRegistry, NativeDocKey};
use crate::exec::output::{OutputSink, OutputStream};
use crate::exec::value::writer::NativeValueWriter;
use crate::exec::{Exec, ExecOutcome, ExecStepResult, NativeExecStep, bound_diagnostic};
use crate::ffi::{
    NativeAction, NativeActionKind, NativeActionResult, NativeActionResultKind,
    NativeExecFinalizationObservation,
};

use super::error::Error;
use super::operation::{
    HistoryDirection, Operation, OperationOutcome, Prepared, complete_operation, finalize,
    native_action, prepare,
};

#[cfg(test)]
type ExecFinalizationObservation = NativeExecFinalizationObservation;

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

impl NativeExecFinalizationObservation {
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
    documents: DocRegistry,
    pending: VecDeque<MutationJob>,
    active: Option<MutationJob>,
    wake_pending: bool,
    stopping: bool,
    quarantined: bool,
}

struct MutationJob {
    job_id: MutationJobId,
    operation: Operation,
    native_target: Option<NativeDocKey>,
    start_deadline: Option<Instant>,
    waiting_for_readiness: bool,
    retry_at: Option<Instant>,
    retry_delay: Duration,
    _execution_reservation: Option<ExecReservation>,
    completion: oneshot::Sender<Result<OperationOutcome, Error>>,
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

    TIMERS_CHANGED.notify_one();

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
                    .then_some(NativeDocKey {
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
        NativeActionResultKind::DocContextRestoreFailed
            | NativeActionResultKind::ExecBridgeFinalizationFailed
            | NativeActionResultKind::ExecBridgeSymbolsClearFailed
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
    observation: NativeExecFinalizationObservation,
) {
    let decision = classify_execution_finalization(result, observation);
    complete_native_action_with_quarantine(job_id, decision.result, decision.quarantine);
}

struct ExecFinalizationDecision {
    result: NativeActionResult,
    quarantine: bool,
}

fn classify_execution_finalization(
    mut result: NativeActionResult,
    observation: NativeExecFinalizationObservation,
) -> ExecFinalizationDecision {
    let native_state_unproved = observation.native_state_unproved();
    let quarantine = native_state_unproved || native_result_requires_quarantine(result.kind);
    let preserve_symbol_failure = result.kind
        == NativeActionResultKind::ExecBridgeSymbolsClearFailed
        && observation.only_symbol_cleanup_unproved();

    if native_state_unproved
        && result.kind != NativeActionResultKind::DocContextRestoreFailed
        && !preserve_symbol_failure
    {
        result.kind = NativeActionResultKind::ExecBridgeFinalizationFailed;
    }

    ExecFinalizationDecision { result, quarantine }
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

pub fn take_execution_step(job_id: MutationJobId) -> NativeExecStep {
    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return NativeExecStep::invalid();
    };

    let Some(job) = scheduler.active.as_mut() else {
        return NativeExecStep::invalid();
    };

    if job.job_id != job_id {
        return NativeExecStep::invalid();
    }

    job.expire_if_due(Instant::now());

    match &mut job.operation {
        Operation::Execute { execution, .. } => execution.take_step(),
        Operation::Open { .. }
        | Operation::Save { .. }
        | Operation::Close { .. }
        | Operation::History { .. } => NativeExecStep::invalid(),
    }
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
                != Some(NativeDocKey {
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

pub fn complete_execution_step(job_id: MutationJobId, mut result: ExecStepResult) -> bool {
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

pub fn abandon_execution(job_id: MutationJobId, mut result: ExecStepResult) -> bool {
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

fn empty_action() -> NativeAction {
    native_action(NativeActionKind::None, None, String::new(), false)
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
#[path = "tests.rs"]
mod tests;
