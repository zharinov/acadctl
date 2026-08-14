use std::sync::Arc;

use acadctl_lisp::{FormSpan, ScanError, ScanPosition};

pub mod output;

#[allow(
    dead_code,
    reason = "request admission stays private until the native proof gates pass"
)]
pub const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
pub const EVALUATOR_SOURCE: &str = include_str!("../../lisp/acadctl.lsp");

pub struct Execution {
    source_name: String,
    source: Arc<String>,
    next_scan: ScanPosition,
    next_form_index: usize,
    phase: Phase,
    failure: Option<Failure>,
    outcome: Option<Outcome>,
}

#[allow(
    dead_code,
    reason = "request admission stays private until the native proof gates pass"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationError {
    SourceTooLarge,
    NullCharacter,
    Scan(ScanError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure(Failure),
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
    Rollback,
    AwaitingRollback,
    Terminal,
    Done,
}

impl Execution {
    #[allow(
        dead_code,
        reason = "request admission stays private until the native proof gates pass"
    )]
    pub fn new(source_name: String, mut source: String) -> Result<Self, ValidationError> {
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
        let next_scan = acadctl_lisp::scan(&source).position();
        let empty = form_count == 0;
        Ok(Self {
            source_name,
            source: Arc::new(source),
            next_scan,
            next_form_index: 1,
            phase: if empty { Phase::Terminal } else { Phase::Begin },
            failure: None,
            outcome: empty.then_some(Outcome::Success),
        })
    }

    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    pub fn take_outcome(&mut self) -> Option<Outcome> {
        self.outcome.take()
    }

    pub fn take_step(&mut self) -> NativeExecutionStep {
        loop {
            match self.phase {
                Phase::Begin => {
                    self.phase = Phase::AwaitingBegin;
                    return NativeExecutionStep::new(StepKind::Begin);
                }
                Phase::BetweenForms => {
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
                            return NativeExecutionStep::form(Arc::clone(&self.source), span);
                        }
                        Some(Err(error)) => {
                            self.failure = Some(Failure {
                                message: error.kind.message().to_owned(),
                                form_index: None,
                                location: Some(SourceLocation {
                                    source_name: self.source_name.clone(),
                                    line: error.line,
                                    column: error.column,
                                }),
                                drawing_outcome: DrawingOutcome::Unknown,
                            });
                            self.phase = Phase::Rollback;
                        }
                        None => {
                            self.phase = Phase::AwaitingCommit;
                            return NativeExecutionStep::new(StepKind::Commit);
                        }
                    }
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
                | Phase::AwaitingRollback
                | Phase::Done => return NativeExecutionStep::new(StepKind::Invalid),
            }
        }
    }

    pub fn complete_step(&mut self, result: StepResult) -> bool {
        match self.phase {
            Phase::AwaitingBegin => {
                if result.kind == StepResultKind::Success {
                    self.phase = Phase::BetweenForms;
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
                    self.phase = Phase::BetweenForms;
                } else {
                    self.failure = Some(Failure {
                        message: result.into_message("form evaluation failed"),
                        form_index: Some(index),
                        location: Some(SourceLocation {
                            source_name: self.source_name.clone(),
                            line,
                            column,
                        }),
                        drawing_outcome: DrawingOutcome::Unknown,
                    });
                    self.phase = Phase::Rollback;
                }
            }
            Phase::AwaitingCommit => {
                if result.kind == StepResultKind::Success {
                    self.outcome = Some(Outcome::Success);
                    self.phase = Phase::Terminal;
                } else {
                    self.failure = Some(Failure {
                        message: result.into_message("could not finish the undo group"),
                        form_index: None,
                        location: None,
                        drawing_outcome: DrawingOutcome::Unknown,
                    });
                    self.phase = Phase::Rollback;
                }
            }
            Phase::AwaitingRollback => {
                let Some(mut failure) = self.failure.take() else {
                    return false;
                };
                if result.kind == StepResultKind::Success {
                    failure.drawing_outcome = DrawingOutcome::RolledBack;
                } else {
                    let rollback = result.into_message("drawing rollback failed");
                    failure.message = format!("{}; {rollback}", failure.message);
                    failure.drawing_outcome = DrawingOutcome::Unknown;
                }
                self.outcome = Some(Outcome::Failure(failure));
                self.phase = Phase::Terminal;
            }
            Phase::Begin
            | Phase::BetweenForms
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
        };
        self.outcome = Some(Outcome::Failure(failure));
        true
    }

    pub fn abandon(&mut self, result: StepResult) -> bool {
        let message = result.into_message("execution could not continue safely");
        let phase = self.phase;
        let existing = self
            .outcome
            .take()
            .or_else(|| self.failure.take().map(Outcome::Failure));
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
        assert!(Execution::new("<stdin>".into(), "x".repeat(MAX_SOURCE_BYTES)).is_ok());
        assert!(
            Execution::new(
                "<stdin>".into(),
                format!("\u{feff}{}", "x".repeat(MAX_SOURCE_BYTES)),
            )
            .is_ok()
        );
        assert_eq!(
            Execution::new("<stdin>".into(), "x".repeat(MAX_SOURCE_BYTES + 1))
                .err()
                .unwrap(),
            ValidationError::SourceTooLarge
        );
        assert_eq!(
            Execution::new("<stdin>".into(), "x\0y".into())
                .err()
                .unwrap(),
            ValidationError::NullCharacter
        );
        assert!(matches!(
            Execution::new("<stdin>".into(), "(unfinished".into()),
            Err(ValidationError::Scan(_))
        ));
    }

    #[test]
    fn embedded_evaluator_is_one_complete_form() {
        assert_eq!(acadctl_lisp::validate(EVALUATOR_SOURCE), Ok(1));
    }

    #[test]
    fn yields_exact_forms_then_commits() {
        let mut execution = Execution::new(
            "batch.lsp".into(),
            "(setq x 1) ; keep with separator\n(+ x 2)".into(),
        )
        .unwrap();

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
        let mut execution = Execution::new("batch.lsp".into(), "ok\n  bad".into()).unwrap();
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
        let mut execution = Execution::new("batch.lsp".into(), "bad".into()).unwrap();
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
        let mut execution = Execution::new("batch.lsp".into(), "ok".into()).unwrap();
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
        let mut execution = Execution::new("batch.lsp".into(), "ok".into()).unwrap();
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
        let mut execution = Execution::new("batch.lsp".into(), "bad".into()).unwrap();
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
        let mut execution = Execution::new("batch.lsp".into(), "ok\nchanged".into()).unwrap();
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
        let mut execution = Execution::new("batch.lsp".into(), "bad".into()).unwrap();
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
    fn empty_batch_finishes_without_an_undo_group() {
        let mut execution = Execution::new("<stdin>".into(), "; only a comment".into()).unwrap();

        assert_eq!(execution.take_step().kind(), StepKind::Done);
        assert_eq!(execution.outcome(), Some(&Outcome::Success));
    }

    fn begin(execution: &mut Execution) {
        assert_eq!(execution.take_step().kind(), StepKind::Begin);
        assert!(execution.complete_step(success()));
    }

    fn successful_execution() -> Execution {
        let mut execution = Execution::new("batch.lsp".into(), "ok".into()).unwrap();
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
