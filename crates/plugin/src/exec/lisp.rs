use super::{ExecStepResult, ExecStepResultKind, bounded_native_diagnostic};
use crate::ffi::{
    NativeBridgeCleanupPlan as BridgeCleanupPlan, NativeBridgeStepResult as BridgeStepResult,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LispStatus {
    Unavailable,
    Nil,
    True,
    Other,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NativeDiagnostic {
    pub(crate) text: String,
    pub(crate) truncated: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LispObservation {
    pub(crate) command_status: i32,
    pub(crate) status: LispStatus,
    pub(crate) status_read_status: i32,
    pub(crate) errno: Option<i32>,
    pub(crate) error: Option<NativeDiagnostic>,
    pub(crate) malformed_status: i32,
}

pub(crate) fn interpret_lisp(
    observation: LispObservation,
    retain_value_on_success: bool,
) -> BridgeCleanupPlan {
    let result = if observation.command_status != 0 {
        native_error(observation.command_status)
    } else {
        match observation.status {
            LispStatus::Unavailable => native_error(nonzero_or(
                observation.status_read_status,
                observation.malformed_status,
            )),
            LispStatus::True => success(),
            LispStatus::Other => native_error(observation.malformed_status),
            LispStatus::Nil => ExecStepResult {
                kind: ExecStepResultKind::LispError,
                native_status: 0,
                lisp_errno: observation.errno.unwrap_or_default(),
                detail: observation
                    .error
                    .map(|error| bounded_native_diagnostic(error.text, error.truncated))
                    .unwrap_or_default(),
                bridge_symbols_clear_status: 0,
            },
        }
    };

    prepare_cleanup(result, retain_value_on_success)
}

pub(crate) fn prepare_cleanup(
    result: ExecStepResult,
    retain_value_on_success: bool,
) -> BridgeCleanupPlan {
    let retain_value = result.kind == ExecStepResultKind::Success && retain_value_on_success;

    BridgeCleanupPlan {
        result,
        retain_value,
    }
}

pub(crate) fn complete_cleanup(
    mut plan: BridgeCleanupPlan,
    cleanup_status: i32,
    fallback_cleanup_status: i32,
) -> BridgeStepResult {
    if cleanup_status == 0 {
        return BridgeStepResult {
            result: plan.result,
            bridge_symbols_may_be_retained: plan.retain_value,
        };
    }

    plan.result.bridge_symbols_clear_status = cleanup_status;

    BridgeStepResult {
        result: plan.result,
        bridge_symbols_may_be_retained: fallback_cleanup_status != 0,
    }
}

fn success() -> ExecStepResult {
    ExecStepResult {
        kind: ExecStepResultKind::Success,
        native_status: 0,
        lisp_errno: 0,
        detail: String::new(),
        bridge_symbols_clear_status: 0,
    }
}

fn native_error(status: i32) -> ExecStepResult {
    ExecStepResult {
        kind: ExecStepResultKind::NativeError,
        native_status: status,
        lisp_errno: 0,
        detail: String::new(),
        bridge_symbols_clear_status: 0,
    }
}

fn nonzero_or(status: i32, fallback: i32) -> i32 {
    if status == 0 { fallback } else { status }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(status: LispStatus) -> LispObservation {
        LispObservation {
            command_status: 0,
            status,
            status_read_status: 0,
            errno: None,
            error: None,
            malformed_status: -5001,
        }
    }

    #[test]
    fn true_is_the_only_status_that_retains_a_requested_value() {
        let success = interpret_lisp(observation(LispStatus::True), true);
        assert_eq!(success.result.kind, ExecStepResultKind::Success);
        assert!(success.retain_value);

        let malformed = interpret_lisp(observation(LispStatus::Other), true);
        assert_eq!(malformed.result.kind, ExecStepResultKind::NativeError);
        assert_eq!(malformed.result.native_status, -5001);
        assert!(!malformed.retain_value);
    }

    #[test]
    fn nil_preserves_lisp_evidence_and_explicit_native_truncation() {
        let mut value = observation(LispStatus::Nil);
        value.errno = Some(7);
        value.error = Some(NativeDiagnostic {
            text: "é".repeat(acadctl_rpc::MAX_DIAGNOSTIC_BYTES),
            truncated: true,
        });

        let plan = interpret_lisp(value, false);

        assert_eq!(plan.result.kind, ExecStepResultKind::LispError);
        assert_eq!(plan.result.lisp_errno, 7);
        assert!(plan.result.detail.ends_with("... [truncated]"));
        assert!(plan.result.detail.len() <= acadctl_rpc::MAX_DIAGNOSTIC_BYTES);
    }

    #[test]
    fn native_command_and_status_read_failures_win_before_lisp_fields() {
        let mut command = observation(LispStatus::Nil);
        command.command_status = -1;
        command.errno = Some(7);
        assert_eq!(interpret_lisp(command, false).result.native_status, -1);

        let mut unavailable = observation(LispStatus::Unavailable);
        unavailable.status_read_status = -2;
        assert_eq!(interpret_lisp(unavailable, false).result.native_status, -2);
    }

    #[test]
    fn cleanup_failure_reports_the_first_status_and_retained_state() {
        let cleared = complete_cleanup(prepare_cleanup(success(), true), -1, 0);
        assert_eq!(cleared.result.bridge_symbols_clear_status, -1);
        assert!(!cleared.bridge_symbols_may_be_retained);

        let retained = complete_cleanup(prepare_cleanup(success(), false), -1, -2);
        assert_eq!(retained.result.bridge_symbols_clear_status, -1);
        assert!(retained.bridge_symbols_may_be_retained);
    }
}
