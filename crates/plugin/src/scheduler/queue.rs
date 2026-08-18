use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use acadctl_rpc::{DrawingId, DrawingPath};
use tokio::sync::oneshot;

use crate::drawing::{Drawing, DrawingRegistry, NativeDocumentKey};
use crate::exec::output::{OutputSink, OutputStream};
use crate::exec::{
    Exec, ExecOutcome, ExecStepResult, NativeExecStep, ValueOutputLease, bound_diagnostic,
};
use crate::ffi::{
    NativeActionResult, NativeActionResultKind, NativeCaptureResult, NativeCaptureResultKind,
    NativeExecFinalizationObservation,
};

use super::error::Error;
use super::native::{
    NativeAction, ViewportCapture, classify_execution_finalization,
    native_result_requires_quarantine, schedule_native_actions,
};
use super::operation::{
    DocumentContextPolicy, HistoryDirection, Operation, OperationOutcome, Prepared,
};
use super::timer::{
    READINESS_RETRY_INITIAL, READINESS_RETRY_MAX, READINESS_TIMEOUT, notify_changed,
};

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
    drawings: DrawingRegistry,
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
    wait: WaitState,
    _execution_reservation: Option<ExecReservation>,
    completion: oneshot::Sender<Result<OperationOutcome, Error>>,
}

enum WaitState {
    Queued {
        deadline: Instant,
    },
    Ready {
        deadline: Instant,
        retry_delay: Duration,
    },
    Deferred {
        deadline: Instant,
        retry_at: Instant,
        retry_delay: Duration,
    },
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
            drawings: DrawingRegistry::new(),
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

    fn list(&self) -> Vec<Drawing> {
        self.drawings.list()
    }

    fn replace_drawing_snapshot(&mut self, drawings: Vec<crate::ffi::NativeDocumentSnapshot>) {
        self.drawings.replace_snapshot(drawings);
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
            || self.pending.front().is_some_and(MutationJob::is_deferred)
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

        job.expire_active_execution_if_due(now);
        job.operation.take_execution_step()
    }

    pub(super) fn acquire_eval_value_output(
        &self,
        job_id: MutationJobId,
        target: NativeDocumentKey,
    ) -> Option<ValueOutputLease> {
        self.active
            .as_ref()
            .filter(|job| job.job_id == job_id && job.native_target == Some(target))?
            .operation
            .acquire_eval_value_output()
    }

    pub(super) fn acquire_form_output(
        &self,
        job_id: MutationJobId,
        target: NativeDocumentKey,
    ) -> Option<ValueOutputLease> {
        self.active
            .as_ref()
            .filter(|job| job.job_id == job_id && job.native_target == Some(target))?
            .operation
            .acquire_form_output()
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
        let active_execution = self
            .active
            .iter()
            .filter(|job| job.operation.execution_readiness_wait_pending())
            .map(MutationJob::wait_deadline);
        let pending = self.pending.iter().map(MutationJob::wait_deadline);

        let retry = self.pending.front().and_then(MutationJob::retry_at);
        active_execution.chain(pending).chain(retry).min()
    }

    pub(super) fn process_due_timers(&mut self, now: Instant) -> (Vec<Completion>, bool) {
        if let Some(active) = self.active.as_mut() {
            active.expire_active_execution_if_due(now);
        }

        let mut expired = Vec::new();
        let mut index = 0;

        while index < self.pending.len() {
            if !self.pending[index].wait_is_due(now) {
                index += 1;
                continue;
            }

            let mut job = self.pending.remove(index).expect("queued job exists");

            let output = job.operation.output_sink();
            let outcome = job.wait_timeout_outcome(&self.drawings, None);
            expired.push((job.completion, outcome, output));
        }

        if let Some(head) = self.pending.front_mut()
            && head.retry_is_due(now)
        {
            head.retry_now();
        }

        (expired, self.request_wake())
    }

    pub(super) fn native_state_may_be_ready(&mut self) -> bool {
        if let Some(job) = self.pending.front_mut().filter(|job| job.is_deferred()) {
            job.readiness_may_have_changed();
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
        id: DrawingId,
        execution: Exec,
        context: DocumentContextPolicy,
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
                context,
            };

            let outcome = match operation.prepare(&self.drawings) {
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
                context,
            },
            native_target: None,
            wait: WaitState::queued(now),
            _execution_reservation: Some(reservation),
            completion,
        });

        Ok((job_id, receiver, self.request_wake(), None))
    }

    fn submit_operation(
        &mut self,
        operation: Operation,
        now: Instant,
    ) -> Result<SubmissionDecision, Error> {
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
            && let Prepared::Immediate(outcome) = operation.prepare(&self.drawings)
        {
            return Ok(SubmissionDecision::Immediate(outcome));
        }

        let job_id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
        let (completion, receiver) = oneshot::channel();

        self.pending.push_back(MutationJob {
            job_id,
            operation,
            native_target: None,
            wait: WaitState::queued(now),
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

        if job.is_deferred() {
            self.pending.push_front(job);

            return TakeDecision::Idle;
        }

        if job.wait_is_due(now) {
            let output = job.operation.output_sink();
            let outcome = job.wait_timeout_outcome(&self.drawings, None);
            return TakeDecision::Complete((job.completion, outcome, output));
        }

        job.begin_readiness(now);

        match job.operation.prepare(&self.drawings) {
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

        let waited_outcome = if result.kind == NativeActionResultKind::NotQuiescent
            && job.operation.can_wait_for_readiness()
        {
            if job.finish_cancel_before_start() || job.execution_has_outcome() {
                Some(job.operation.complete(&self.drawings, job.native_target))
            } else if job.wait_is_due(now) {
                Some(job.wait_timeout_outcome(&self.drawings, job.native_target))
            } else {
                job.native_target = None;
                job.defer_for_readiness(now);
                self.pending.push_front(job);
                return (None, Vec::new(), true);
            }
        } else {
            None
        };

        let output = job.operation.output_sink();
        let native_target = job.native_target;
        let outcome = waited_outcome.unwrap_or_else(|| {
            job.operation
                .complete_native(result, &self.drawings, native_target)
        });

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

    fn wake_failed(&mut self, now: Instant) -> (Option<Completion>, bool) {
        self.wake_pending = false;

        if self.stopping || self.quarantined || self.active.is_some() {
            return (None, false);
        }

        let Some(mut job) = self.pending.pop_front() else {
            return (None, false);
        };

        if job.wait_is_due(now) {
            let output = job.operation.output_sink();
            let outcome = job.wait_timeout_outcome(&self.drawings, None);
            let should_wake = self.request_wake();

            return (Some((job.completion, outcome, output)), should_wake);
        }

        job.begin_readiness(now);
        job.defer_for_readiness(now);
        self.pending.push_front(job);
        (None, false)
    }
}

pub async fn open(path: DrawingPath) -> Result<Drawing, Error> {
    match submit_operation(Operation::Open { path }).await? {
        OperationOutcome::Drawing(drawing) => Ok(drawing),
        OperationOutcome::Closed | OperationOutcome::Capture(_) | OperationOutcome::Exec(_) => {
            Err(Error::OpenNotPublished)
        }
    }
}

pub async fn save(id: DrawingId, path: Option<acadctl_rpc::SavePath>) -> Result<Drawing, Error> {
    match submit_operation(Operation::Save { id, path }).await? {
        OperationOutcome::Drawing(drawing) => Ok(drawing),
        OperationOutcome::Closed | OperationOutcome::Capture(_) | OperationOutcome::Exec(_) => {
            Err(Error::SaveNotPublished)
        }
    }
}

pub async fn switch(id: DrawingId) -> Result<Drawing, Error> {
    match submit_operation(Operation::Switch { id }).await? {
        OperationOutcome::Drawing(drawing) => Ok(drawing),
        OperationOutcome::Closed | OperationOutcome::Capture(_) | OperationOutcome::Exec(_) => {
            Err(Error::SwitchNotPublished)
        }
    }
}

pub async fn close(id: DrawingId, discard: bool) -> Result<(), Error> {
    match submit_operation(Operation::Close { id, discard }).await? {
        OperationOutcome::Closed => Ok(()),
        OperationOutcome::Drawing(_) | OperationOutcome::Capture(_) | OperationOutcome::Exec(_) => {
            Err(Error::CloseNotPublished)
        }
    }
}

pub async fn capture(id: DrawingId) -> Result<ViewportCapture, Error> {
    match submit_operation(Operation::Capture { id, capture: None }).await? {
        OperationOutcome::Capture(capture) => Ok(capture),
        OperationOutcome::Drawing(_) | OperationOutcome::Closed | OperationOutcome::Exec(_) => Err(
            Error::CaptureInvalid("capture operation completed without a frame".into()),
        ),
    }
}

pub async fn undo(id: DrawingId, force: bool) -> Result<Drawing, Error> {
    history(id, HistoryDirection::Undo, force).await
}

pub async fn redo(id: DrawingId, force: bool) -> Result<Drawing, Error> {
    history(id, HistoryDirection::Redo, force).await
}

async fn history(
    id: DrawingId,
    direction: HistoryDirection,
    force: bool,
) -> Result<Drawing, Error> {
    let context = if force {
        DocumentContextPolicy::ForceTemporary
    } else {
        DocumentContextPolicy::RequireActive
    };
    match submit_operation(Operation::History {
        id,
        direction,
        context,
    })
    .await?
    {
        OperationOutcome::Drawing(drawing) => Ok(drawing),
        OperationOutcome::Closed | OperationOutcome::Capture(_) | OperationOutcome::Exec(_) => {
            Err(Error::DrawingGone)
        }
    }
}

pub fn admit_execution(
    id: DrawingId,
    execution: Exec,
    output: OutputStream,
    force: bool,
    reservation: ExecReservation,
) -> Result<ExecAdmission, Error> {
    let context = if force {
        DocumentContextPolicy::ForceTemporary
    } else {
        DocumentContextPolicy::RequireActive
    };
    let (job_id, receiver, should_wake, immediate) = SCHEDULER
        .lock()
        .map_err(|_| Error::SchedulerStateUnavailable)?
        .admit_execution(id, execution, context, reservation, Instant::now())?;

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
            OperationOutcome::Drawing(_)
            | OperationOutcome::Closed
            | OperationOutcome::Capture(_) => Err(Error::ExecNotFinished),
        }
    }
}

pub fn list() -> Result<Vec<Drawing>, Error> {
    SCHEDULER
        .lock()
        .map_err(|_| Error::SchedulerStateUnavailable)
        .map(|scheduler| scheduler.list())
}

pub fn replace_drawing_snapshot(drawings: Vec<crate::ffi::NativeDocumentSnapshot>) {
    if let Ok(mut scheduler) = SCHEDULER.lock() {
        scheduler.replace_drawing_snapshot(drawings);
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
        .submit_operation(operation, Instant::now())?;

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

pub fn complete_native_capture(
    job_id: MutationJobId,
    mut result: NativeCaptureResult,
    pixels: &[u8],
) {
    bound_diagnostic(&mut result.detail);

    let mut action_result = NativeActionResult {
        kind: match result.kind {
            NativeCaptureResultKind::Success => NativeActionResultKind::Success,
            NativeCaptureResultKind::DrawingGone => NativeActionResultKind::DrawingGone,
            NativeCaptureResultKind::DrawingGenerationChanged => {
                NativeActionResultKind::DrawingGenerationChanged
            }
            NativeCaptureResultKind::NotActive => NativeActionResultKind::NotActive,
            NativeCaptureResultKind::NotQuiescent => NativeActionResultKind::NotQuiescent,
            NativeCaptureResultKind::Unavailable => NativeActionResultKind::CaptureUnavailable,
            NativeCaptureResultKind::Invalid => NativeActionResultKind::CaptureInvalid,
            kind => {
                result.detail = format!("unknown native capture result ({})", kind.repr);
                NativeActionResultKind::CaptureInvalid
            }
        },
        native_status: 0,
        native_detail: result.detail.clone(),
    };

    if result.kind == NativeCaptureResultKind::Success {
        let recorded = SCHEDULER.lock().ok().and_then(|mut scheduler| {
            let job = scheduler
                .active
                .as_mut()
                .filter(|job| job.job_id == job_id)?;
            Some(job.operation.record_native_capture(&result, pixels))
        });

        if let Some(Err(error)) = recorded {
            action_result.kind = NativeActionResultKind::CaptureInvalid;
            action_result.native_detail = error.to_string();
        }
    }

    complete_native_action(job_id, action_result);
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

pub fn wake_failed() {
    let Some((completion, should_wake)) = SCHEDULER
        .lock()
        .ok()
        .map(|mut scheduler| scheduler.wake_failed(Instant::now()))
    else {
        return;
    };

    if let Some(completion) = completion {
        finish_completion(completion);
    }

    if should_wake {
        schedule_native_actions();
    }

    notify_changed();
}

fn finish_completion((completion, outcome, output): Completion) {
    if let Some(output) = output {
        output.finish();
    }

    let _ = completion.send(outcome);
}

impl MutationJob {
    fn wait_deadline(&self) -> Instant {
        self.wait.deadline()
    }

    fn wait_is_due(&self, now: Instant) -> bool {
        now >= self.wait_deadline()
    }

    fn expire_active_execution_if_due(&mut self, now: Instant) -> bool {
        if !self.wait_is_due(now) || !self.operation.execution_readiness_wait_pending() {
            return false;
        }

        self.operation.expire_before_start(format!(
            "AutoCAD did not become ready within {} seconds",
            READINESS_TIMEOUT.as_secs()
        ))
    }

    fn wait_timeout_outcome(
        &mut self,
        drawings: &DrawingRegistry,
        native_target: Option<NativeDocumentKey>,
    ) -> Result<OperationOutcome, Error> {
        if self.operation.is_execution() {
            let _ = self.operation.expire_before_start(format!(
                "AutoCAD did not become ready within {} seconds",
                READINESS_TIMEOUT.as_secs()
            ));

            self.operation.complete(drawings, native_target)
        } else {
            Err(Error::ReadinessTimedOut(self.operation.drawing_id()))
        }
    }

    fn begin_readiness(&mut self, now: Instant) {
        self.wait.begin_readiness(now);
    }

    fn is_deferred(&self) -> bool {
        matches!(self.wait, WaitState::Deferred { .. })
    }

    fn retry_at(&self) -> Option<Instant> {
        self.wait.retry_at()
    }

    fn retry_is_due(&self, now: Instant) -> bool {
        self.retry_at().is_some_and(|retry_at| now >= retry_at)
    }

    fn retry_now(&mut self) {
        self.wait.retry_now();
    }

    fn readiness_may_have_changed(&mut self) {
        self.wait.readiness_may_have_changed();
    }

    fn execution_has_outcome(&self) -> bool {
        self.operation.execution_has_outcome()
    }

    fn finish_cancel_before_start(&mut self) -> bool {
        self.operation.finish_cancel_before_start()
    }

    fn defer_for_readiness(&mut self, now: Instant) {
        self.wait.defer(now);
    }
}

impl WaitState {
    fn queued(now: Instant) -> Self {
        Self::Queued {
            deadline: now + READINESS_TIMEOUT,
        }
    }

    fn deadline(&self) -> Instant {
        match self {
            Self::Queued { deadline }
            | Self::Ready { deadline, .. }
            | Self::Deferred { deadline, .. } => *deadline,
        }
    }

    fn begin_readiness(&mut self, now: Instant) {
        if matches!(self, Self::Queued { .. }) {
            *self = Self::Ready {
                deadline: now + READINESS_TIMEOUT,
                retry_delay: READINESS_RETRY_INITIAL,
            };
        }
    }

    fn retry_at(&self) -> Option<Instant> {
        match self {
            Self::Deferred { retry_at, .. } => Some(*retry_at),
            Self::Queued { .. } | Self::Ready { .. } => None,
        }
    }

    fn retry_now(&mut self) {
        let Self::Deferred {
            deadline,
            retry_delay,
            ..
        } = *self
        else {
            return;
        };
        *self = Self::Ready {
            deadline,
            retry_delay,
        };
    }

    fn readiness_may_have_changed(&mut self) {
        let Self::Deferred { deadline, .. } = *self else {
            return;
        };
        *self = Self::Ready {
            deadline,
            retry_delay: READINESS_RETRY_INITIAL,
        };
    }

    fn defer(&mut self, now: Instant) {
        let Self::Ready {
            deadline,
            retry_delay,
        } = *self
        else {
            return;
        };
        let retry_at = (now + retry_delay).min(deadline);
        *self = Self::Deferred {
            deadline,
            retry_at,
            retry_delay: retry_delay.saturating_mul(2).min(READINESS_RETRY_MAX),
        };
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
