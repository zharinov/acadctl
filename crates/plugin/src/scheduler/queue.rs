use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use acadctl_rpc::{DocId, DrawingPath};
use tokio::sync::oneshot;

use crate::doc::{Doc, DocRegistry, NativeDocKey};
use crate::exec::output::{OutputSink, OutputStream};
use crate::exec::{
    Exec, ExecOutcome, ExecStepResult, NativeExecStep, ValueOutputLease, bound_diagnostic,
};
use crate::ffi::{NativeActionResult, NativeActionResultKind, NativeExecFinalizationObservation};

use super::error::Error;
use super::native::{
    NativeAction, classify_execution_finalization, native_result_requires_quarantine,
    schedule_native_actions,
};
use super::operation::{HistoryDirection, Operation, OperationOutcome, Prepared};
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

pub(super) type Completion = (
    oneshot::Sender<Result<OperationOutcome, Error>>,
    Result<OperationOutcome, Error>,
    Option<OutputSink>,
);
type Admission = (
    MutationJobId,
    oneshot::Receiver<Result<OperationOutcome, Error>>,
    bool,
    Option<Completion>,
);
type StoppedJob = (
    oneshot::Sender<Result<OperationOutcome, Error>>,
    Option<OutputSink>,
);

enum SubmissionDecision {
    Immediate(Result<OperationOutcome, Error>),
    Queued {
        receiver: oneshot::Receiver<Result<OperationOutcome, Error>>,
        should_wake: bool,
    },
}

enum TakeDecision {
    Action(NativeAction),
    Complete(Completion),
    Idle,
}

enum CancelDecision {
    Active(OutputSink),
    Pending {
        output: OutputSink,
        completion: oneshot::Sender<Result<OperationOutcome, Error>>,
        should_wake: bool,
    },
    Result(CancelResult),
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

    fn list(&self) -> Vec<Doc> {
        self.documents.list()
    }

    fn replace_document_snapshot(&mut self, documents: Vec<crate::ffi::NativeDocSnapshot>) {
        self.documents.replace_snapshot(documents);
    }

    fn start(&mut self) {
        self.stopping = false;
    }

    fn execution_usage(&self) -> (usize, usize) {
        self.pending
            .iter()
            .chain(self.active.iter())
            .filter_map(|job| job.operation.execution_source_bytes())
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

    pub(super) fn take_execution_step(
        &mut self,
        job_id: MutationJobId,
        now: Instant,
    ) -> NativeExecStep {
        let Some(job) = self.active.as_mut().filter(|job| job.job_id == job_id) else {
            return NativeExecStep::invalid();
        };

        job.expire_if_due(now);
        job.operation.take_execution_step()
    }

    pub(super) fn acquire_eval_value_output(
        &self,
        job_id: MutationJobId,
        target: NativeDocKey,
    ) -> Option<ValueOutputLease> {
        self.active
            .as_ref()
            .filter(|job| job.job_id == job_id && job.native_target == Some(target))?
            .operation
            .acquire_eval_value_output()
    }

    pub(super) fn complete_execution_step(
        &mut self,
        job_id: MutationJobId,
        result: ExecStepResult,
    ) -> bool {
        self.active
            .as_mut()
            .filter(|job| job.job_id == job_id)
            .is_some_and(|job| job.operation.complete_execution_step(result))
    }

    pub(super) fn abandon_execution(
        &mut self,
        job_id: MutationJobId,
        result: ExecStepResult,
    ) -> bool {
        self.active
            .as_mut()
            .filter(|job| job.job_id == job_id)
            .is_some_and(|job| job.operation.abandon_execution(result))
    }

    pub(super) fn next_timer_deadline(&self) -> Option<Instant> {
        let execution_start = self
            .pending
            .iter()
            .chain(self.active.iter())
            .filter(|job| job.execution_start_pending())
            .filter_map(|job| job.start_deadline)
            .min();

        let retry = self
            .pending
            .front()
            .filter(|job| job.waiting_for_readiness)
            .and_then(|job| job.retry_at);
        execution_start.into_iter().chain(retry).min()
    }

    pub(super) fn process_due_timers(&mut self, now: Instant) -> (Vec<Completion>, bool) {
        if let Some(active) = self.active.as_mut() {
            active.expire_if_due(now);
        }

        let mut expired = Vec::new();
        let mut index = 0;

        while index < self.pending.len() {
            if !self.pending[index].deadline_is_due(now) {
                index += 1;
                continue;
            }

            let mut job = self.pending.remove(index).expect("queued job exists");

            if job.expire_if_due(now) {
                let output = job.operation.output_sink();
                let outcome = job.operation.complete(&self.documents, None);

                expired.push((job.completion, outcome, output));
            } else {
                self.pending.insert(index, job);
                index += 1;
            }
        }

        if let Some(head) = self.pending.front_mut()
            && head.waiting_for_readiness
            && head.retry_at.is_some_and(|retry_at| now >= retry_at)
        {
            head.waiting_for_readiness = false;
            head.retry_at = None;
        }

        (expired, self.request_wake())
    }

    pub(super) fn native_state_may_be_ready(&mut self) -> bool {
        if let Some(job) = self
            .pending
            .front_mut()
            .filter(|job| job.waiting_for_readiness)
        {
            job.waiting_for_readiness = false;
            job.retry_at = None;
            job.retry_delay = BUSY_RETRY_INITIAL;
        }

        self.request_wake()
    }

    fn stop(&mut self) -> (Option<OutputSink>, Vec<StoppedJob>) {
        self.stopping = true;
        self.wake_pending = false;

        let active_output = self
            .active
            .as_mut()
            .and_then(|job| job.operation.request_cancel().map(|(_, output)| output));
        let pending = std::mem::take(&mut self.pending)
            .into_iter()
            .map(|job| (job.completion, job.operation.output_sink()))
            .collect();

        (active_output, pending)
    }

    fn admit_execution(
        &mut self,
        id: DocId,
        execution: Exec,
        reservation: ExecReservation,
        now: Instant,
    ) -> Result<Admission, Error> {
        if self.stopping {
            return Err(Error::PluginStopping);
        }

        if self.quarantined {
            return Err(Error::NativeMutationStateUnknown);
        }

        let job_id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
        let (completion, receiver) = oneshot::channel();

        if execution.outcome().is_some() {
            let operation = Operation::Execute {
                id,
                execution: Box::new(execution),
            };

            let outcome = match operation.prepare(&self.documents) {
                Prepared::Immediate(outcome) => outcome,
                Prepared::Native(_) => Err(Error::ExecNotFinished),
            };

            return Ok((
                job_id,
                receiver,
                false,
                Some((completion, outcome, operation.output_sink())),
            ));
        }

        if self.mutation_job_count() >= MAX_MUTATION_JOBS {
            return Err(Error::MutationCapacity);
        }

        let (execution_count, source_bytes) = self.execution_usage();

        if execution_count >= MAX_ADMITTED_EXECUTIONS
            || source_bytes.saturating_add(execution.source_bytes()) > MAX_ADMITTED_SOURCE_BYTES
        {
            return Err(Error::ExecCapacity);
        }

        self.pending.push_back(MutationJob {
            job_id,
            operation: Operation::Execute {
                id,
                execution: Box::new(execution),
            },
            native_target: None,
            start_deadline: Some(now + EXECUTION_START_TIMEOUT),
            waiting_for_readiness: false,
            retry_at: None,
            retry_delay: BUSY_RETRY_INITIAL,
            _execution_reservation: Some(reservation),
            completion,
        });

        Ok((job_id, receiver, self.request_wake(), None))
    }

    fn submit_operation(&mut self, operation: Operation) -> Result<SubmissionDecision, Error> {
        if self.stopping {
            return Err(Error::PluginStopping);
        }

        if self.quarantined {
            return Err(Error::NativeMutationStateUnknown);
        }

        if self.mutation_job_count() >= MAX_MUTATION_JOBS {
            return Err(Error::MutationCapacity);
        }

        if self.idle()
            && let Prepared::Immediate(outcome) = operation.prepare(&self.documents)
        {
            return Ok(SubmissionDecision::Immediate(outcome));
        }

        let job_id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
        let (completion, receiver) = oneshot::channel();

        self.pending.push_back(MutationJob {
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

        Ok(SubmissionDecision::Queued {
            receiver,
            should_wake: self.request_wake(),
        })
    }

    fn take_native_action(&mut self, now: Instant) -> TakeDecision {
        self.wake_pending = false;

        if self.stopping || self.quarantined || self.active.is_some() {
            return TakeDecision::Idle;
        }

        let Some(mut job) = self.pending.pop_front() else {
            return TakeDecision::Idle;
        };

        if job.waiting_for_readiness {
            self.pending.push_front(job);

            return TakeDecision::Idle;
        }

        job.expire_if_due(now);

        match job.operation.prepare(&self.documents) {
            Prepared::Immediate(outcome) => {
                TakeDecision::Complete((job.completion, outcome, job.operation.output_sink()))
            }
            Prepared::Native(command) => {
                job.native_target = command.target();
                let action = NativeAction::issue(job.job_id, command);
                self.active = Some(job);
                TakeDecision::Action(action)
            }
        }
    }

    fn complete_native_action(
        &mut self,
        job_id: MutationJobId,
        result: NativeActionResult,
        quarantine: bool,
        now: Instant,
    ) -> (Option<Completion>, Vec<StoppedJob>, bool) {
        let Some(mut job) = self.active.take() else {
            return (None, Vec::new(), false);
        };

        if job.job_id != job_id {
            self.active = Some(job);
            return (None, Vec::new(), false);
        }

        let settled_before_start = if result.kind == NativeActionResultKind::NotQuiescent
            && job.execution_has_not_handed_off_form()
        {
            if job.finish_cancel_before_start()
                || job.expire_if_due(now)
                || job.execution_has_outcome()
            {
                true
            } else {
                job.native_target = None;
                job.defer_for_readiness(now);
                self.pending.push_front(job);
                return (None, Vec::new(), true);
            }
        } else {
            false
        };

        let output = job.operation.output_sink();
        let native_target = job.native_target;
        let outcome = if settled_before_start {
            job.operation.complete(&self.documents, native_target)
        } else {
            job.operation
                .complete_native(result, &self.documents, native_target)
        };

        let rejected = if quarantine {
            self.quarantined = true;
            self.wake_pending = false;
            std::mem::take(&mut self.pending)
                .into_iter()
                .map(|job| (job.completion, job.operation.output_sink()))
                .collect()
        } else {
            Vec::new()
        };

        (Some((job.completion, outcome, output)), rejected, false)
    }

    fn cancel_execution(&mut self, job_id: MutationJobId) -> CancelDecision {
        if let Some(job) = self.active.as_mut().filter(|job| job.job_id == job_id) {
            return match job.operation.request_cancel() {
                Some((true, output)) => CancelDecision::Active(output),
                Some((false, _)) => CancelDecision::Result(CancelResult::TooLate),
                None => CancelDecision::Result(CancelResult::NotFound),
            };
        }

        let Some(index) = self.pending.iter().position(|job| {
            job.job_id == job_id && matches!(job.operation, Operation::Execute { .. })
        }) else {
            return CancelDecision::Result(CancelResult::NotFound);
        };

        let Some(output) = self.pending[index].operation.cancel_before_start() else {
            return CancelDecision::Result(CancelResult::TooLate);
        };

        let job = self.pending.remove(index).expect("queued job exists");

        CancelDecision::Pending {
            output,
            completion: job.completion,
            should_wake: self.request_wake(),
        }
    }

    fn wake_failed(&mut self) -> Vec<StoppedJob> {
        self.wake_pending = false;
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(|job| (job.completion, job.operation.output_sink()))
            .collect()
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
    let (job_id, receiver, should_wake, immediate) = SCHEDULER
        .lock()
        .map_err(|_| Error::SchedulerStateUnavailable)?
        .admit_execution(id, execution, reservation, Instant::now())?;

    if let Some(completion) = immediate {
        finish_completion(completion);
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
        .map(|scheduler| scheduler.list())
}

pub fn replace_document_snapshot(documents: Vec<crate::ffi::NativeDocSnapshot>) {
    if let Ok(mut scheduler) = SCHEDULER.lock() {
        scheduler.replace_document_snapshot(documents);
    }
}

pub fn start() {
    if let Ok(mut scheduler) = SCHEDULER.lock() {
        scheduler.start();
    }
}

pub fn stop() {
    let stopped = SCHEDULER.lock().ok().map(|mut scheduler| scheduler.stop());

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
    let decision = SCHEDULER
        .lock()
        .map_err(|_| Error::SchedulerStateUnavailable)?
        .submit_operation(operation)?;

    match decision {
        SubmissionDecision::Immediate(outcome) => outcome,
        SubmissionDecision::Queued {
            receiver,
            should_wake,
        } => {
            if should_wake {
                schedule_native_actions();
            }
            receiver.await.map_err(|_| Error::Stopped)?
        }
    }
}

pub fn take_native_action() -> NativeAction {
    let decision = SCHEDULER
        .lock()
        .ok()
        .map_or(TakeDecision::Idle, |mut scheduler| {
            scheduler.take_native_action(Instant::now())
        });

    match decision {
        TakeDecision::Action(action) => action,
        TakeDecision::Complete(completion) => {
            finish_completion(completion);
            NativeAction::idle()
        }
        TakeDecision::Idle => NativeAction::idle(),
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

    let Some((completion, rejected, notify)) = SCHEDULER.lock().ok().map(|mut scheduler| {
        scheduler.complete_native_action(job_id, result, quarantine, Instant::now())
    }) else {
        return;
    };

    if let Some(completion) = completion {
        finish_completion(completion);
    }

    for (completion, output) in rejected {
        if let Some(output) = output {
            output.stop();
        }

        let _ = completion.send(Err(Error::NativeMutationStateUnknown));
    }

    if notify {
        notify_changed();
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
    let Some(decision) = SCHEDULER
        .lock()
        .ok()
        .map(|mut scheduler| scheduler.cancel_execution(job_id))
    else {
        return CancelResult::Unavailable;
    };

    match decision {
        CancelDecision::Active(output) => {
            output.request_cancel();
            CancelResult::Accepted
        }
        CancelDecision::Pending {
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
        CancelDecision::Result(result) => result,
    }
}

pub fn try_claim_native_action_wake() -> bool {
    SCHEDULER
        .lock()
        .is_ok_and(|mut scheduler| scheduler.request_wake())
}

pub fn wake_failed(status: i32) {
    let Some(completions) = SCHEDULER
        .lock()
        .ok()
        .map(|mut scheduler| scheduler.wake_failed())
    else {
        return;
    };

    for (completion, output) in completions {
        if let Some(output) = output {
            output.stop();
        }

        let _ = completion.send(Err(Error::ScheduleFailed(status)));
    }
}

fn finish_completion((completion, outcome, output): Completion) {
    if let Some(output) = output {
        output.finish();
    }

    let _ = completion.send(outcome);
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

        self.operation.expire_before_start(format!(
            "execution did not start within {} seconds",
            EXECUTION_START_TIMEOUT.as_secs()
        ))
    }

    fn execution_has_not_handed_off_form(&self) -> bool {
        self.operation.execution_has_not_handed_off_form()
    }

    pub(super) fn execution_start_pending(&self) -> bool {
        self.operation.execution_start_pending()
    }

    fn execution_has_outcome(&self) -> bool {
        self.operation.execution_has_outcome()
    }

    fn finish_cancel_before_start(&mut self) -> bool {
        self.operation.finish_cancel_before_start()
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
