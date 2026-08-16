use std::sync::{Arc, Mutex};

use acadctl_lisp::{FormSpan, ScanPosition};
use acadctl_rpc::SourceName;
use bytes::Bytes;

use super::diagnostic::{append_diagnostic, bounded_diagnostic};
use super::io::{ExecIo, ValueBridgeState, ValueOutputKind, ValueOutputLease};
use super::outcome::{
    DrawingOutcome, ExecFailure, ExecMode, ExecOutcome, SourceLocation, SourceValidationError,
};
use super::output::{self, OutputSink, OutputStream};
use super::{ExecStepKind, ExecStepResult, ExecStepResultKind};

pub struct Exec {
    mode: ExecMode,
    source_name: SourceName,
    source: Bytes,
    next_scan: ScanPosition,
    next_form_index: usize,
    phase: Phase,
    form_handed_off: bool,
    value_retained: bool,
    eval_location: Option<SourceLocation>,
    cancel_requested: bool,
    unwind: Option<UnwindCause>,
    outcome: Option<ExecOutcome>,
    io: Arc<ExecIo>,
}

pub struct NativeExecStep {
    kind: ExecStepKind,
    source: Option<Bytes>,
    span: Option<FormSpan>,
    retain_value: bool,
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
    Failure(ExecFailure),
    Cancelled,
}

impl Exec {
    pub fn new(
        mode: ExecMode,
        source_name: SourceName,
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

        if mode == ExecMode::Eval && form_count != 1 {
            return Err(SourceValidationError::ExpectedOneForm { actual: form_count });
        }

        let next_scan = acadctl_lisp::scan(source_text).position();
        let empty = form_count == 0;
        let (output, stream) = output::channel();
        let io = Arc::new(ExecIo {
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
                form_handed_off: false,
                value_retained: false,
                eval_location: None,
                cancel_requested: false,
                unwind: None,
                outcome: empty.then_some(ExecOutcome::Success),
                io,
            },
            stream,
        ))
    }

    pub fn outcome(&self) -> Option<&ExecOutcome> {
        self.outcome.as_ref()
    }

    pub fn take_outcome(&mut self) -> Option<ExecOutcome> {
        self.outcome.take()
    }

    pub fn output_sink(&self) -> OutputSink {
        self.io.output.clone()
    }

    pub fn source_bytes(&self) -> usize {
        self.source.len()
    }

    pub fn has_handed_off_form(&self) -> bool {
        self.form_handed_off
    }

    pub fn cancellation_requested(&self) -> bool {
        self.cancel_requested
    }

    pub fn start_deadline_pending(&self) -> bool {
        !self.form_handed_off
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
        if self.form_handed_off || self.outcome.is_some() || self.cancel_requested {
            return false;
        }

        let failure = ExecFailure::not_started(message);

        match self.phase {
            Phase::BeginUndoGroup => {
                self.outcome = Some(ExecOutcome::Failure(failure));
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
        self.outcome = Some(ExecOutcome::Cancelled);
        self.phase = Phase::Terminal;
        true
    }

    pub fn take_step(&mut self) -> NativeExecStep {
        loop {
            match self.phase {
                Phase::BeginUndoGroup => {
                    if self.cancel_requested {
                        self.outcome = Some(ExecOutcome::Cancelled);
                        self.phase = Phase::Terminal;
                        continue;
                    }

                    self.phase = Phase::AwaitingBeginUndoGroup;

                    return NativeExecStep::new(ExecStepKind::BeginUndoGroup);
                }
                Phase::BetweenForms => {
                    if self.cancel_requested {
                        if self.form_handed_off {
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

                            if self.mode == ExecMode::Eval {
                                self.eval_location = Some(SourceLocation::from_span(
                                    self.source_name.clone(),
                                    &span,
                                ));
                            }

                            self.form_handed_off = true;
                            self.io.begin_value_output(ValueOutputKind::Form);

                            return NativeExecStep::form(
                                self.source.clone(),
                                span,
                                self.mode == ExecMode::Eval,
                            );
                        }
                        Some(Err(error)) => {
                            self.begin_unwind(UnwindCause::Failure(ExecFailure {
                                message: error.kind.message().to_owned(),
                                form_index: None,
                                location: Some(SourceLocation::from_scan_error(
                                    self.source_name.clone(),
                                    &error,
                                )),
                                drawing_outcome: DrawingOutcome::Unknown,
                                drawing_error: None,
                            }));
                        }
                        None => {
                            self.phase = Phase::AwaitingCommitUndoGroup;

                            return NativeExecStep::new(ExecStepKind::CommitUndoGroup);
                        }
                    }
                }
                Phase::EmitEvalValue => {
                    self.io.begin_value_output(ValueOutputKind::EvalValue);
                    self.phase = Phase::AwaitingEmitEvalValue;

                    return NativeExecStep::new(ExecStepKind::EmitEvalValue);
                }
                Phase::ClearRetainedEvalValue => {
                    self.phase = Phase::AwaitingClearRetainedEvalValue;

                    return NativeExecStep::new(ExecStepKind::ClearRetainedEvalValue);
                }
                Phase::CloseEmptyUndoGroup => {
                    self.phase = Phase::AwaitingCloseEmptyUndoGroup;

                    return NativeExecStep::new(ExecStepKind::CloseEmptyUndoGroup);
                }
                Phase::RollbackUndoGroup => {
                    self.phase = Phase::AwaitingRollbackUndoGroup;

                    return NativeExecStep::new(ExecStepKind::RollbackUndoGroup);
                }
                Phase::Terminal => {
                    self.phase = Phase::Done;

                    return NativeExecStep::new(ExecStepKind::Done);
                }

                Phase::AwaitingBeginUndoGroup
                | Phase::AwaitingEvaluateForm { .. }
                | Phase::AwaitingCommitUndoGroup
                | Phase::AwaitingEmitEvalValue
                | Phase::AwaitingClearRetainedEvalValue
                | Phase::AwaitingCloseEmptyUndoGroup
                | Phase::AwaitingRollbackUndoGroup
                | Phase::Done => return NativeExecStep::new(ExecStepKind::Invalid),
            }
        }
    }

    pub fn complete_step(&mut self, result: ExecStepResult) -> bool {
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
                    self.outcome = Some(ExecOutcome::Failure(failure));
                    self.phase = Phase::Terminal;
                } else {
                    self.unwind = None;
                    self.outcome = Some(ExecOutcome::Failure(ExecFailure::not_started(
                        result.into_message("could not begin the undo group"),
                    )));
                    self.phase = Phase::Terminal;
                }
            }

            Phase::AwaitingEvaluateForm {
                index,
                line,
                column,
            } => {
                let bridge_failure = self.io.close_value_output(ValueOutputKind::Form);

                if self.mode == ExecMode::Eval && result.succeeded() {
                    self.value_retained = true;
                }

                if result.primary_failed() {
                    self.begin_unwind(UnwindCause::Failure(ExecFailure {
                        message: result.into_message("form evaluation failed"),
                        form_index: Some(index),
                        location: Some(SourceLocation::new(self.source_name.clone(), line, column)),
                        drawing_outcome: DrawingOutcome::Unknown,
                        drawing_error: None,
                    }));
                } else if let Some(bridge_failure) = bridge_failure {
                    let mut message = bridge_failure.message().to_owned();

                    if let Some(cleanup) = result.bridge_symbols_clear_message() {
                        append_diagnostic(&mut message, &cleanup);
                    }

                    self.begin_unwind(UnwindCause::Failure(ExecFailure {
                        message,
                        form_index: Some(index),
                        location: Some(SourceLocation::new(self.source_name.clone(), line, column)),
                        drawing_outcome: DrawingOutcome::Unknown,
                        drawing_error: None,
                    }));
                } else if !result.succeeded() {
                    self.begin_unwind(UnwindCause::Failure(ExecFailure {
                        message: result.into_message("form evaluation failed"),
                        form_index: Some(index),
                        location: Some(SourceLocation::new(self.source_name.clone(), line, column)),
                        drawing_outcome: DrawingOutcome::Unknown,
                        drawing_error: None,
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
                    if self.mode == ExecMode::Eval && self.value_retained {
                        self.phase = Phase::EmitEvalValue;
                    } else if self.mode == ExecMode::Eval {
                        self.outcome = Some(ExecOutcome::Failure(self.eval_failure(
                            "the AutoLISP evaluator did not retain its result value".to_owned(),
                            DrawingOutcome::Committed,
                        )));
                        self.phase = Phase::Terminal;
                    } else {
                        self.outcome = Some(ExecOutcome::Success);
                        self.phase = Phase::Terminal;
                    }
                } else {
                    self.begin_unwind(UnwindCause::Failure(ExecFailure::unknown_drawing_outcome(
                        result.into_message("could not finish the undo group"),
                    )));
                }
            }
            Phase::AwaitingEmitEvalValue => {
                let bridge_failure = self.io.close_value_output(ValueOutputKind::EvalValue);
                let failure = if result.primary_failed() {
                    Some(result.into_message("could not emit the eval result"))
                } else if let Some(bridge_failure) = bridge_failure {
                    let mut message = bridge_failure.message().to_owned();

                    if let Some(cleanup) = result.bridge_symbols_clear_message() {
                        append_diagnostic(&mut message, &cleanup);
                    }

                    Some(message)
                } else if !result.succeeded() {
                    Some(result.into_message("could not emit the eval result"))
                } else {
                    None
                };

                if let Some(message) = failure {
                    self.outcome = Some(ExecOutcome::Failure(
                        self.eval_failure(message, DrawingOutcome::Committed),
                    ));
                } else {
                    self.value_retained = false;
                    self.outcome = Some(ExecOutcome::Success);
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
                    UnwindCause::Cancelled if result.succeeded() => ExecOutcome::Cancelled,
                    UnwindCause::Cancelled => {
                        ExecOutcome::Failure(ExecFailure::unknown_drawing_outcome(
                            result.into_message("could not close the cancelled undo group"),
                        ))
                    }
                    UnwindCause::Failure(failure) if result.succeeded() => {
                        ExecOutcome::Failure(failure)
                    }
                    UnwindCause::Failure(mut failure) => {
                        let cleanup = result.into_message("could not close the expired undo group");
                        append_diagnostic(&mut failure.message, &cleanup);
                        failure.drawing_outcome = DrawingOutcome::Unknown;
                        ExecOutcome::Failure(failure)
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

                        ExecOutcome::Failure(failure)
                    }
                    UnwindCause::Cancelled => {
                        if result.succeeded() {
                            ExecOutcome::Cancelled
                        } else {
                            ExecOutcome::Failure(ExecFailure::unknown_drawing_outcome(
                                result.into_message("drawing unwind failed"),
                            ))
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
        self.phase = if self.mode == ExecMode::Eval && self.form_handed_off {
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

    fn eval_failure(&self, message: String, drawing_outcome: DrawingOutcome) -> ExecFailure {
        ExecFailure {
            message: bounded_diagnostic(message),
            form_index: Some(1),
            location: self.eval_location.clone(),
            drawing_outcome,
            drawing_error: None,
        }
    }

    pub fn record_bridge_finalization_failure(&mut self, result: ExecStepResult) -> bool {
        let cleanup = result.into_message("the execution bridge could not be finalized safely");
        let Some(outcome) = self.outcome.take() else {
            return false;
        };

        self.outcome = Some(ExecOutcome::Failure(outcome.into_unknown_failure(cleanup)));
        true
    }

    pub fn abandon(&mut self, result: ExecStepResult) -> bool {
        let message = result.into_message("execution could not continue safely");
        let phase = self.phase;

        if matches!(phase, Phase::AwaitingEvaluateForm { .. }) {
            let _ = self.io.close_value_output(ValueOutputKind::Form);
        } else if phase == Phase::AwaitingEmitEvalValue {
            let _ = self.io.close_value_output(ValueOutputKind::EvalValue);
        }

        let existing = self.outcome.take().or_else(|| {
            self.unwind.take().map(|cause| match cause {
                UnwindCause::Failure(failure) => ExecOutcome::Failure(failure),
                UnwindCause::Cancelled => ExecOutcome::Cancelled,
            })
        });
        let failure = match existing {
            Some(existing) => existing.into_unknown_failure(message),
            None => {
                let (form_index, location) = match phase {
                    Phase::AwaitingEvaluateForm {
                        index,
                        line,
                        column,
                    } => (
                        Some(index),
                        Some(SourceLocation::new(self.source_name.clone(), line, column)),
                    ),
                    Phase::AwaitingEmitEvalValue => (Some(1), self.eval_location.clone()),
                    _ => (None, None),
                };

                ExecFailure {
                    message,
                    form_index,
                    location,
                    drawing_outcome: DrawingOutcome::Unknown,
                    drawing_error: None,
                }
            }
        };

        self.outcome = Some(ExecOutcome::Failure(failure));
        self.phase = if phase == Phase::Done {
            Phase::Done
        } else {
            Phase::Terminal
        };

        true
    }
}

impl NativeExecStep {
    pub fn invalid() -> Self {
        Self::new(ExecStepKind::Invalid)
    }

    fn new(kind: ExecStepKind) -> Self {
        Self {
            kind,
            source: None,
            span: None,
            retain_value: false,
        }
    }

    fn form(source: Bytes, span: FormSpan, retain_value: bool) -> Self {
        Self {
            kind: ExecStepKind::EvaluateForm,
            source: Some(source),
            span: Some(span),
            retain_value,
        }
    }

    pub const fn kind(&self) -> ExecStepKind {
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

impl ExecStepResult {
    fn succeeded(&self) -> bool {
        self.kind == ExecStepResultKind::Success && self.bridge_symbols_clear_status == 0
    }

    fn primary_failed(&self) -> bool {
        self.kind != ExecStepResultKind::Success
    }

    fn bridge_symbols_clear_message(&self) -> Option<String> {
        (self.bridge_symbols_clear_status != 0).then(|| {
            format!(
                "could not clear the reserved AutoLISP execution bridge symbols (native status {})",
                self.bridge_symbols_clear_status
            )
        })
    }

    fn into_message(self, fallback: &str) -> String {
        let cleanup = self.bridge_symbols_clear_message();
        let primary = if self.kind == ExecStepResultKind::Success {
            None
        } else if !self.detail.is_empty() {
            Some(self.detail)
        } else if self.kind == ExecStepResultKind::LispError && self.lisp_errno != 0 {
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
