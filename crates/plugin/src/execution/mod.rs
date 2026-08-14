use std::sync::{Arc, Mutex};

use acadctl_lisp::{FormSpan, ScanError, ScanPosition};
use bytes::Bytes;
use output::{OutputSink, OutputStream};

pub mod output;
pub mod value;
pub mod value_bridge;
pub(crate) mod visitor;

pub const EVALUATOR_SOURCE: &str = include_str!("../../lisp/acadctl.lsp");

pub(crate) fn bound_diagnostic(message: &mut String) {
    const SUFFIX: &str = "... [truncated]";
    if message.len() <= acadctl_rpc::MAX_DIAGNOSTIC_BYTES {
        return;
    }
    let mut end = acadctl_rpc::MAX_DIAGNOSTIC_BYTES - SUFFIX.len();
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message.push_str(SUFFIX);
}

pub(crate) fn bounded_diagnostic(mut message: String) -> String {
    bound_diagnostic(&mut message);
    message
}

fn append_diagnostic(message: &mut String, detail: &str) {
    message.push_str("; ");
    message.push_str(detail);
    bound_diagnostic(message);
}

pub struct Execution {
    mode: ExecutionMode,
    source_name: String,
    source: Bytes,
    next_scan: ScanPosition,
    next_form_index: usize,
    phase: Phase,
    form_attempted: bool,
    value_retained: bool,
    eval_location: Option<SourceLocation>,
    cancel_requested: bool,
    unwind: Option<UnwindCause>,
    outcome: Option<ExecutionOutcome>,
    io: Arc<ExecutionIo>,
}

pub(crate) struct ExecutionIo {
    output: OutputSink,
    bridge: Mutex<ValueBridgeState>,
}

#[derive(Default)]
struct ValueBridgeState {
    generation: u64,
    open_kind: Option<ValueOutputKind>,
    writer_active: bool,
    writer_claimed: bool,
    failure: Option<ValueBridgeFailure>,
}

pub(crate) struct ValueOutputLease {
    io: Arc<ExecutionIo>,
    generation: u64,
    kind: ValueOutputKind,
    released: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValueOutputKind {
    Form,
    EvalValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValueBridgeFailure {
    InvalidSequence,
    LimitExceeded,
    OutputFinished,
    PostCommitCancelled,
    Abandoned,
    MissingValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionMode {
    Eval,
    Exec,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceValidationError {
    SourceTooLarge,
    InvalidUtf8,
    NullCharacter,
    ExpectedOneForm { actual: usize },
    Scan(ScanError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionOutcome {
    Success,
    Failure(ExecutionFailure),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionFailure {
    pub message: String,
    pub form_index: Option<usize>,
    pub location: Option<SourceLocation>,
    pub drawing_outcome: DrawingOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub source_name: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawingOutcome {
    NotStarted,
    RolledBack,
    Committed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionStepKind {
    Invalid,
    BeginUndoGroup,
    EvaluateForm,
    CommitUndoGroup,
    EmitEvalValue,
    ClearRetainedEvalValue,
    CloseEmptyUndoGroup,
    RollbackUndoGroup,
    Done,
}

pub struct NativeExecutionStep {
    kind: ExecutionStepKind,
    source: Option<Bytes>,
    span: Option<FormSpan>,
    retain_value: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionStepResultKind {
    Success,
    LispError,
    NativeError,
}

pub struct ExecutionStepResult {
    pub kind: ExecutionStepResultKind,
    pub native_status: i32,
    pub lisp_errno: i32,
    pub detail: String,
    pub evaluator_state_cleanup_status: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    BeginUndoGroup,
    AwaitingBeginUndoGroup,
    BetweenForms,
    AwaitingEvaluateForm {
        index: usize,
        line: usize,
        column: usize,
    },
    AwaitingCommitUndoGroup,
    EmitEvalValue,
    AwaitingEmitEvalValue,
    ClearRetainedEvalValue,
    AwaitingClearRetainedEvalValue,
    CloseEmptyUndoGroup,
    AwaitingCloseEmptyUndoGroup,
    RollbackUndoGroup,
    AwaitingRollbackUndoGroup,
    Terminal,
    Done,
}

enum UnwindCause {
    Failure(ExecutionFailure),
    Cancelled,
}

impl ExecutionIo {
    pub(crate) fn output_sink(&self) -> OutputSink {
        self.output.clone()
    }

    fn begin_value_output(&self, kind: ValueOutputKind) {
        let mut state = self
            .bridge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.open_kind.is_some() || state.writer_active {
            state.failure.get_or_insert(ValueBridgeFailure::Abandoned);
        }
        state.generation = state.generation.wrapping_add(1).max(1);
        state.open_kind = Some(kind);
        state.writer_active = false;
        state.writer_claimed = false;
    }

    fn acquire_value_output(self: &Arc<Self>, kind: ValueOutputKind) -> Option<ValueOutputLease> {
        let mut state = self
            .bridge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.open_kind != Some(kind)
            || state.failure.is_some()
            || state.writer_active
            || (kind == ValueOutputKind::EvalValue && state.writer_claimed)
        {
            return None;
        }
        state.writer_active = true;
        state.writer_claimed = true;
        Some(ValueOutputLease {
            io: Arc::clone(self),
            generation: state.generation,
            kind,
            released: false,
        })
    }

    fn close_value_output(&self, kind: ValueOutputKind) -> Option<ValueBridgeFailure> {
        let mut state = self
            .bridge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.open_kind != Some(kind) {
            state
                .failure
                .get_or_insert(ValueBridgeFailure::InvalidSequence);
        }
        state.open_kind = None;
        if state.writer_active {
            state.failure.get_or_insert(ValueBridgeFailure::Abandoned);
        }
        state.writer_active = false;
        if kind == ValueOutputKind::EvalValue && !state.writer_claimed {
            state
                .failure
                .get_or_insert(ValueBridgeFailure::MissingValue);
        }
        state.failure.take()
    }

    fn value_output_is_open(&self, generation: u64, kind: ValueOutputKind) -> bool {
        let state = self
            .bridge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.open_kind == Some(kind)
            && state.generation == generation
            && state.writer_active
            && state.failure.is_none()
    }

    fn release_value_output(
        &self,
        generation: u64,
        kind: ValueOutputKind,
        failure: Option<ValueBridgeFailure>,
    ) {
        let mut state = self
            .bridge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.generation != generation || state.open_kind != Some(kind) {
            return;
        }
        if let Some(failure) = failure {
            state.failure.get_or_insert(failure);
        }
        state.writer_active = false;
    }

    #[cfg(test)]
    fn record_bridge_failure(&self, failure: ValueBridgeFailure) {
        let mut state = self
            .bridge
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.failure.get_or_insert(failure);
    }
}

impl ValueOutputLease {
    pub(crate) fn output_sink(&self) -> OutputSink {
        self.io.output_sink()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.io.value_output_is_open(self.generation, self.kind)
    }

    pub(crate) fn release(mut self, failure: Option<ValueBridgeFailure>) {
        self.io
            .release_value_output(self.generation, self.kind, failure);
        self.released = true;
    }
}

impl Drop for ValueOutputLease {
    fn drop(&mut self) {
        if !self.released {
            self.io.release_value_output(
                self.generation,
                self.kind,
                Some(ValueBridgeFailure::Abandoned),
            );
        }
    }
}

impl ValueBridgeFailure {
    const fn message(self) -> &'static str {
        match self {
            Self::InvalidSequence => "the AutoLISP output bridge emitted an invalid value sequence",
            Self::LimitExceeded => "the AutoLISP output bridge exceeded its structural limit",
            Self::OutputFinished => "execution output ended while AutoLISP was still producing it",
            Self::PostCommitCancelled => "eval result output was cancelled after commit",
            Self::Abandoned => "the AutoLISP output bridge abandoned an unfinished value",
            Self::MissingValue => "the AutoLISP evaluator did not emit its result value",
        }
    }
}

impl Execution {
    pub fn new(
        mode: ExecutionMode,
        source_name: String,
        source: Bytes,
    ) -> Result<(Self, OutputStream), SourceValidationError> {
        let mut source = source;
        if source.starts_with(&[0xef, 0xbb, 0xbf]) {
            source = source.slice(3..);
        }
        if source.len() > acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES {
            return Err(SourceValidationError::SourceTooLarge);
        }
        let source_text =
            std::str::from_utf8(&source).map_err(|_| SourceValidationError::InvalidUtf8)?;
        if source_text.contains('\0') {
            return Err(SourceValidationError::NullCharacter);
        }

        let form_count =
            acadctl_lisp::validate(source_text).map_err(SourceValidationError::Scan)?;
        if mode == ExecutionMode::Eval && form_count != 1 {
            return Err(SourceValidationError::ExpectedOneForm { actual: form_count });
        }
        let next_scan = acadctl_lisp::scan(source_text).position();
        let empty = form_count == 0;
        let (output, stream) = output::channel();
        let io = Arc::new(ExecutionIo {
            output,
            bridge: Mutex::new(ValueBridgeState::default()),
        });
        if empty {
            io.output.finish();
        }
        Ok((
            Self {
                mode,
                source_name,
                source,
                next_scan,
                next_form_index: 1,
                phase: if empty {
                    Phase::Terminal
                } else {
                    Phase::BeginUndoGroup
                },
                form_attempted: false,
                value_retained: false,
                eval_location: None,
                cancel_requested: false,
                unwind: None,
                outcome: empty.then_some(ExecutionOutcome::Success),
                io,
            },
            stream,
        ))
    }

    #[cfg(test)]
    pub const fn mode(&self) -> ExecutionMode {
        self.mode
    }

    pub fn outcome(&self) -> Option<&ExecutionOutcome> {
        self.outcome.as_ref()
    }

    pub fn take_outcome(&mut self) -> Option<ExecutionOutcome> {
        self.outcome.take()
    }

    pub fn output_sink(&self) -> OutputSink {
        self.io.output.clone()
    }

    pub fn source_bytes(&self) -> usize {
        self.source.len()
    }

    pub fn form_started(&self) -> bool {
        self.form_attempted
    }

    pub fn cancellation_requested(&self) -> bool {
        self.cancel_requested
    }

    pub fn start_deadline_pending(&self) -> bool {
        !self.form_attempted
            && self.outcome.is_none()
            && !self.cancel_requested
            && self.unwind.is_none()
    }

    pub(crate) fn acquire_form_output(&self) -> Option<ValueOutputLease> {
        matches!(self.phase, Phase::AwaitingEvaluateForm { .. })
            .then(|| self.io.acquire_value_output(ValueOutputKind::Form))
            .flatten()
    }

    pub(crate) fn acquire_eval_value_output(&self) -> Option<ValueOutputLease> {
        matches!(self.phase, Phase::AwaitingEmitEvalValue)
            .then(|| self.io.acquire_value_output(ValueOutputKind::EvalValue))
            .flatten()
    }

    pub fn request_cancel(&mut self) -> bool {
        if self.cancel_requested {
            return true;
        }
        if self.unwind.is_some() {
            return false;
        }
        if matches!(
            self.phase,
            Phase::AwaitingCommitUndoGroup
                | Phase::EmitEvalValue
                | Phase::AwaitingEmitEvalValue
                | Phase::ClearRetainedEvalValue
                | Phase::AwaitingClearRetainedEvalValue
                | Phase::CloseEmptyUndoGroup
                | Phase::AwaitingCloseEmptyUndoGroup
                | Phase::RollbackUndoGroup
                | Phase::AwaitingRollbackUndoGroup
                | Phase::Terminal
                | Phase::Done
        ) {
            return false;
        }
        self.cancel_requested = true;
        true
    }

    pub fn expire_before_start(&mut self, message: String) -> bool {
        if self.form_attempted || self.outcome.is_some() || self.cancel_requested {
            return false;
        }

        let failure = ExecutionFailure {
            message,
            form_index: None,
            location: None,
            drawing_outcome: DrawingOutcome::NotStarted,
        };
        match self.phase {
            Phase::BeginUndoGroup => {
                self.outcome = Some(ExecutionOutcome::Failure(failure));
                self.phase = Phase::Terminal;
            }
            Phase::AwaitingBeginUndoGroup => {
                self.unwind = Some(UnwindCause::Failure(failure));
            }
            Phase::BetweenForms => {
                self.unwind = Some(UnwindCause::Failure(failure));
                self.phase = Phase::CloseEmptyUndoGroup;
            }
            Phase::AwaitingEvaluateForm { .. }
            | Phase::AwaitingCommitUndoGroup
            | Phase::EmitEvalValue
            | Phase::AwaitingEmitEvalValue
            | Phase::ClearRetainedEvalValue
            | Phase::AwaitingClearRetainedEvalValue
            | Phase::CloseEmptyUndoGroup
            | Phase::AwaitingCloseEmptyUndoGroup
            | Phase::RollbackUndoGroup
            | Phase::AwaitingRollbackUndoGroup
            | Phase::Terminal
            | Phase::Done => return false,
        }
        true
    }

    pub fn cancel_before_start(&mut self) -> bool {
        if self.outcome.is_some() || !matches!(self.phase, Phase::BeginUndoGroup) {
            return false;
        }
        self.cancel_requested = true;
        self.outcome = Some(ExecutionOutcome::Cancelled);
        self.phase = Phase::Terminal;
        true
    }

    pub fn take_step(&mut self) -> NativeExecutionStep {
        loop {
            match self.phase {
                Phase::BeginUndoGroup => {
                    if self.cancel_requested {
                        self.outcome = Some(ExecutionOutcome::Cancelled);
                        self.phase = Phase::Terminal;
                        continue;
                    }
                    self.phase = Phase::AwaitingBeginUndoGroup;
                    return NativeExecutionStep::new(ExecutionStepKind::BeginUndoGroup);
                }
                Phase::BetweenForms => {
                    if self.cancel_requested {
                        if self.form_attempted {
                            self.begin_unwind(UnwindCause::Cancelled);
                        } else {
                            self.unwind = Some(UnwindCause::Cancelled);
                            self.phase = Phase::CloseEmptyUndoGroup;
                        }
                        continue;
                    }
                    let source = std::str::from_utf8(&self.source)
                        .expect("validated execution source remains UTF-8");
                    let mut scanner = acadctl_lisp::Scanner::resume(source, self.next_scan);
                    match scanner.next() {
                        Some(Ok(span)) => {
                            self.next_scan = scanner.position();
                            let index = self.next_form_index;
                            self.next_form_index += 1;
                            self.phase = Phase::AwaitingEvaluateForm {
                                index,
                                line: span.line,
                                column: span.column,
                            };
                            if self.mode == ExecutionMode::Eval {
                                self.eval_location = Some(SourceLocation {
                                    source_name: self.source_name.clone(),
                                    line: span.line,
                                    column: span.column,
                                });
                            }
                            self.io.begin_value_output(ValueOutputKind::Form);
                            self.form_attempted = true;
                            return NativeExecutionStep::form(
                                self.source.clone(),
                                span,
                                self.mode == ExecutionMode::Eval,
                            );
                        }
                        Some(Err(error)) => {
                            self.begin_unwind(UnwindCause::Failure(ExecutionFailure {
                                message: error.kind.message().to_owned(),
                                form_index: None,
                                location: Some(SourceLocation {
                                    source_name: self.source_name.clone(),
                                    line: error.line,
                                    column: error.column,
                                }),
                                drawing_outcome: DrawingOutcome::Unknown,
                            }));
                        }
                        None => {
                            self.phase = Phase::AwaitingCommitUndoGroup;
                            return NativeExecutionStep::new(ExecutionStepKind::CommitUndoGroup);
                        }
                    }
                }
                Phase::EmitEvalValue => {
                    self.io.begin_value_output(ValueOutputKind::EvalValue);
                    self.phase = Phase::AwaitingEmitEvalValue;
                    return NativeExecutionStep::new(ExecutionStepKind::EmitEvalValue);
                }
                Phase::ClearRetainedEvalValue => {
                    self.phase = Phase::AwaitingClearRetainedEvalValue;
                    return NativeExecutionStep::new(ExecutionStepKind::ClearRetainedEvalValue);
                }
                Phase::CloseEmptyUndoGroup => {
                    self.phase = Phase::AwaitingCloseEmptyUndoGroup;
                    return NativeExecutionStep::new(ExecutionStepKind::CloseEmptyUndoGroup);
                }
                Phase::RollbackUndoGroup => {
                    self.phase = Phase::AwaitingRollbackUndoGroup;
                    return NativeExecutionStep::new(ExecutionStepKind::RollbackUndoGroup);
                }
                Phase::Terminal => {
                    self.phase = Phase::Done;
                    return NativeExecutionStep::new(ExecutionStepKind::Done);
                }
                Phase::AwaitingBeginUndoGroup
                | Phase::AwaitingEvaluateForm { .. }
                | Phase::AwaitingCommitUndoGroup
                | Phase::AwaitingEmitEvalValue
                | Phase::AwaitingClearRetainedEvalValue
                | Phase::AwaitingCloseEmptyUndoGroup
                | Phase::AwaitingRollbackUndoGroup
                | Phase::Done => return NativeExecutionStep::new(ExecutionStepKind::Invalid),
            }
        }
    }

    pub fn complete_step(&mut self, result: ExecutionStepResult) -> bool {
        match self.phase {
            Phase::AwaitingBeginUndoGroup => {
                if result.succeeded() {
                    if self.unwind.is_some() {
                        self.phase = Phase::CloseEmptyUndoGroup;
                    } else if self.cancel_requested {
                        self.unwind = Some(UnwindCause::Cancelled);
                        self.phase = Phase::CloseEmptyUndoGroup;
                    } else {
                        self.phase = Phase::BetweenForms;
                    }
                } else if matches!(self.unwind, Some(UnwindCause::Failure(_))) {
                    let Some(UnwindCause::Failure(mut failure)) = self.unwind.take() else {
                        unreachable!("the unwind cause was just matched")
                    };
                    let begin = result.into_message("could not begin the undo group");
                    append_diagnostic(&mut failure.message, &begin);
                    self.outcome = Some(ExecutionOutcome::Failure(failure));
                    self.phase = Phase::Terminal;
                } else {
                    self.unwind = None;
                    self.outcome = Some(ExecutionOutcome::Failure(ExecutionFailure {
                        message: result.into_message("could not begin the undo group"),
                        form_index: None,
                        location: None,
                        drawing_outcome: DrawingOutcome::NotStarted,
                    }));
                    self.phase = Phase::Terminal;
                }
            }
            Phase::AwaitingEvaluateForm {
                index,
                line,
                column,
            } => {
                let bridge_failure = self.io.close_value_output(ValueOutputKind::Form);
                if self.mode == ExecutionMode::Eval && result.succeeded() {
                    self.value_retained = true;
                }
                if result.primary_failed() {
                    self.begin_unwind(UnwindCause::Failure(ExecutionFailure {
                        message: result.into_message("form evaluation failed"),
                        form_index: Some(index),
                        location: Some(SourceLocation {
                            source_name: self.source_name.clone(),
                            line,
                            column,
                        }),
                        drawing_outcome: DrawingOutcome::Unknown,
                    }));
                } else if let Some(failure) = bridge_failure {
                    let mut message = failure.message().to_owned();
                    if let Some(cleanup) = result.evaluator_state_cleanup_message() {
                        append_diagnostic(&mut message, &cleanup);
                    }
                    self.begin_unwind(UnwindCause::Failure(ExecutionFailure {
                        message,
                        form_index: Some(index),
                        location: Some(SourceLocation {
                            source_name: self.source_name.clone(),
                            line,
                            column,
                        }),
                        drawing_outcome: DrawingOutcome::Unknown,
                    }));
                } else if !result.succeeded() {
                    self.begin_unwind(UnwindCause::Failure(ExecutionFailure {
                        message: result.into_message("form evaluation failed"),
                        form_index: Some(index),
                        location: Some(SourceLocation {
                            source_name: self.source_name.clone(),
                            line,
                            column,
                        }),
                        drawing_outcome: DrawingOutcome::Unknown,
                    }));
                } else {
                    if self.cancel_requested {
                        self.begin_unwind(UnwindCause::Cancelled);
                    } else {
                        self.phase = Phase::BetweenForms;
                    }
                }
            }
            Phase::AwaitingCommitUndoGroup => {
                if result.succeeded() {
                    if self.mode == ExecutionMode::Eval && self.value_retained {
                        self.phase = Phase::EmitEvalValue;
                    } else if self.mode == ExecutionMode::Eval {
                        self.outcome = Some(ExecutionOutcome::Failure(self.eval_failure(
                            "the AutoLISP evaluator did not retain its result value".to_owned(),
                            DrawingOutcome::Committed,
                        )));
                        self.phase = Phase::Terminal;
                    } else {
                        self.outcome = Some(ExecutionOutcome::Success);
                        self.phase = Phase::Terminal;
                    }
                } else {
                    self.begin_unwind(UnwindCause::Failure(ExecutionFailure {
                        message: result.into_message("could not finish the undo group"),
                        form_index: None,
                        location: None,
                        drawing_outcome: DrawingOutcome::Unknown,
                    }));
                }
            }
            Phase::AwaitingEmitEvalValue => {
                let bridge_failure = self.io.close_value_output(ValueOutputKind::EvalValue);
                let failure = if result.primary_failed() {
                    Some(result.into_message("could not emit the eval result"))
                } else if let Some(bridge_failure) = bridge_failure {
                    let mut message = bridge_failure.message().to_owned();
                    if let Some(cleanup) = result.evaluator_state_cleanup_message() {
                        append_diagnostic(&mut message, &cleanup);
                    }
                    Some(message)
                } else if !result.succeeded() {
                    Some(result.into_message("could not emit the eval result"))
                } else {
                    None
                };
                if let Some(message) = failure {
                    self.outcome = Some(ExecutionOutcome::Failure(
                        self.eval_failure(message, DrawingOutcome::Committed),
                    ));
                } else {
                    self.value_retained = false;
                    self.outcome = Some(ExecutionOutcome::Success);
                }
                self.phase = Phase::Terminal;
            }
            Phase::AwaitingClearRetainedEvalValue => {
                if result.succeeded() {
                    self.value_retained = false;
                } else {
                    let cleanup = result
                        .into_message("could not clear the retained AutoLISP evaluator value");
                    self.record_value_cleanup_failure(cleanup);
                }
                self.phase = Phase::RollbackUndoGroup;
            }
            Phase::AwaitingCloseEmptyUndoGroup => {
                let Some(cause) = self.unwind.take() else {
                    return false;
                };
                self.outcome = Some(match cause {
                    UnwindCause::Cancelled if result.succeeded() => ExecutionOutcome::Cancelled,
                    UnwindCause::Cancelled => ExecutionOutcome::Failure(ExecutionFailure {
                        message: result.into_message("could not close the cancelled undo group"),
                        form_index: None,
                        location: None,
                        drawing_outcome: DrawingOutcome::Unknown,
                    }),
                    UnwindCause::Failure(failure) if result.succeeded() => {
                        ExecutionOutcome::Failure(failure)
                    }
                    UnwindCause::Failure(mut failure) => {
                        let cleanup = result.into_message("could not close the expired undo group");
                        append_diagnostic(&mut failure.message, &cleanup);
                        failure.drawing_outcome = DrawingOutcome::Unknown;
                        ExecutionOutcome::Failure(failure)
                    }
                });
                self.phase = Phase::Terminal;
            }
            Phase::AwaitingRollbackUndoGroup => {
                let Some(cause) = self.unwind.take() else {
                    return false;
                };
                self.outcome = Some(match cause {
                    UnwindCause::Failure(mut failure) => {
                        if result.succeeded() {
                            failure.drawing_outcome = DrawingOutcome::RolledBack;
                        } else {
                            let unwind = result.into_message("drawing unwind failed");
                            append_diagnostic(&mut failure.message, &unwind);
                            failure.drawing_outcome = DrawingOutcome::Unknown;
                        }
                        ExecutionOutcome::Failure(failure)
                    }
                    UnwindCause::Cancelled => {
                        if result.succeeded() {
                            ExecutionOutcome::Cancelled
                        } else {
                            ExecutionOutcome::Failure(ExecutionFailure {
                                message: result.into_message("drawing unwind failed"),
                                form_index: None,
                                location: None,
                                drawing_outcome: DrawingOutcome::Unknown,
                            })
                        }
                    }
                });
                self.phase = Phase::Terminal;
            }
            Phase::BeginUndoGroup
            | Phase::BetweenForms
            | Phase::EmitEvalValue
            | Phase::ClearRetainedEvalValue
            | Phase::CloseEmptyUndoGroup
            | Phase::RollbackUndoGroup
            | Phase::Terminal
            | Phase::Done => return false,
        }
        true
    }

    fn begin_unwind(&mut self, cause: UnwindCause) {
        self.unwind = Some(cause);
        self.phase = if self.mode == ExecutionMode::Eval && self.form_attempted {
            Phase::ClearRetainedEvalValue
        } else {
            Phase::RollbackUndoGroup
        };
    }

    fn record_value_cleanup_failure(&mut self, cleanup: String) {
        let cause = match self.unwind.take() {
            Some(UnwindCause::Failure(mut failure)) => {
                append_diagnostic(&mut failure.message, &cleanup);
                UnwindCause::Failure(failure)
            }
            Some(UnwindCause::Cancelled) | None => {
                UnwindCause::Failure(self.eval_failure(cleanup, DrawingOutcome::Unknown))
            }
        };
        self.unwind = Some(cause);
    }

    fn eval_failure(&self, message: String, drawing_outcome: DrawingOutcome) -> ExecutionFailure {
        ExecutionFailure {
            message: bounded_diagnostic(message),
            form_index: Some(1),
            location: self.eval_location.clone(),
            drawing_outcome,
        }
    }

    pub fn record_terminal_failure(&mut self, result: ExecutionStepResult) -> bool {
        let cleanup = result.into_message("the native execution lease could not be released");
        let Some(outcome) = self.outcome.take() else {
            return false;
        };
        let failure = match outcome {
            ExecutionOutcome::Success => ExecutionFailure {
                message: cleanup,
                form_index: None,
                location: None,
                drawing_outcome: DrawingOutcome::Unknown,
            },
            ExecutionOutcome::Failure(mut failure) => {
                append_diagnostic(&mut failure.message, &cleanup);
                failure.drawing_outcome = DrawingOutcome::Unknown;
                failure
            }
            ExecutionOutcome::Cancelled => ExecutionFailure {
                message: cleanup,
                form_index: None,
                location: None,
                drawing_outcome: DrawingOutcome::Unknown,
            },
        };
        self.outcome = Some(ExecutionOutcome::Failure(failure));
        true
    }

    pub fn abandon(&mut self, result: ExecutionStepResult) -> bool {
        let message = result.into_message("execution could not continue safely");
        let phase = self.phase;
        if matches!(phase, Phase::AwaitingEvaluateForm { .. }) {
            let _ = self.io.close_value_output(ValueOutputKind::Form);
        } else if phase == Phase::AwaitingEmitEvalValue {
            let _ = self.io.close_value_output(ValueOutputKind::EvalValue);
        }
        let existing = self.outcome.take().or_else(|| {
            self.unwind.take().map(|cause| match cause {
                UnwindCause::Failure(failure) => ExecutionOutcome::Failure(failure),
                UnwindCause::Cancelled => ExecutionOutcome::Cancelled,
            })
        });
        let failure = match existing {
            Some(ExecutionOutcome::Success) => ExecutionFailure {
                message,
                form_index: None,
                location: None,
                drawing_outcome: DrawingOutcome::Unknown,
            },
            Some(ExecutionOutcome::Failure(mut failure)) => {
                append_diagnostic(&mut failure.message, &message);
                failure.drawing_outcome = DrawingOutcome::Unknown;
                failure
            }
            Some(ExecutionOutcome::Cancelled) => ExecutionFailure {
                message,
                form_index: None,
                location: None,
                drawing_outcome: DrawingOutcome::Unknown,
            },
            None => {
                let (form_index, location) = match phase {
                    Phase::AwaitingEvaluateForm {
                        index,
                        line,
                        column,
                    } => (
                        Some(index),
                        Some(SourceLocation {
                            source_name: self.source_name.clone(),
                            line,
                            column,
                        }),
                    ),
                    Phase::AwaitingEmitEvalValue => (Some(1), self.eval_location.clone()),
                    _ => (None, None),
                };
                ExecutionFailure {
                    message,
                    form_index,
                    location,
                    drawing_outcome: DrawingOutcome::Unknown,
                }
            }
        };
        self.outcome = Some(ExecutionOutcome::Failure(failure));
        self.phase = if phase == Phase::Done {
            Phase::Done
        } else {
            Phase::Terminal
        };
        true
    }
}

impl NativeExecutionStep {
    pub fn invalid() -> Self {
        Self::new(ExecutionStepKind::Invalid)
    }

    fn new(kind: ExecutionStepKind) -> Self {
        Self {
            kind,
            source: None,
            span: None,
            retain_value: false,
        }
    }

    fn form(source: Bytes, span: FormSpan, retain_value: bool) -> Self {
        Self {
            kind: ExecutionStepKind::EvaluateForm,
            source: Some(source),
            span: Some(span),
            retain_value,
        }
    }

    pub const fn kind(&self) -> ExecutionStepKind {
        self.kind
    }

    pub fn source(&self) -> &str {
        match (&self.source, &self.span) {
            (Some(source), Some(span)) => {
                let source =
                    std::str::from_utf8(source).expect("validated execution source remains UTF-8");
                &source[span.byte_start..span.byte_end]
            }
            _ => "",
        }
    }

    pub const fn retain_value(&self) -> bool {
        self.retain_value
    }
}

impl ExecutionStepResult {
    fn succeeded(&self) -> bool {
        self.kind == ExecutionStepResultKind::Success && self.evaluator_state_cleanup_status == 0
    }

    fn primary_failed(&self) -> bool {
        self.kind != ExecutionStepResultKind::Success
    }

    fn evaluator_state_cleanup_message(&self) -> Option<String> {
        (self.evaluator_state_cleanup_status != 0).then(|| {
            format!(
                "could not clear the reserved AutoLISP evaluator state (native status {})",
                self.evaluator_state_cleanup_status
            )
        })
    }

    fn into_message(self, fallback: &str) -> String {
        let cleanup = self.evaluator_state_cleanup_message();
        let primary = if self.kind == ExecutionStepResultKind::Success {
            None
        } else if !self.detail.is_empty() {
            Some(self.detail)
        } else if self.kind == ExecutionStepResultKind::LispError && self.lisp_errno != 0 {
            Some(format!("{fallback} (ERRNO {})", self.lisp_errno))
        } else if self.native_status != 0 {
            Some(format!("{fallback} (native status {})", self.native_status))
        } else {
            Some(fallback.to_owned())
        };
        bounded_diagnostic(match (primary, cleanup) {
            (Some(primary), Some(cleanup)) => format!("{primary}; {cleanup}"),
            (Some(primary), None) => primary,
            (None, Some(cleanup)) => cleanup,
            (None, None) => fallback.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::value_bridge::{NativeValueWriter, ValueEvent, WriteResult};
    use super::*;

    #[test]
    fn diagnostic_composition_preserves_the_stored_byte_limit() {
        let mut message = bounded_diagnostic("é".repeat(acadctl_rpc::MAX_DIAGNOSTIC_BYTES));
        append_diagnostic(&mut message, &"x".repeat(acadctl_rpc::MAX_DIAGNOSTIC_BYTES));
        assert!(message.len() <= acadctl_rpc::MAX_DIAGNOSTIC_BYTES);
        assert!(message.ends_with("... [truncated]"));
        assert!(std::str::from_utf8(message.as_bytes()).is_ok());
    }

    #[test]
    fn validates_source_before_native_admission() {
        assert!(
            Execution::new(
                ExecutionMode::Exec,
                "<stdin>".into(),
                "x".repeat(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES).into(),
            )
            .is_ok()
        );
        assert!(
            Execution::new(
                ExecutionMode::Exec,
                "<stdin>".into(),
                format!(
                    "\u{feff}{}",
                    "x".repeat(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES)
                )
                .into(),
            )
            .is_ok()
        );
        assert_eq!(
            Execution::new(
                ExecutionMode::Exec,
                "<stdin>".into(),
                "x".repeat(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 1)
                    .into(),
            )
            .err()
            .unwrap(),
            SourceValidationError::SourceTooLarge
        );
        assert_eq!(
            Execution::new(ExecutionMode::Exec, "<stdin>".into(), "x\0y".into())
                .err()
                .unwrap(),
            SourceValidationError::NullCharacter
        );
        assert_eq!(
            Execution::new(
                ExecutionMode::Exec,
                "<stdin>".into(),
                Bytes::from_static(b"\xff"),
            )
            .err()
            .unwrap(),
            SourceValidationError::InvalidUtf8
        );
        assert!(matches!(
            Execution::new(ExecutionMode::Exec, "<stdin>".into(), "(unfinished".into(),),
            Err(SourceValidationError::Scan(_))
        ));
    }

    #[test]
    fn bom_removal_and_native_steps_share_the_source_allocation() {
        let source = Bytes::from_static(b"\xef\xbb\xbfform");
        let expected = source.as_ref()[3..].as_ptr();
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "<stdin>".into(), source).unwrap();
        assert_eq!(execution.source.as_ptr(), expected);
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::BeginUndoGroup
        );
        assert!(execution.complete_step(success()));
        let form = execution.take_step();
        assert_eq!(form.kind(), ExecutionStepKind::EvaluateForm);
        assert_eq!(form.source.as_ref().unwrap().as_ptr(), expected);
        assert_eq!(form.source(), "form");
    }

    #[test]
    fn eval_requires_exactly_one_form_while_exec_accepts_a_batch() {
        assert_eq!(
            Execution::new(ExecutionMode::Eval, "<stdin>".into(), "".into())
                .err()
                .unwrap(),
            SourceValidationError::ExpectedOneForm { actual: 0 }
        );
        assert_eq!(
            Execution::new(ExecutionMode::Eval, "<stdin>".into(), "a b".into())
                .err()
                .unwrap(),
            SourceValidationError::ExpectedOneForm { actual: 2 }
        );
        let eval = Execution::new(ExecutionMode::Eval, "<stdin>".into(), "a".into())
            .unwrap()
            .0;
        assert_eq!(eval.mode(), ExecutionMode::Eval);

        let exec = Execution::new(ExecutionMode::Exec, "<stdin>".into(), "a b".into())
            .unwrap()
            .0;
        assert_eq!(exec.mode(), ExecutionMode::Exec);
    }

    #[test]
    fn embedded_evaluator_is_one_complete_form() {
        assert_eq!(acadctl_lisp::validate(EVALUATOR_SOURCE), Ok(1));
    }

    #[test]
    fn yields_exact_forms_then_commits() {
        let mut execution = Execution::new(
            ExecutionMode::Exec,
            "batch.lsp".into(),
            "(setq x 1) ; keep with separator\n(+ x 2)".into(),
        )
        .unwrap()
        .0;

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::BeginUndoGroup
        );
        assert!(execution.complete_step(success()));

        let first = execution.take_step();
        assert_eq!(first.kind(), ExecutionStepKind::EvaluateForm);
        assert_eq!(first.source(), "(setq x 1)");
        assert!(execution.complete_step(success()));

        let second = execution.take_step();
        assert_eq!(second.kind(), ExecutionStepKind::EvaluateForm);
        assert_eq!(second.source(), "(+ x 2)");
        assert!(execution.complete_step(success()));

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::CommitUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        assert_eq!(execution.outcome(), Some(&ExecutionOutcome::Success));
    }

    #[test]
    fn eval_retains_its_form_value_and_emits_it_only_after_commit() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Eval, "inspect.lsp".into(), "(+ 1 2)".into()).unwrap();
        begin(&mut execution);

        let form = execution.take_step();
        assert_eq!(form.kind(), ExecutionStepKind::EvaluateForm);
        assert!(form.retain_value());
        assert!(execution.complete_step(success()));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::CommitUndoGroup
        );
        assert!(!execution.request_cancel());
        assert!(execution.complete_step(success()));

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EmitEvalValue
        );
        let lease = execution
            .acquire_eval_value_output()
            .expect("the post-commit value epoch is open");
        let mut writer = NativeValueWriter::eval_value(lease);
        assert_eq!(writer.write(ValueEvent::Integer(3)), WriteResult::Continue);
        assert_eq!(writer.finish(), WriteResult::Continue);
        assert!(execution.complete_step(success()));

        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        assert_eq!(execution.outcome(), Some(&ExecutionOutcome::Success));
    }

    #[test]
    fn exec_forms_never_request_value_retention() {
        let mut execution =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "(+ 1 2)".into())
                .unwrap()
                .0;
        begin(&mut execution);

        let form = execution.take_step();
        assert_eq!(form.kind(), ExecutionStepKind::EvaluateForm);
        assert!(!form.retain_value());
    }

    #[test]
    fn a_missing_post_commit_writer_is_a_committed_failure() {
        let mut execution = eval_through_commit();
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EmitEvalValue
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);

        assert_eq!(
            execution.outcome(),
            Some(&ExecutionOutcome::Failure(ExecutionFailure {
                message: "the AutoLISP evaluator did not emit its result value".into(),
                form_index: Some(1),
                location: Some(SourceLocation {
                    source_name: "inspect.lsp".into(),
                    line: 1,
                    column: 1,
                }),
                drawing_outcome: DrawingOutcome::Committed,
            }))
        );
    }

    #[test]
    fn a_post_commit_native_failure_never_requests_rollback() {
        let mut execution = eval_through_commit();
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EmitEvalValue
        );
        assert!(execution.complete_step(native_error("value visitor failed", -5001)));

        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        let Some(ExecutionOutcome::Failure(failure)) = execution.outcome() else {
            panic!("expected committed serialization failure");
        };
        assert_eq!(failure.message, "value visitor failed");
        assert_eq!(failure.form_index, Some(1));
        assert_eq!(failure.drawing_outcome, DrawingOutcome::Committed);
    }

    #[test]
    fn post_commit_failure_keeps_visitor_and_cleanup_evidence() {
        let mut execution = eval_through_commit();
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EmitEvalValue
        );
        assert!(
            execution.complete_step(with_cleanup(lisp_error("value visitor failed", 7), -5001,))
        );

        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        let Some(ExecutionOutcome::Failure(failure)) = execution.outcome() else {
            panic!("expected committed serialization failure");
        };
        assert_eq!(
            failure.message,
            "value visitor failed; could not clear the reserved AutoLISP evaluator state (native status -5001)"
        );
        assert_eq!(failure.drawing_outcome, DrawingOutcome::Committed);
    }

    #[test]
    fn post_commit_bridge_failure_keeps_cleanup_evidence() {
        let mut execution = eval_through_commit();
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EmitEvalValue
        );
        assert!(execution.complete_step(with_cleanup(success(), -5001)));

        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        let Some(ExecutionOutcome::Failure(failure)) = execution.outcome() else {
            panic!("expected committed serialization failure");
        };
        assert_eq!(
            failure.message,
            "the AutoLISP evaluator did not emit its result value; could not clear the reserved AutoLISP evaluator state (native status -5001)"
        );
        assert_eq!(failure.drawing_outcome, DrawingOutcome::Committed);
    }

    #[test]
    fn eval_cancellation_clears_the_retained_value_before_rollback() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Eval, "inspect.lsp".into(), "form".into()).unwrap();
        begin(&mut execution);
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.request_cancel());
        assert!(execution.complete_step(success()));

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::ClearRetainedEvalValue
        );
        assert!(execution.complete_step(success()));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        assert_eq!(execution.outcome(), Some(&ExecutionOutcome::Cancelled));
    }

    #[test]
    fn eval_value_cleanup_failure_is_preserved_when_rollback_succeeds() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Eval, "inspect.lsp".into(), "form".into()).unwrap();
        begin(&mut execution);
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.request_cancel());
        assert!(execution.complete_step(success()));

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::ClearRetainedEvalValue
        );
        assert!(execution.complete_step(native_error("value cleanup failed", -5001)));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);

        let Some(ExecutionOutcome::Failure(failure)) = execution.outcome() else {
            panic!("expected cleanup failure");
        };
        assert_eq!(failure.message, "value cleanup failed");
        assert_eq!(failure.form_index, Some(1));
        assert_eq!(failure.drawing_outcome, DrawingOutcome::RolledBack);
    }

    #[test]
    fn form_failure_keeps_lisp_and_cleanup_evidence() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Eval, "inspect.lsp".into(), "form".into()).unwrap();
        begin(&mut execution);
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.complete_step(with_cleanup(lisp_error("bad argument type", 7), -5001,)));

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::ClearRetainedEvalValue
        );
        assert!(execution.complete_step(success()));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);

        let Some(ExecutionOutcome::Failure(failure)) = execution.outcome() else {
            panic!("expected evaluation failure");
        };
        assert_eq!(
            failure.message,
            "bad argument type; could not clear the reserved AutoLISP evaluator state (native status -5001)"
        );
        assert_eq!(failure.drawing_outcome, DrawingOutcome::RolledBack);
    }

    #[test]
    fn rolls_back_a_lisp_failure_at_its_form_location() {
        let mut execution =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "ok\n  bad".into())
                .unwrap()
                .0;
        begin(&mut execution);
        assert_eq!(execution.take_step().source(), "ok");
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().source(), "bad");
        assert!(execution.complete_step(lisp_error("bad argument type", 7)));

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        assert_eq!(
            execution.outcome(),
            Some(&ExecutionOutcome::Failure(ExecutionFailure {
                message: "bad argument type".into(),
                form_index: Some(2),
                location: Some(SourceLocation {
                    source_name: "batch.lsp".into(),
                    line: 2,
                    column: 3,
                }),
                drawing_outcome: DrawingOutcome::RolledBack,
            }))
        );
    }

    #[test]
    fn rollback_failure_preserves_the_original_error_and_marks_unknown() {
        let mut execution = Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "bad".into())
            .unwrap()
            .0;
        begin(&mut execution);
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.complete_step(lisp_error("boom", 0)));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(native_error("U failed", -5001)));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);

        let Some(ExecutionOutcome::Failure(failure)) = execution.outcome() else {
            panic!("expected failure");
        };
        assert_eq!(failure.message, "boom; U failed");
        assert_eq!(failure.drawing_outcome, DrawingOutcome::Unknown);
    }

    #[test]
    fn commit_failure_is_rolled_back() {
        let mut execution = Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "ok".into())
            .unwrap()
            .0;
        begin(&mut execution);
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.complete_step(success()));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::CommitUndoGroup
        );
        assert!(execution.complete_step(native_error("End failed", -5001)));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);

        assert_eq!(
            execution.outcome(),
            Some(&ExecutionOutcome::Failure(ExecutionFailure {
                message: "End failed".into(),
                form_index: None,
                location: None,
                drawing_outcome: DrawingOutcome::RolledBack,
            }))
        );
    }

    #[test]
    fn eval_commit_failure_clears_its_value_before_rollback() {
        let mut execution = Execution::new(ExecutionMode::Eval, "inspect.lsp".into(), "ok".into())
            .unwrap()
            .0;
        begin(&mut execution);
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.complete_step(success()));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::CommitUndoGroup
        );
        assert!(execution.complete_step(native_error("End failed", -5001)));

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::ClearRetainedEvalValue
        );
        assert!(execution.complete_step(success()));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        let Some(ExecutionOutcome::Failure(failure)) = execution.outcome() else {
            panic!("expected commit failure");
        };
        assert_eq!(failure.message, "End failed");
        assert_eq!(failure.drawing_outcome, DrawingOutcome::RolledBack);
    }

    #[test]
    fn begin_failure_never_claims_that_drawing_work_started() {
        let mut execution = Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "ok".into())
            .unwrap()
            .0;
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::BeginUndoGroup
        );
        assert!(execution.complete_step(native_error("Begin failed", -5001)));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);

        assert_eq!(
            execution.outcome(),
            Some(&ExecutionOutcome::Failure(ExecutionFailure {
                message: "Begin failed".into(),
                form_index: None,
                location: None,
                drawing_outcome: DrawingOutcome::NotStarted,
            }))
        );
    }

    #[test]
    fn cleanup_failure_overrides_success_with_an_unknown_drawing_outcome() {
        let mut execution = successful_execution();
        assert!(execution.record_terminal_failure(native_error("unlock failed", 42)));
        assert_eq!(
            execution.outcome(),
            Some(&ExecutionOutcome::Failure(ExecutionFailure {
                message: "unlock failed".into(),
                form_index: None,
                location: None,
                drawing_outcome: DrawingOutcome::Unknown,
            }))
        );
    }

    #[test]
    fn cleanup_failure_preserves_an_existing_execution_failure() {
        let mut execution = Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "bad".into())
            .unwrap()
            .0;
        begin(&mut execution);
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.complete_step(lisp_error("boom", 0)));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);

        assert!(execution.record_terminal_failure(native_error("restore failed", 43)));
        let Some(ExecutionOutcome::Failure(failure)) = execution.outcome() else {
            panic!("expected failure");
        };
        assert_eq!(failure.message, "boom; restore failed");
        assert_eq!(failure.drawing_outcome, DrawingOutcome::Unknown);
    }

    #[test]
    fn abandonment_terminalizes_an_in_flight_form_as_unknown() {
        let mut execution = Execution::new(
            ExecutionMode::Exec,
            "batch.lsp".into(),
            "ok\nchanged".into(),
        )
        .unwrap()
        .0;
        begin(&mut execution);
        assert_eq!(execution.take_step().source(), "ok");
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().source(), "changed");

        assert!(execution.abandon(native_error(
            "the target database changed during execution",
            -5001,
        )));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        assert_eq!(
            execution.outcome(),
            Some(&ExecutionOutcome::Failure(ExecutionFailure {
                message: "the target database changed during execution".into(),
                form_index: Some(2),
                location: Some(SourceLocation {
                    source_name: "batch.lsp".into(),
                    line: 2,
                    column: 1,
                }),
                drawing_outcome: DrawingOutcome::Unknown,
            }))
        );
    }

    #[test]
    fn abandonment_preserves_the_failure_that_started_rollback() {
        let mut execution = Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "bad".into())
            .unwrap()
            .0;
        begin(&mut execution);
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.complete_step(lisp_error("boom", 0)));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );

        assert!(execution.abandon(native_error("database replaced", -5001)));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        let Some(ExecutionOutcome::Failure(failure)) = execution.outcome() else {
            panic!("expected failure");
        };
        assert_eq!(failure.message, "boom; database replaced");
        assert_eq!(failure.drawing_outcome, DrawingOutcome::Unknown);
    }

    #[test]
    fn cancellation_before_begin_never_opens_an_undo_group() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();

        assert!(execution.request_cancel());
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        assert_eq!(execution.outcome(), Some(&ExecutionOutcome::Cancelled));
    }

    #[test]
    fn cancellation_during_begin_closes_the_empty_group_without_u() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::BeginUndoGroup
        );
        assert!(execution.request_cancel());
        assert!(execution.complete_step(success()));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::CloseEmptyUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        assert_eq!(execution.outcome(), Some(&ExecutionOutcome::Cancelled));
    }

    #[test]
    fn cancellation_after_a_form_uses_rollback() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        begin(&mut execution);

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.request_cancel());
        assert!(execution.complete_step(success()));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        assert_eq!(execution.outcome(), Some(&ExecutionOutcome::Cancelled));
    }

    #[test]
    fn evaluator_failure_wins_over_concurrent_cancellation() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "bad".into()).unwrap();
        begin(&mut execution);

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.request_cancel());
        assert!(execution.complete_step(lisp_error("boom", 0)));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        let Some(ExecutionOutcome::Failure(failure)) = execution.outcome() else {
            panic!("expected the evaluator failure");
        };
        assert_eq!(failure.message, "boom");
        assert_eq!(failure.drawing_outcome, DrawingOutcome::RolledBack);
    }

    #[test]
    fn evaluator_failure_wins_over_a_concurrent_output_bridge_failure() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "bad".into()).unwrap();
        begin(&mut execution);

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        execution
            .io
            .record_bridge_failure(ValueBridgeFailure::InvalidSequence);
        assert!(execution.complete_step(lisp_error("boom", 0)));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        let Some(ExecutionOutcome::Failure(failure)) = execution.outcome() else {
            panic!("expected the evaluator failure");
        };
        assert_eq!(failure.message, "boom");
    }

    #[test]
    fn output_bridge_failure_keeps_cleanup_evidence() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "bad".into()).unwrap();
        begin(&mut execution);

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        execution
            .io
            .record_bridge_failure(ValueBridgeFailure::InvalidSequence);
        assert!(execution.complete_step(with_cleanup(success(), -5001)));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        let Some(ExecutionOutcome::Failure(failure)) = execution.outcome() else {
            panic!("expected bridge failure");
        };
        assert_eq!(
            failure.message,
            "the AutoLISP output bridge emitted an invalid value sequence; could not clear the reserved AutoLISP evaluator state (native status -5001)"
        );
        assert_eq!(failure.drawing_outcome, DrawingOutcome::RolledBack);
    }

    #[test]
    fn output_bridge_failure_wins_over_concurrent_cancellation() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        begin(&mut execution);

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        execution
            .io
            .record_bridge_failure(ValueBridgeFailure::InvalidSequence);
        assert!(execution.request_cancel());
        assert!(execution.complete_step(success()));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        let Some(ExecutionOutcome::Failure(failure)) = execution.outcome() else {
            panic!("expected the output bridge failure");
        };
        assert_eq!(
            failure.message,
            "the AutoLISP output bridge emitted an invalid value sequence"
        );
    }

    #[test]
    fn cancellation_after_commit_handoff_is_too_late() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        begin(&mut execution);
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.complete_step(success()));

        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::CommitUndoGroup
        );
        assert!(!execution.request_cancel());
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        assert_eq!(execution.outcome(), Some(&ExecutionOutcome::Success));
    }

    #[test]
    fn rollback_failure_overrides_cancellation() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        begin(&mut execution);
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.request_cancel());
        assert!(execution.complete_step(success()));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::RollbackUndoGroup
        );
        assert!(execution.complete_step(native_error("U failed", -5001)));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);

        assert_eq!(
            execution.outcome(),
            Some(&ExecutionOutcome::Failure(ExecutionFailure {
                message: "U failed".into(),
                form_index: None,
                location: None,
                drawing_outcome: DrawingOutcome::Unknown,
            }))
        );
    }

    #[test]
    fn empty_batch_finishes_without_an_undo_group() {
        let mut execution = Execution::new(
            ExecutionMode::Exec,
            "<stdin>".into(),
            "; only a comment".into(),
        )
        .unwrap()
        .0;

        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        assert_eq!(execution.outcome(), Some(&ExecutionOutcome::Success));
    }

    fn begin(execution: &mut Execution) {
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::BeginUndoGroup
        );
        assert!(execution.complete_step(success()));
    }

    fn successful_execution() -> Execution {
        let mut execution = Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "ok".into())
            .unwrap()
            .0;
        begin(&mut execution);
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.complete_step(success()));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::CommitUndoGroup
        );
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), ExecutionStepKind::Done);
        execution
    }

    fn eval_through_commit() -> Execution {
        let mut execution =
            Execution::new(ExecutionMode::Eval, "inspect.lsp".into(), "(+ 1 2)".into())
                .unwrap()
                .0;
        begin(&mut execution);
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::EvaluateForm
        );
        assert!(execution.complete_step(success()));
        assert_eq!(
            execution.take_step().kind(),
            ExecutionStepKind::CommitUndoGroup
        );
        assert!(execution.complete_step(success()));
        execution
    }

    fn success() -> ExecutionStepResult {
        ExecutionStepResult {
            kind: ExecutionStepResultKind::Success,
            native_status: 0,
            lisp_errno: 0,
            detail: String::new(),
            evaluator_state_cleanup_status: 0,
        }
    }

    fn lisp_error(detail: &str, lisp_errno: i32) -> ExecutionStepResult {
        ExecutionStepResult {
            kind: ExecutionStepResultKind::LispError,
            native_status: 0,
            lisp_errno,
            detail: detail.into(),
            evaluator_state_cleanup_status: 0,
        }
    }

    fn native_error(detail: &str, native_status: i32) -> ExecutionStepResult {
        ExecutionStepResult {
            kind: ExecutionStepResultKind::NativeError,
            native_status,
            lisp_errno: 0,
            detail: detail.into(),
            evaluator_state_cleanup_status: 0,
        }
    }

    fn with_cleanup(
        mut result: ExecutionStepResult,
        evaluator_state_cleanup_status: i32,
    ) -> ExecutionStepResult {
        result.evaluator_state_cleanup_status = evaluator_state_cleanup_status;
        result
    }
}
