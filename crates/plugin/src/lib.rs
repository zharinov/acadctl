#[cxx::bridge(namespace = "acadctl")]
mod ffi {
    struct NativeDocumentSnapshot {
        document_token: usize,
        database_token: usize,
        name: String,
        named: bool,
        modified: bool,
        read_only: bool,
        active: bool,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeActionKind {
        None,
        Open,
        Switch,
        Save,
        Close,
        Capture,
        Undo,
        Redo,
        QueueExecDriver,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeActionResultKind {
        Success,
        DrawingGone,
        DrawingGenerationChanged,
        Unnamed,
        ReadOnly,
        NotActive,
        Dirty,
        DestinationExists,
        OpenFailed,
        SwitchFailed,
        SaveFailed,
        CloseFailed,
        HistoryFailed,
        NotQuiescent,
        UndoDisabled,
        DocumentContextFailed,
        DocumentContextRestoreFailed,
        ExecBridgeFinalizationFailed,
        ExecBridgeSymbolsClearFailed,
        ExecBridgeFailed,
        CaptureUnavailable,
        CaptureInvalid,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeCaptureResultKind {
        Success,
        DrawingGone,
        DrawingGenerationChanged,
        NotActive,
        NotQuiescent,
        Unavailable,
        Invalid,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativePixelFormat {
        Invalid,
        Bgra8,
        Bgrx8,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeRowOrder {
        Invalid,
        TopDown,
        BottomUp,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeExecStepKind {
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

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeExecStepResultKind {
        Success,
        LispError,
        NativeError,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeOutputWriteResult {
        Continue,
        Inactive,
        Disconnected,
        Cancelled,
        Stopped,
        Finished,
        InvalidSequence,
        LimitExceeded,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeLispPayloadKind {
        Invalid,
        Nil,
        Integer,
        Real,
        String,
        Entity,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeLispStatusKind {
        Unavailable,
        Nil,
        True,
        Other,
    }

    struct NativeActionResult {
        kind: NativeActionResultKind,
        native_status: i32,
        native_detail: String,
    }

    struct NativeCaptureResult {
        kind: NativeCaptureResultKind,
        width: u32,
        height: u32,
        stride: usize,
        pixel_format: NativePixelFormat,
        row_order: NativeRowOrder,
        realistic_style: bool,
        detail: String,
    }

    struct NativeExecStepResult {
        kind: NativeExecStepResultKind,
        native_status: i32,
        lisp_errno: i32,
        detail: String,
        bridge_symbols_clear_status: i32,
    }

    struct NativeLispOutputEvent {
        code: i32,
        payload_kind: NativeLispPayloadKind,
        integer: i64,
        real: f64,
        has_text: bool,
    }

    struct NativeLispObservation {
        command_status: i32,
        status_kind: NativeLispStatusKind,
        status_read_status: i32,
        errno_available: bool,
        lisp_errno: i32,
        error_available: bool,
        error_text: String,
        error_text_truncated: bool,
        malformed_status: i32,
    }

    struct NativeBridgeCleanupPlan {
        result: NativeExecStepResult,
        retain_value: bool,
    }

    struct NativeBridgeStepResult {
        result: NativeExecStepResult,
        bridge_symbols_may_be_retained: bool,
    }

    #[derive(Default)]
    struct NativeExecFinalizationObservation {
        undo_group_may_be_open: bool,
        bridge_symbols_may_be_retained: bool,
        staged_form_may_be_retained: bool,
        output_port_active: bool,
        terminal_cleanup_failed: bool,
    }

    extern "Rust" {
        type NativeAction;
        type NativeExecStep;
        type NativeOutputPort;

        fn start_rpc_server() -> String;

        fn publish_document_snapshot(drawings: Vec<NativeDocumentSnapshot>);

        fn take_native_action() -> Box<NativeAction>;

        fn job_id(self: &NativeAction) -> u64;

        fn kind(self: &NativeAction) -> NativeActionKind;

        fn document_token(self: &NativeAction) -> usize;

        fn database_token(self: &NativeAction) -> usize;

        fn open_path(self: &NativeAction) -> &str;

        fn save_path(self: &NativeAction) -> &str;

        fn force_document_context(self: &NativeAction) -> bool;

        fn close_discard(self: &NativeAction) -> bool;

        fn complete_native_action(job_id: u64, result: NativeActionResult);

        fn complete_native_capture(job_id: u64, result: NativeCaptureResult, pixels: &[u8]);

        fn complete_execution_native_action(
            job_id: u64,
            result: NativeActionResult,
            observation: NativeExecFinalizationObservation,
        );

        fn try_claim_native_action_wake() -> bool;

        fn native_state_may_be_ready();

        fn native_action_wake_failed();

        fn take_execution_step(job_id: u64) -> Box<NativeExecStep>;

        fn execution_step_kind(step: &NativeExecStep) -> NativeExecStepKind;

        fn execution_step_source(step: &NativeExecStep) -> &str;

        fn execution_step_retain_value(step: &NativeExecStep) -> bool;

        fn native_diagnostic_capture_units() -> usize;

        fn complete_execution_step(job_id: u64, result: NativeExecStepResult) -> bool;

        fn abandon_execution(job_id: u64, result: NativeExecStepResult) -> bool;

        fn interpret_lisp_observation(
            observation: NativeLispObservation,
            retain_value_on_success: bool,
        ) -> NativeBridgeCleanupPlan;

        fn prepare_bridge_cleanup(
            result: NativeExecStepResult,
            retain_value_on_success: bool,
        ) -> NativeBridgeCleanupPlan;

        fn complete_bridge_cleanup(
            plan: NativeBridgeCleanupPlan,
            cleanup_status: i32,
            fallback_cleanup_status: i32,
        ) -> NativeBridgeStepResult;

        fn begin_eval_output(
            job_id: u64,
            document_token: usize,
            database_token: usize,
        ) -> Box<NativeOutputPort>;

        fn begin_form_output(
            job_id: u64,
            document_token: usize,
            database_token: usize,
        ) -> Box<NativeOutputPort>;

        fn output_port_claimed(port: &NativeOutputPort) -> bool;

        fn invalidate_output_port(port: &mut NativeOutputPort);

        fn write_lisp_output_event(
            port: &mut NativeOutputPort,
            event: NativeLispOutputEvent,
            text: &str,
        ) -> NativeOutputWriteResult;

        fn finish_output_port(port: Box<NativeOutputPort>) -> NativeOutputWriteResult;

        fn stop_rpc_server();
    }
}

mod drawing;
mod exec;
mod rpc;
mod scheduler;
pub(crate) mod screenshot;

use exec::NativeExecStep;
use exec::lisp::{LispObservation, LispStatus, NativeDiagnostic};
use exec::value::port::NativeOutputPort;
use scheduler::NativeAction;

fn start_rpc_server() -> String {
    rpc::start().err().unwrap_or_default()
}

fn publish_document_snapshot(drawings: Vec<ffi::NativeDocumentSnapshot>) {
    scheduler::replace_drawing_snapshot(drawings);
}

fn take_native_action() -> Box<NativeAction> {
    Box::new(scheduler::take_native_action())
}

fn complete_native_action(job_id: u64, result: ffi::NativeActionResult) {
    scheduler::complete_native_action(job_id, result);
}

fn complete_native_capture(job_id: u64, result: ffi::NativeCaptureResult, pixels: &[u8]) {
    scheduler::complete_native_capture(job_id, result, pixels);
}

fn complete_execution_native_action(
    job_id: u64,
    result: ffi::NativeActionResult,
    observation: ffi::NativeExecFinalizationObservation,
) {
    scheduler::complete_execution_native_action(job_id, result, observation);
}

fn try_claim_native_action_wake() -> bool {
    scheduler::try_claim_native_action_wake()
}

fn native_state_may_be_ready() {
    scheduler::native_state_may_be_ready();
}

fn native_action_wake_failed() {
    scheduler::wake_failed();
}

fn take_execution_step(job_id: u64) -> Box<NativeExecStep> {
    Box::new(scheduler::take_execution_step(job_id))
}

fn execution_step_kind(step: &NativeExecStep) -> ffi::NativeExecStepKind {
    step.kind()
}

fn execution_step_source(step: &NativeExecStep) -> &str {
    step.source()
}

fn execution_step_retain_value(step: &NativeExecStep) -> bool {
    step.retain_value()
}

fn native_diagnostic_capture_units() -> usize {
    acadctl_rpc::MAX_DIAGNOSTIC_BYTES + 1
}

fn interpret_lisp_observation(
    observation: ffi::NativeLispObservation,
    retain_value_on_success: bool,
) -> ffi::NativeBridgeCleanupPlan {
    let status = match observation.status_kind {
        ffi::NativeLispStatusKind::Nil => LispStatus::Nil,
        ffi::NativeLispStatusKind::True => LispStatus::True,
        ffi::NativeLispStatusKind::Other => LispStatus::Other,
        _ => LispStatus::Unavailable,
    };
    let error = observation.error_available.then_some(NativeDiagnostic {
        text: observation.error_text,
        truncated: observation.error_text_truncated,
    });
    exec::lisp::interpret_lisp(
        LispObservation {
            command_status: observation.command_status,
            status,
            status_read_status: observation.status_read_status,
            errno: observation
                .errno_available
                .then_some(observation.lisp_errno),
            error,
            malformed_status: observation.malformed_status,
        },
        retain_value_on_success,
    )
}

fn prepare_bridge_cleanup(
    result: ffi::NativeExecStepResult,
    retain_value_on_success: bool,
) -> ffi::NativeBridgeCleanupPlan {
    exec::lisp::prepare_cleanup(result, retain_value_on_success)
}

fn complete_bridge_cleanup(
    plan: ffi::NativeBridgeCleanupPlan,
    cleanup_status: i32,
    fallback_cleanup_status: i32,
) -> ffi::NativeBridgeStepResult {
    exec::lisp::complete_cleanup(plan, cleanup_status, fallback_cleanup_status)
}

fn complete_execution_step(job_id: u64, result: ffi::NativeExecStepResult) -> bool {
    scheduler::complete_execution_step(job_id, result)
}

fn abandon_execution(job_id: u64, result: ffi::NativeExecStepResult) -> bool {
    scheduler::abandon_execution(job_id, result)
}

fn begin_eval_output(
    job_id: u64,
    document_token: usize,
    database_token: usize,
) -> Box<NativeOutputPort> {
    Box::new(scheduler::begin_eval_output(
        job_id,
        document_token,
        database_token,
    ))
}

fn begin_form_output(
    job_id: u64,
    document_token: usize,
    database_token: usize,
) -> Box<NativeOutputPort> {
    Box::new(scheduler::begin_form_output(
        job_id,
        document_token,
        database_token,
    ))
}

fn output_port_claimed(port: &NativeOutputPort) -> bool {
    port.claimed()
}

fn invalidate_output_port(port: &mut NativeOutputPort) {
    port.invalidate();
}

fn write_lisp_output_event(
    port: &mut NativeOutputPort,
    event: ffi::NativeLispOutputEvent,
    text: &str,
) -> ffi::NativeOutputWriteResult {
    use exec::value::event::{Payload, ProtocolViolation};

    let payload = match event.payload_kind {
        ffi::NativeLispPayloadKind::Nil => Ok(Payload::Nil),
        ffi::NativeLispPayloadKind::Integer => Ok(Payload::Integer(event.integer)),
        ffi::NativeLispPayloadKind::Real => Ok(Payload::Real(event.real)),
        ffi::NativeLispPayloadKind::String => Ok(Payload::String(text)),
        ffi::NativeLispPayloadKind::Entity => Ok(Payload::Entity(event.has_text.then_some(text))),
        _ => Err(ProtocolViolation),
    };

    port.write(payload.and_then(|payload| exec::value::event::output_event(event.code, payload)))
}

#[allow(
    clippy::boxed_local,
    reason = "CXX transfers exclusive ownership so finish can validate and close the port"
)]
fn finish_output_port(port: Box<NativeOutputPort>) -> ffi::NativeOutputWriteResult {
    (*port).finish()
}

fn stop_rpc_server() {
    rpc::stop();
}
