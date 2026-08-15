use acadctl_lisp::{FormSpan, ScanError};
use acadctl_rpc::SourceName;

use super::diagnostic::append_diagnostic;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecMode {
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
pub enum ExecOutcome {
    Success,
    Failure(ExecFailure),
    Cancelled,
}

impl ExecOutcome {
    pub(super) fn into_unknown_failure(self, message: String) -> ExecFailure {
        if let Self::Failure(mut failure) = self {
            append_diagnostic(&mut failure.message, &message);
            failure.drawing_outcome = DrawingOutcome::Unknown;
            failure
        } else {
            ExecFailure::unknown_drawing_outcome(message)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecFailure {
    pub message: String,
    pub form_index: Option<usize>,
    pub location: Option<SourceLocation>,
    pub drawing_outcome: DrawingOutcome,
}

impl ExecFailure {
    pub(crate) fn not_started(message: String) -> Self {
        Self {
            message,
            form_index: None,
            location: None,
            drawing_outcome: DrawingOutcome::NotStarted,
        }
    }

    pub(super) fn unknown_drawing_outcome(message: String) -> Self {
        Self {
            message,
            form_index: None,
            location: None,
            drawing_outcome: DrawingOutcome::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub source_name: SourceName,
    pub line: usize,
    pub column: usize,
}

impl SourceLocation {
    pub(crate) const fn new(source_name: SourceName, line: usize, column: usize) -> Self {
        Self {
            source_name,
            line,
            column,
        }
    }

    pub(crate) fn from_span(source_name: SourceName, span: &FormSpan) -> Self {
        Self::new(source_name, span.line, span.column)
    }

    pub(crate) fn from_scan_error(source_name: SourceName, error: &ScanError) -> Self {
        Self::new(source_name, error.line, error.column)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawingOutcome {
    NotStarted,
    RolledBack,
    Committed,
    Unknown,
}
