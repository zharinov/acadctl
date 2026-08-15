#[cxx::bridge(namespace = "acadctl")]
mod ffi {
    struct NativeDocumentSnapshot {
        document_token: usize,
        database_token: usize,
        name: String,
        named: bool,
        modified: bool,
        read_only: bool,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeActionKind {
        None,
        Open,
        Save,
        Close,
        Undo,
        Redo,
        QueueExecutionDriver,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeActionResultKind {
        Success,
        DocumentGone,
        DocumentGenerationChanged,
        Unnamed,
        ReadOnly,
        Dirty,
        OpenFailed,
        LockFailed,
        SaveFailed,
        CloseFailed,
        HistoryFailed,
        NotQuiescent,
        UndoDisabled,
        DocumentContextFailed,
        DocumentContextRestoreFailed,
        ExecutionBridgeFinalizationFailed,
        ExecutionBridgeSymbolsClearFailed,
        ExecutionBridgeFailed,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeExecutionStepKind {
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
    enum NativeExecutionStepResultKind {
        Success,
        LispError,
        NativeError,
    }

    #[derive(Debug)]
    #[repr(u8)]
    enum NativeValueWriteResult {
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

    struct NativeAction {
        job_id: u64,
        kind: NativeActionKind,
        document_token: usize,
        database_token: usize,
        path: String,
        discard: bool,
    }

    struct NativeActionResult {
        kind: NativeActionResultKind,
        native_status: i32,
        native_detail: String,
    }

    struct NativeExecutionStepResult {
        kind: NativeExecutionStepResultKind,
        native_status: i32,
        lisp_errno: i32,
        detail: String,
        bridge_symbols_clear_status: i32,
    }

    struct NativeLispValueEvent {
        code: i32,
        payload_kind: NativeLispPayloadKind,
        integer: i64,
        real: f64,
        has_text: bool,
    }

    struct NativeBridgeProtocol {
        execution_driver_expression: String,
        execution_driver_invocation: String,
        begin_println_function: String,
        value_event_function: String,
        advance_execution_function: String,
        finish_println_function: String,
        source_symbol: String,
        staged_form_symbol: String,
        status_symbol: String,
        error_symbol: String,
        errno_symbol: String,
        value_symbol: String,
        pending_status: String,
        value_chunk_capture_units: usize,
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
        result: NativeExecutionStepResult,
        retain_value: bool,
    }

    struct NativeBridgeStepResult {
        result: NativeExecutionStepResult,
        bridge_symbols_may_be_retained: bool,
    }

    #[derive(Default)]
    struct NativeExecutionFinalizationObservation {
        undo_group_may_be_open: bool,
        bridge_symbols_may_be_retained: bool,
        staged_form_may_be_retained: bool,
        value_writer_active: bool,
        terminal_cleanup_failed: bool,
    }

    extern "Rust" {
        type NativeExecutionStep;
        type NativeValueWriter;

        fn start_rpc_server() -> String;

        fn publish_document_snapshot(documents: Vec<NativeDocumentSnapshot>);

        fn take_native_action() -> NativeAction;

        fn complete_native_action(job_id: u64, result: NativeActionResult);

        fn complete_execution_native_action(
            job_id: u64,
            result: NativeActionResult,
            observation: NativeExecutionFinalizationObservation,
        );

        fn try_claim_native_action_wake() -> bool;

        fn native_state_may_be_ready();

        fn native_action_wake_failed(status: i32);

        fn take_execution_step(job_id: u64) -> Box<NativeExecutionStep>;

        fn execution_step_kind(step: &NativeExecutionStep) -> NativeExecutionStepKind;

        fn execution_step_source(step: &NativeExecutionStep) -> &str;

        fn execution_step_retain_value(step: &NativeExecutionStep) -> bool;

        fn form_evaluator_source() -> &'static str;

        fn eval_value_visitor_source() -> &'static str;

        fn native_diagnostic_capture_units() -> usize;

        fn complete_execution_step(job_id: u64, result: NativeExecutionStepResult) -> bool;

        fn abandon_execution(job_id: u64, result: NativeExecutionStepResult) -> bool;

        fn native_bridge_protocol() -> NativeBridgeProtocol;

        fn interpret_lisp_observation(
            observation: NativeLispObservation,
            retain_value_on_success: bool,
        ) -> NativeBridgeCleanupPlan;

        fn prepare_bridge_cleanup(
            result: NativeExecutionStepResult,
            retain_value_on_success: bool,
        ) -> NativeBridgeCleanupPlan;

        fn complete_bridge_cleanup(
            plan: NativeBridgeCleanupPlan,
            cleanup_status: i32,
            fallback_cleanup_status: i32,
        ) -> NativeBridgeStepResult;

        fn begin_println(document_token: usize, database_token: usize) -> Box<NativeValueWriter>;

        fn begin_eval_value(
            job_id: u64,
            document_token: usize,
            database_token: usize,
        ) -> Box<NativeValueWriter>;

        fn value_writer_active(writer: &NativeValueWriter) -> bool;

        fn invalidate_value_writer(writer: &mut NativeValueWriter);

        fn write_lisp_value_event(
            writer: &mut NativeValueWriter,
            event: NativeLispValueEvent,
            text: &str,
        ) -> NativeValueWriteResult;

        fn finish_value_writer(writer: Box<NativeValueWriter>) -> NativeValueWriteResult;

        fn stop_rpc_server();
    }
}

#[path = "../bridge_protocol.rs"]
mod bridge_protocol;
mod documents;
mod execution;
mod rpc_server;
mod scheduler;

use execution::NativeExecutionStep;
use execution::native_bridge::{LispObservation, LispStatus, NativeDiagnostic};
use execution::value_bridge::{NativeValueWriter, ValueEvent, WriteResult};

fn start_rpc_server() -> String {
    rpc_server::start().err().unwrap_or_default()
}

fn publish_document_snapshot(documents: Vec<ffi::NativeDocumentSnapshot>) {
    scheduler::replace_document_snapshot(documents);
}

fn take_native_action() -> ffi::NativeAction {
    scheduler::take_native_action()
}

fn complete_native_action(job_id: u64, result: ffi::NativeActionResult) {
    scheduler::complete_native_action(job_id, result);
}

fn complete_execution_native_action(
    job_id: u64,
    result: ffi::NativeActionResult,
    observation: ffi::NativeExecutionFinalizationObservation,
) {
    scheduler::complete_execution_native_action(job_id, result, observation);
}

fn try_claim_native_action_wake() -> bool {
    scheduler::try_claim_native_action_wake()
}

fn native_state_may_be_ready() {
    scheduler::native_state_may_be_ready();
}

fn native_action_wake_failed(status: i32) {
    scheduler::wake_failed(status);
}

fn take_execution_step(job_id: u64) -> Box<NativeExecutionStep> {
    Box::new(scheduler::take_execution_step(job_id))
}

fn execution_step_kind(step: &NativeExecutionStep) -> ffi::NativeExecutionStepKind {
    step.kind()
}

fn execution_step_source(step: &NativeExecutionStep) -> &str {
    step.source()
}

fn execution_step_retain_value(step: &NativeExecutionStep) -> bool {
    step.retain_value()
}

fn form_evaluator_source() -> &'static str {
    execution::form_evaluator_source()
}

fn eval_value_visitor_source() -> &'static str {
    execution::visitor::source()
}

fn native_diagnostic_capture_units() -> usize {
    acadctl_rpc::MAX_DIAGNOSTIC_BYTES + 1
}

fn native_bridge_protocol() -> ffi::NativeBridgeProtocol {
    ffi::NativeBridgeProtocol {
        execution_driver_expression: bridge_protocol::execution_driver_expression(),
        execution_driver_invocation: bridge_protocol::execution_driver_invocation(),
        begin_println_function: bridge_protocol::BEGIN_PRINTLN_FUNCTION.into(),
        value_event_function: bridge_protocol::VALUE_EVENT_FUNCTION.into(),
        advance_execution_function: bridge_protocol::ADVANCE_EXECUTION_FUNCTION.into(),
        finish_println_function: bridge_protocol::FINISH_PRINTLN_FUNCTION.into(),
        source_symbol: bridge_protocol::SOURCE_SYMBOL.into(),
        staged_form_symbol: bridge_protocol::STAGED_FORM_SYMBOL.into(),
        status_symbol: bridge_protocol::STATUS_SYMBOL.into(),
        error_symbol: bridge_protocol::ERROR_SYMBOL.into(),
        errno_symbol: bridge_protocol::ERRNO_SYMBOL.into(),
        value_symbol: bridge_protocol::VALUE_SYMBOL.into(),
        pending_status: bridge_protocol::PENDING_STATUS.into(),
        value_chunk_capture_units: bridge_protocol::NATIVE_VALUE_CHUNK_CAPTURE_UNITS,
    }
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
    execution::native_bridge::interpret_lisp(
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
    result: ffi::NativeExecutionStepResult,
    retain_value_on_success: bool,
) -> ffi::NativeBridgeCleanupPlan {
    execution::native_bridge::prepare_cleanup(result, retain_value_on_success)
}

fn complete_bridge_cleanup(
    plan: ffi::NativeBridgeCleanupPlan,
    cleanup_status: i32,
    fallback_cleanup_status: i32,
) -> ffi::NativeBridgeStepResult {
    execution::native_bridge::complete_cleanup(plan, cleanup_status, fallback_cleanup_status)
}

fn complete_execution_step(job_id: u64, result: ffi::NativeExecutionStepResult) -> bool {
    scheduler::complete_execution_step(job_id, result)
}

fn abandon_execution(job_id: u64, result: ffi::NativeExecutionStepResult) -> bool {
    scheduler::abandon_execution(job_id, result)
}

fn begin_println(document_token: usize, database_token: usize) -> Box<NativeValueWriter> {
    Box::new(scheduler::begin_println(document_token, database_token))
}

fn begin_eval_value(
    job_id: u64,
    document_token: usize,
    database_token: usize,
) -> Box<NativeValueWriter> {
    Box::new(scheduler::begin_eval_value(
        job_id,
        document_token,
        database_token,
    ))
}

fn value_writer_active(writer: &NativeValueWriter) -> bool {
    writer.active()
}

fn invalidate_value_writer(writer: &mut NativeValueWriter) {
    writer.write(ValueEvent::Invalid);
}

fn write_lisp_value_event(
    writer: &mut NativeValueWriter,
    event: ffi::NativeLispValueEvent,
    text: &str,
) -> ffi::NativeValueWriteResult {
    use execution::visitor::Payload;

    let payload = match event.payload_kind {
        ffi::NativeLispPayloadKind::Nil => Payload::Nil,
        ffi::NativeLispPayloadKind::Integer => Payload::Integer(event.integer),
        ffi::NativeLispPayloadKind::Real => Payload::Real(event.real),
        ffi::NativeLispPayloadKind::String => Payload::String(text),
        ffi::NativeLispPayloadKind::Entity => Payload::Entity(event.has_text.then_some(text)),
        _ => Payload::Invalid,
    };

    let value = execution::visitor::value_event(event.code, payload);
    native_value_write_result(writer.write(value))
}

#[allow(
    clippy::boxed_local,
    reason = "CXX transfers exclusive ownership so finish can validate and close the writer"
)]
fn finish_value_writer(writer: Box<NativeValueWriter>) -> ffi::NativeValueWriteResult {
    native_value_write_result((*writer).finish())
}

fn native_value_write_result(result: WriteResult) -> ffi::NativeValueWriteResult {
    match result {
        WriteResult::Continue => ffi::NativeValueWriteResult::Continue,
        WriteResult::Inactive => ffi::NativeValueWriteResult::Inactive,
        WriteResult::Disconnected => ffi::NativeValueWriteResult::Disconnected,
        WriteResult::Cancelled => ffi::NativeValueWriteResult::Cancelled,
        WriteResult::Stopped => ffi::NativeValueWriteResult::Stopped,
        WriteResult::Finished => ffi::NativeValueWriteResult::Finished,
        WriteResult::InvalidSequence => ffi::NativeValueWriteResult::InvalidSequence,
        WriteResult::LimitExceeded => ffi::NativeValueWriteResult::LimitExceeded,
    }
}

fn stop_rpc_server() {
    rpc_server::stop();
}
