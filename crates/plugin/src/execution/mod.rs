use std::sync::Arc;

use acadctl_lisp::{FormSpan, ScanError, ScanPosition};
use output::{OutputSink, OutputStream};

pub mod output;
pub mod value;

#[allow(
    dead_code,
    reason = "request admission stays private until the native proof gates pass"
)]
pub const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
pub const EVALUATOR_SOURCE: &str = include_str!("../../lisp/acadctl.lsp");

pub struct Execution {
    #[allow(
        dead_code,
        reason = "the stored mode is consumed when public output is wired"
    )]
    mode: ExecutionMode,
    source_name: String,
    source: Arc<String>,
    next_scan: ScanPosition,
    next_form_index: usize,
    phase: Phase,
    form_attempted: bool,
    cancel_requested: bool,
    rollback: Option<RollbackCause>,
    outcome: Option<Outcome>,
    output: OutputSink,
}

#[allow(
    dead_code,
    reason = "request admission stays private until the native proof gates pass"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionMode {
    Eval,
    Exec,
}

#[allow(
    dead_code,
    reason = "request admission stays private until the native proof gates pass"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationError {
    SourceTooLarge,
    NullCharacter,
    ExpectedOneForm { actual: usize },
    Scan(ScanError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure(Failure),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
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
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    Invalid,
    Begin,
    Form,
    Commit,
    Abort,
    Rollback,
    Done,
}

pub struct NativeExecutionStep {
    kind: StepKind,
    source: Option<Arc<String>>,
    span: Option<FormSpan>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepResultKind {
    Success,
    LispError,
    NativeError,
}

pub struct StepResult {
    pub kind: StepResultKind,
    pub native_status: i32,
    pub lisp_errno: i32,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    #[allow(
        dead_code,
        reason = "request admission stays private until the native proof gates pass"
    )]
    Begin,
    AwaitingBegin,
    BetweenForms,
    AwaitingForm {
        index: usize,
        line: usize,
        column: usize,
    },
    AwaitingCommit,
    Abort,
    AwaitingAbort,
    Rollback,
    AwaitingRollback,
    Terminal,
    Done,
}

enum RollbackCause {
    Failure(Failure),
    Cancelled,
}

impl Execution {
    #[allow(
        dead_code,
        reason = "request admission stays private until the native proof gates pass"
    )]
    pub fn new(
        mode: ExecutionMode,
        source_name: String,
        mut source: String,
    ) -> Result<(Self, OutputStream), ValidationError> {
        if source.starts_with('\u{feff}') {
            source.drain(..'\u{feff}'.len_utf8());
        }
        if source.len() > MAX_SOURCE_BYTES {
            return Err(ValidationError::SourceTooLarge);
        }
        if source.contains('\0') {
            return Err(ValidationError::NullCharacter);
        }

        let form_count = acadctl_lisp::validate(&source).map_err(ValidationError::Scan)?;
        if mode == ExecutionMode::Eval && form_count != 1 {
            return Err(ValidationError::ExpectedOneForm { actual: form_count });
        }
        let next_scan = acadctl_lisp::scan(&source).position();
        let empty = form_count == 0;
        let (output, stream) = output::channel();
        if empty {
            output.finish();
        }
        Ok((
            Self {
                mode,
                source_name,
                source: Arc::new(source),
                next_scan,
                next_form_index: 1,
                phase: if empty { Phase::Terminal } else { Phase::Begin },
                form_attempted: false,
                cancel_requested: false,
                rollback: None,
                outcome: empty.then_some(Outcome::Success),
                output,
            },
            stream,
        ))
    }

    #[allow(
        dead_code,
        reason = "the stored mode is consumed when public output is wired"
    )]
    pub const fn mode(&self) -> ExecutionMode {
        self.mode
    }

    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    pub fn take_outcome(&mut self) -> Option<Outcome> {
        self.outcome.take()
    }

    pub fn output_sink(&self) -> OutputSink {
        self.output.clone()
    }

    pub fn request_cancel(&mut self) -> bool {
        if self.cancel_requested {
            return true;
        }
        if matches!(
            self.phase,
            Phase::AwaitingCommit
                | Phase::Abort
                | Phase::AwaitingAbort
                | Phase::Rollback
                | Phase::AwaitingRollback
                | Phase::Terminal
                | Phase::Done
        ) {
            return false;
        }
        self.cancel_requested = true;
        true
    }

    #[allow(
        dead_code,
        reason = "queued cancellation is private until the Execute RPC is exposed"
    )]
    pub fn cancel_before_start(&mut self) -> bool {
        if self.outcome.is_some() || !matches!(self.phase, Phase::Begin) {
            return false;
        }
        self.cancel_requested = true;
        self.outcome = Some(Outcome::Cancelled);
        self.phase = Phase::Terminal;
        true
    }

    pub fn take_step(&mut self) -> NativeExecutionStep {
        loop {
            match self.phase {
                Phase::Begin => {
                    if self.cancel_requested {
                        self.outcome = Some(Outcome::Cancelled);
                        self.phase = Phase::Terminal;
                        continue;
                    }
                    self.phase = Phase::AwaitingBegin;
                    return NativeExecutionStep::new(StepKind::Begin);
                }
                Phase::BetweenForms => {
                    if self.cancel_requested {
                        self.rollback = Some(RollbackCause::Cancelled);
                        self.phase = if self.form_attempted {
                            Phase::Rollback
                        } else {
                            Phase::Abort
                        };
                        continue;
                    }
                    let mut scanner = acadctl_lisp::Scanner::resume(&self.source, self.next_scan);
                    match scanner.next() {
                        Some(Ok(span)) => {
                            self.next_scan = scanner.position();
                            let index = self.next_form_index;
                            self.next_form_index += 1;
                            self.phase = Phase::AwaitingForm {
                                index,
                                line: span.line,
                                column: span.column,
                            };
                            self.form_attempted = true;
                            return NativeExecutionStep::form(Arc::clone(&self.source), span);
                        }
                        Some(Err(error)) => {
                            self.rollback = Some(RollbackCause::Failure(Failure {
                                message: error.kind.message().to_owned(),
                                form_index: None,
                                location: Some(SourceLocation {
                                    source_name: self.source_name.clone(),
                                    line: error.line,
                                    column: error.column,
                                }),
                                drawing_outcome: DrawingOutcome::Unknown,
                            }));
                            self.phase = Phase::Rollback;
                        }
                        None => {
                            self.phase = Phase::AwaitingCommit;
                            return NativeExecutionStep::new(StepKind::Commit);
                        }
                    }
                }
                Phase::Abort => {
                    self.phase = Phase::AwaitingAbort;
                    return NativeExecutionStep::new(StepKind::Abort);
                }
                Phase::Rollback => {
                    self.phase = Phase::AwaitingRollback;
                    return NativeExecutionStep::new(StepKind::Rollback);
                }
                Phase::Terminal => {
                    self.phase = Phase::Done;
                    return NativeExecutionStep::new(StepKind::Done);
                }
                Phase::AwaitingBegin
                | Phase::AwaitingForm { .. }
                | Phase::AwaitingCommit
                | Phase::AwaitingAbort
                | Phase::AwaitingRollback
                | Phase::Done => return NativeExecutionStep::new(StepKind::Invalid),
            }
        }
    }

    pub fn complete_step(&mut self, result: StepResult) -> bool {
        match self.phase {
            Phase::AwaitingBegin => {
                if result.kind == StepResultKind::Success {
                    if self.cancel_requested {
                        self.rollback = Some(RollbackCause::Cancelled);
                        self.phase = Phase::Abort;
                    } else {
                        self.phase = Phase::BetweenForms;
                    }
                } else {
                    self.outcome = Some(Outcome::Failure(Failure {
                        message: result.into_message("could not begin the undo group"),
                        form_index: None,
                        location: None,
                        drawing_outcome: DrawingOutcome::NotStarted,
                    }));
                    self.phase = Phase::Terminal;
                }
            }
            Phase::AwaitingForm {
                index,
                line,
                column,
            } => {
                if result.kind == StepResultKind::Success {
                    if self.cancel_requested {
                        self.rollback = Some(RollbackCause::Cancelled);
                        self.phase = Phase::Rollback;
                    } else {
                        self.phase = Phase::BetweenForms;
                    }
                } else {
                    self.rollback = Some(RollbackCause::Failure(Failure {
                        message: result.into_message("form evaluation failed"),
                        form_index: Some(index),
                        location: Some(SourceLocation {
                            source_name: self.source_name.clone(),
                            line,
                            column,
                        }),
                        drawing_outcome: DrawingOutcome::Unknown,
                    }));
                    self.phase = Phase::Rollback;
                }
            }
            Phase::AwaitingCommit => {
                if result.kind == StepResultKind::Success {
                    self.outcome = Some(Outcome::Success);
                    self.phase = Phase::Terminal;
                } else {
                    self.rollback = Some(RollbackCause::Failure(Failure {
                        message: result.into_message("could not finish the undo group"),
                        form_index: None,
                        location: None,
                        drawing_outcome: DrawingOutcome::Unknown,
                    }));
                    self.phase = Phase::Rollback;
                }
            }
            Phase::AwaitingAbort => {
                let Some(RollbackCause::Cancelled) = self.rollback.take() else {
                    return false;
                };
                if result.kind == StepResultKind::Success {
                    self.outcome = Some(Outcome::Cancelled);
                } else {
                    self.outcome = Some(Outcome::Failure(Failure {
                        message: result.into_message("could not close the cancelled undo group"),
                        form_index: None,
                        location: None,
                        drawing_outcome: DrawingOutcome::Unknown,
                    }));
                }
                self.phase = Phase::Terminal;
            }
            Phase::AwaitingRollback => {
                let Some(cause) = self.rollback.take() else {
                    return false;
                };
                self.outcome = Some(match cause {
                    RollbackCause::Failure(mut failure) => {
                        if result.kind == StepResultKind::Success {
                            failure.drawing_outcome = DrawingOutcome::RolledBack;
                        } else {
                            let rollback = result.into_message("drawing rollback failed");
                            failure.message = format!("{}; {rollback}", failure.message);
                            failure.drawing_outcome = DrawingOutcome::Unknown;
                        }
                        Outcome::Failure(failure)
                    }
                    RollbackCause::Cancelled => {
                        if result.kind == StepResultKind::Success {
                            Outcome::Cancelled
                        } else {
                            Outcome::Failure(Failure {
                                message: result.into_message("drawing rollback failed"),
                                form_index: None,
                                location: None,
                                drawing_outcome: DrawingOutcome::Unknown,
                            })
                        }
                    }
                });
                self.phase = Phase::Terminal;
            }
            Phase::Begin
            | Phase::BetweenForms
            | Phase::Abort
            | Phase::Rollback
            | Phase::Terminal
            | Phase::Done => return false,
        }
        true
    }

    pub fn record_terminal_failure(&mut self, result: StepResult) -> bool {
        let cleanup = result.into_message("the native execution lease could not be released");
        let Some(outcome) = self.outcome.take() else {
            return false;
        };
        let failure = match outcome {
            Outcome::Success => Failure {
                message: cleanup,
                form_index: None,
                location: None,
                drawing_outcome: DrawingOutcome::Unknown,
            },
            Outcome::Failure(mut failure) => {
                failure.message = format!("{}; {cleanup}", failure.message);
                failure.drawing_outcome = DrawingOutcome::Unknown;
                failure
            }
            Outcome::Cancelled => Failure {
                message: cleanup,
                form_index: None,
                location: None,
                drawing_outcome: DrawingOutcome::Unknown,
            },
        };
        self.outcome = Some(Outcome::Failure(failure));
        true
    }

    pub fn abandon(&mut self, result: StepResult) -> bool {
        let message = result.into_message("execution could not continue safely");
        let phase = self.phase;
        let existing = self.outcome.take().or_else(|| {
            self.rollback.take().map(|cause| match cause {
                RollbackCause::Failure(failure) => Outcome::Failure(failure),
                RollbackCause::Cancelled => Outcome::Cancelled,
            })
        });
        let failure = match existing {
            Some(Outcome::Success) => Failure {
                message,
                form_index: None,
                location: None,
                drawing_outcome: DrawingOutcome::Unknown,
            },
            Some(Outcome::Failure(mut failure)) => {
                failure.message = format!("{}; {message}", failure.message);
                failure.drawing_outcome = DrawingOutcome::Unknown;
                failure
            }
            Some(Outcome::Cancelled) => Failure {
                message,
                form_index: None,
                location: None,
                drawing_outcome: DrawingOutcome::Unknown,
            },
            None => {
                let (form_index, location) = match phase {
                    Phase::AwaitingForm {
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
                    _ => (None, None),
                };
                Failure {
                    message,
                    form_index,
                    location,
                    drawing_outcome: DrawingOutcome::Unknown,
                }
            }
        };
        self.outcome = Some(Outcome::Failure(failure));
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
        Self::new(StepKind::Invalid)
    }

    fn new(kind: StepKind) -> Self {
        Self {
            kind,
            source: None,
            span: None,
        }
    }

    fn form(source: Arc<String>, span: FormSpan) -> Self {
        Self {
            kind: StepKind::Form,
            source: Some(source),
            span: Some(span),
        }
    }

    pub const fn kind(&self) -> StepKind {
        self.kind
    }

    pub fn source(&self) -> &str {
        match (&self.source, &self.span) {
            (Some(source), Some(span)) => &source[span.byte_start..span.byte_end],
            _ => "",
        }
    }
}

impl StepResult {
    fn into_message(self, fallback: &str) -> String {
        if !self.detail.is_empty() {
            self.detail
        } else if self.kind == StepResultKind::LispError && self.lisp_errno != 0 {
            format!("{fallback} (ERRNO {})", self.lisp_errno)
        } else if self.native_status != 0 {
            format!("{fallback} (native status {})", self.native_status)
        } else {
            fallback.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_source_before_native_admission() {
        assert!(
            Execution::new(
                ExecutionMode::Exec,
                "<stdin>".into(),
                "x".repeat(MAX_SOURCE_BYTES),
            )
            .is_ok()
        );
        assert!(
            Execution::new(
                ExecutionMode::Exec,
                "<stdin>".into(),
                format!("\u{feff}{}", "x".repeat(MAX_SOURCE_BYTES)),
            )
            .is_ok()
        );
        assert_eq!(
            Execution::new(
                ExecutionMode::Exec,
                "<stdin>".into(),
                "x".repeat(MAX_SOURCE_BYTES + 1),
            )
            .err()
            .unwrap(),
            ValidationError::SourceTooLarge
        );
        assert_eq!(
            Execution::new(ExecutionMode::Exec, "<stdin>".into(), "x\0y".into())
                .err()
                .unwrap(),
            ValidationError::NullCharacter
        );
        assert!(matches!(
            Execution::new(ExecutionMode::Exec, "<stdin>".into(), "(unfinished".into(),),
            Err(ValidationError::Scan(_))
        ));
    }

    #[test]
    fn eval_requires_exactly_one_form_while_exec_accepts_a_batch() {
        assert_eq!(
            Execution::new(ExecutionMode::Eval, "<stdin>".into(), "".into())
                .err()
                .unwrap(),
            ValidationError::ExpectedOneForm { actual: 0 }
        );
        assert_eq!(
            Execution::new(ExecutionMode::Eval, "<stdin>".into(), "a b".into())
                .err()
                .unwrap(),
            ValidationError::ExpectedOneForm { actual: 2 }
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

        assert_eq!(execution.take_step().kind(), StepKind::Begin);
        assert!(execution.complete_step(success()));

        let first = execution.take_step();
        assert_eq!(first.kind(), StepKind::Form);
        assert_eq!(first.source(), "(setq x 1)");
        assert!(execution.complete_step(success()));

        let second = execution.take_step();
        assert_eq!(second.kind(), StepKind::Form);
        assert_eq!(second.source(), "(+ x 2)");
        assert!(execution.complete_step(success()));

        assert_eq!(execution.take_step().kind(), StepKind::Commit);
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Done);
        assert_eq!(execution.outcome(), Some(&Outcome::Success));
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

        assert_eq!(execution.take_step().kind(), StepKind::Rollback);
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Done);
        assert_eq!(
            execution.outcome(),
            Some(&Outcome::Failure(Failure {
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
        assert_eq!(execution.take_step().kind(), StepKind::Form);
        assert!(execution.complete_step(lisp_error("boom", 0)));
        assert_eq!(execution.take_step().kind(), StepKind::Rollback);
        assert!(execution.complete_step(native_error("U failed", -5001)));
        assert_eq!(execution.take_step().kind(), StepKind::Done);

        let Some(Outcome::Failure(failure)) = execution.outcome() else {
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
        assert_eq!(execution.take_step().kind(), StepKind::Form);
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Commit);
        assert!(execution.complete_step(native_error("End failed", -5001)));
        assert_eq!(execution.take_step().kind(), StepKind::Rollback);
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Done);

        assert_eq!(
            execution.outcome(),
            Some(&Outcome::Failure(Failure {
                message: "End failed".into(),
                form_index: None,
                location: None,
                drawing_outcome: DrawingOutcome::RolledBack,
            }))
        );
    }

    #[test]
    fn begin_failure_never_claims_that_drawing_work_started() {
        let mut execution = Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "ok".into())
            .unwrap()
            .0;
        assert_eq!(execution.take_step().kind(), StepKind::Begin);
        assert!(execution.complete_step(native_error("Begin failed", -5001)));
        assert_eq!(execution.take_step().kind(), StepKind::Done);

        assert_eq!(
            execution.outcome(),
            Some(&Outcome::Failure(Failure {
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
            Some(&Outcome::Failure(Failure {
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
        assert_eq!(execution.take_step().kind(), StepKind::Form);
        assert!(execution.complete_step(lisp_error("boom", 0)));
        assert_eq!(execution.take_step().kind(), StepKind::Rollback);
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Done);

        assert!(execution.record_terminal_failure(native_error("restore failed", 43)));
        let Some(Outcome::Failure(failure)) = execution.outcome() else {
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
        assert_eq!(execution.take_step().kind(), StepKind::Done);
        assert_eq!(
            execution.outcome(),
            Some(&Outcome::Failure(Failure {
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
        assert_eq!(execution.take_step().kind(), StepKind::Form);
        assert!(execution.complete_step(lisp_error("boom", 0)));
        assert_eq!(execution.take_step().kind(), StepKind::Rollback);

        assert!(execution.abandon(native_error("database replaced", -5001)));
        assert_eq!(execution.take_step().kind(), StepKind::Done);
        let Some(Outcome::Failure(failure)) = execution.outcome() else {
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
        assert_eq!(execution.take_step().kind(), StepKind::Done);
        assert_eq!(execution.outcome(), Some(&Outcome::Cancelled));
    }

    #[test]
    fn cancellation_during_begin_closes_the_empty_group_without_u() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();

        assert_eq!(execution.take_step().kind(), StepKind::Begin);
        assert!(execution.request_cancel());
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Abort);
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Done);
        assert_eq!(execution.outcome(), Some(&Outcome::Cancelled));
    }

    #[test]
    fn cancellation_after_a_form_uses_rollback() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        begin(&mut execution);

        assert_eq!(execution.take_step().kind(), StepKind::Form);
        assert!(execution.request_cancel());
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Rollback);
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Done);
        assert_eq!(execution.outcome(), Some(&Outcome::Cancelled));
    }

    #[test]
    fn evaluator_failure_wins_over_concurrent_cancellation() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "bad".into()).unwrap();
        begin(&mut execution);

        assert_eq!(execution.take_step().kind(), StepKind::Form);
        assert!(execution.request_cancel());
        assert!(execution.complete_step(lisp_error("boom", 0)));
        assert_eq!(execution.take_step().kind(), StepKind::Rollback);
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Done);
        let Some(Outcome::Failure(failure)) = execution.outcome() else {
            panic!("expected the evaluator failure");
        };
        assert_eq!(failure.message, "boom");
        assert_eq!(failure.drawing_outcome, DrawingOutcome::RolledBack);
    }

    #[test]
    fn cancellation_after_commit_handoff_is_too_late() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        begin(&mut execution);
        assert_eq!(execution.take_step().kind(), StepKind::Form);
        assert!(execution.complete_step(success()));

        assert_eq!(execution.take_step().kind(), StepKind::Commit);
        assert!(!execution.request_cancel());
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Done);
        assert_eq!(execution.outcome(), Some(&Outcome::Success));
    }

    #[test]
    fn rollback_failure_overrides_cancellation() {
        let (mut execution, _output) =
            Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "form".into()).unwrap();
        begin(&mut execution);
        assert_eq!(execution.take_step().kind(), StepKind::Form);
        assert!(execution.request_cancel());
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Rollback);
        assert!(execution.complete_step(native_error("U failed", -5001)));
        assert_eq!(execution.take_step().kind(), StepKind::Done);

        assert_eq!(
            execution.outcome(),
            Some(&Outcome::Failure(Failure {
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

        assert_eq!(execution.take_step().kind(), StepKind::Done);
        assert_eq!(execution.outcome(), Some(&Outcome::Success));
    }

    fn begin(execution: &mut Execution) {
        assert_eq!(execution.take_step().kind(), StepKind::Begin);
        assert!(execution.complete_step(success()));
    }

    fn successful_execution() -> Execution {
        let mut execution = Execution::new(ExecutionMode::Exec, "batch.lsp".into(), "ok".into())
            .unwrap()
            .0;
        begin(&mut execution);
        assert_eq!(execution.take_step().kind(), StepKind::Form);
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Commit);
        assert!(execution.complete_step(success()));
        assert_eq!(execution.take_step().kind(), StepKind::Done);
        execution
    }

    fn success() -> StepResult {
        StepResult {
            kind: StepResultKind::Success,
            native_status: 0,
            lisp_errno: 0,
            detail: String::new(),
        }
    }

    fn lisp_error(detail: &str, lisp_errno: i32) -> StepResult {
        StepResult {
            kind: StepResultKind::LispError,
            native_status: 0,
            lisp_errno,
            detail: detail.into(),
        }
    }

    fn native_error(detail: &str, native_status: i32) -> StepResult {
        StepResult {
            kind: StepResultKind::NativeError,
            native_status,
            lisp_errno: 0,
            detail: detail.into(),
        }
    }
}
