use std::sync::LazyLock;

pub(crate) use crate::ffi::{
    NativeExecStepKind as ExecStepKind, NativeExecStepResult as ExecStepResult,
    NativeExecStepResultKind as ExecStepResultKind,
};

mod diagnostic;
mod io;
pub(crate) mod lisp;
mod outcome;
pub mod output;
pub(crate) mod protocol;
mod state;
pub mod value;

pub(crate) use diagnostic::{bound_diagnostic, bounded_diagnostic, bounded_native_diagnostic};
#[cfg(test)]
pub(crate) use io::{ExecIo, ValueBridgeState};
pub(crate) use io::{ValueBridgeFailure, ValueOutputLease};
pub use outcome::{
    DrawingOutcome, ExecFailure, ExecMode, ExecOutcome, SourceLocation, SourceValidationError,
};
pub use state::{Exec, NativeExecStep};

static FORM_EVALUATOR_SOURCE: LazyLock<String> = LazyLock::new(protocol::form_evaluator_source);

pub(crate) fn form_evaluator_source() -> &'static str {
    &FORM_EVALUATOR_SOURCE
}

#[cfg(test)]
mod tests;
