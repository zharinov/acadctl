use acadctl_rpc::DrawingPath;

use crate::drawing::{DrawingRegistry, NativeDocumentKey};
use crate::exec::{ExecOutcome, ExecStepResult};
use crate::ffi::{
    NativeActionKind, NativeActionResult, NativeActionResultKind, NativeExecFinalizationObservation,
};

use super::error::{Error, NativeFailure};
use super::operation::{Operation, OperationOutcome};

pub(super) enum NativeCommand {
    Open(DrawingPath),
    Drawing {
        target: NativeDocumentKey,
        operation: NativeDrawingOperation,
    },
}

pub(super) enum NativeDrawingOperation {
    Save,
    Close { discard: bool },
    Undo,
    Redo,
    QueueExecDriver,
}

pub(crate) struct NativeAction {
    state: NativeActionState,
}

enum NativeActionState {
    Idle,
    Issued { job_id: u64, command: NativeCommand },
}

impl NativeCommand {
    pub(super) fn open(path: DrawingPath) -> Self {
        Self::Open(path)
    }

    pub(super) fn save(target: NativeDocumentKey) -> Self {
        Self::drawing(target, NativeDrawingOperation::Save)
    }

    pub(super) fn close(target: NativeDocumentKey, discard: bool) -> Self {
        Self::drawing(target, NativeDrawingOperation::Close { discard })
    }

    pub(super) fn undo(target: NativeDocumentKey) -> Self {
        Self::drawing(target, NativeDrawingOperation::Undo)
    }

    pub(super) fn redo(target: NativeDocumentKey) -> Self {
        Self::drawing(target, NativeDrawingOperation::Redo)
    }

    pub(super) fn queue_exec_driver(target: NativeDocumentKey) -> Self {
        Self::drawing(target, NativeDrawingOperation::QueueExecDriver)
    }

    fn drawing(target: NativeDocumentKey, operation: NativeDrawingOperation) -> Self {
        Self::Drawing { target, operation }
    }

    pub(super) fn target(&self) -> Option<NativeDocumentKey> {
        match self {
            Self::Open(_) => None,
            Self::Drawing { target, .. } => Some(*target),
        }
    }

    fn kind(&self) -> NativeActionKind {
        match self {
            Self::Open(_) => NativeActionKind::Open,
            Self::Drawing { operation, .. } => operation.kind(),
        }
    }
}

impl NativeDrawingOperation {
    fn kind(&self) -> NativeActionKind {
        match self {
            Self::Save => NativeActionKind::Save,
            Self::Close { .. } => NativeActionKind::Close,
            Self::Undo => NativeActionKind::Undo,
            Self::Redo => NativeActionKind::Redo,
            Self::QueueExecDriver => NativeActionKind::QueueExecDriver,
        }
    }
}

impl NativeAction {
    pub(super) fn idle() -> Self {
        Self {
            state: NativeActionState::Idle,
        }
    }

    pub(super) fn issue(job_id: u64, command: NativeCommand) -> Self {
        Self {
            state: NativeActionState::Issued { job_id, command },
        }
    }

    pub(crate) fn job_id(&self) -> u64 {
        let NativeActionState::Issued { job_id, .. } = &self.state else {
            panic!("idle native action has no job ID");
        };

        *job_id
    }

    pub(crate) fn kind(&self) -> NativeActionKind {
        match &self.state {
            NativeActionState::Idle => NativeActionKind::None,
            NativeActionState::Issued { command, .. } => command.kind(),
        }
    }

    pub(crate) fn document_token(&self) -> usize {
        self.drawing_target().document_token
    }

    pub(crate) fn database_token(&self) -> usize {
        self.drawing_target().database_token
    }

    pub(crate) fn open_path(&self) -> &str {
        match &self.state {
            NativeActionState::Issued {
                command: NativeCommand::Open(path),
                ..
            } => path.as_str(),
            NativeActionState::Idle | NativeActionState::Issued { .. } => {
                panic!("native action is not open")
            }
        }
    }

    pub(crate) fn close_discard(&self) -> bool {
        match &self.state {
            NativeActionState::Issued {
                command:
                    NativeCommand::Drawing {
                        operation: NativeDrawingOperation::Close { discard },
                        ..
                    },
                ..
            } => *discard,
            NativeActionState::Idle | NativeActionState::Issued { .. } => {
                panic!("native action is not close")
            }
        }
    }

    fn drawing_target(&self) -> NativeDocumentKey {
        match &self.state {
            NativeActionState::Issued {
                command: NativeCommand::Drawing { target, .. },
                ..
            } => *target,
            NativeActionState::Idle | NativeActionState::Issued { .. } => {
                panic!("native action has no drawing target")
            }
        }
    }
}

impl NativeExecFinalizationObservation {
    fn native_state_unproved(&self) -> bool {
        self.undo_group_may_be_open
            || self.bridge_symbols_may_be_retained
            || self.staged_form_may_be_retained
            || self.value_writer_active
            || self.terminal_cleanup_failed
    }

    fn only_symbol_cleanup_unproved(&self) -> bool {
        (self.bridge_symbols_may_be_retained || self.terminal_cleanup_failed)
            && !self.undo_group_may_be_open
            && !self.staged_form_may_be_retained
            && !self.value_writer_active
    }
}

impl Operation {
    pub(super) fn complete_native(
        &mut self,
        mut result: NativeActionResult,
        drawings: &DrawingRegistry,
        native_target: Option<NativeDocumentKey>,
    ) -> Result<OperationOutcome, Error> {
        if matches!(
            self,
            Operation::Execute { execution, .. } if execution.outcome().is_some()
        ) && !matches!(
            result.kind,
            NativeActionResultKind::DocumentContextRestoreFailed
                | NativeActionResultKind::ExecBridgeFinalizationFailed
                | NativeActionResultKind::ExecBridgeSymbolsClearFailed
                | NativeActionResultKind::ExecBridgeFailed
        ) {
            return self.complete(drawings, native_target);
        }

        if result.kind == NativeActionResultKind::ExecBridgeSymbolsClearFailed
            && let Operation::Execute { execution, .. } = self
            && matches!(execution.outcome(), Some(ExecOutcome::Failure(_)))
        {
            return self.complete(drawings, native_target);
        }

        if matches!(
            result.kind,
            NativeActionResultKind::DocumentContextRestoreFailed
                | NativeActionResultKind::ExecBridgeFinalizationFailed
        ) && let Operation::Execute { execution, .. } = self
            && execution.outcome().is_some()
        {
            let recorded = execution.record_bridge_finalization_failure(ExecStepResult {
                kind: crate::exec::ExecStepResultKind::NativeError,
                native_status: result.native_status,
                lisp_errno: 0,
                detail: std::mem::take(&mut result.native_detail),
                bridge_symbols_clear_status: 0,
            });
            debug_assert!(recorded);

            return self.complete(drawings, native_target);
        }

        interpret(result, self)?;
        self.complete(drawings, native_target)
    }
}

pub(super) fn interpret(result: NativeActionResult, operation: &Operation) -> Result<(), Error> {
    let failure = NativeFailure {
        status: result.native_status,
        detail: result.native_detail,
    };

    match result.kind {
        NativeActionResultKind::Success => Ok(()),
        NativeActionResultKind::DrawingGone => Err(Error::DrawingGone),
        NativeActionResultKind::DrawingGenerationChanged => Err(Error::DrawingGenerationChanged),
        NativeActionResultKind::Unnamed => Err(operation
            .drawing_id()
            .map(Error::Unnamed)
            .unwrap_or(Error::UnknownResult(result.kind.repr))),
        NativeActionResultKind::ReadOnly => Err(operation
            .drawing_id()
            .map(Error::ReadOnly)
            .unwrap_or(Error::UnknownResult(result.kind.repr))),
        NativeActionResultKind::Dirty => Err(operation
            .drawing_id()
            .map(Error::Dirty)
            .unwrap_or(Error::UnknownResult(result.kind.repr))),
        NativeActionResultKind::OpenFailed => Err(Error::OpenFailed(failure)),
        NativeActionResultKind::LockFailed => Err(Error::LockFailed(failure)),
        NativeActionResultKind::SaveFailed => Err(Error::SaveFailed(failure)),
        NativeActionResultKind::CloseFailed => Err(Error::CloseFailed(failure)),
        NativeActionResultKind::HistoryFailed => {
            let Operation::History { direction, .. } = operation else {
                return Err(Error::UnknownResult(result.kind.repr));
            };

            Err(Error::HistoryFailed {
                direction: *direction,
                failure,
            })
        }
        NativeActionResultKind::NotQuiescent => Err(Error::NotQuiescent),
        NativeActionResultKind::UndoDisabled => Err(Error::UndoDisabled),
        NativeActionResultKind::DocumentContextFailed => Err(Error::DocumentContextFailed(failure)),
        NativeActionResultKind::DocumentContextRestoreFailed => {
            Err(Error::DocumentContextRestoreFailed(failure))
        }
        NativeActionResultKind::ExecBridgeFinalizationFailed => {
            Err(Error::ExecBridgeFinalizationFailed(failure))
        }
        NativeActionResultKind::ExecBridgeSymbolsClearFailed => {
            Err(Error::ExecBridgeSymbolsClearFailed(failure))
        }
        NativeActionResultKind::ExecBridgeFailed => Err(Error::ExecBridgeFailed(failure)),
        kind => Err(Error::UnknownResult(kind.repr)),
    }
}

pub(super) fn native_result_requires_quarantine(kind: NativeActionResultKind) -> bool {
    matches!(
        kind,
        NativeActionResultKind::DocumentContextRestoreFailed
            | NativeActionResultKind::ExecBridgeFinalizationFailed
            | NativeActionResultKind::ExecBridgeSymbolsClearFailed
    )
}

pub(super) struct ExecFinalizationDecision {
    pub(super) result: NativeActionResult,
    pub(super) quarantine: bool,
}

pub(super) fn classify_execution_finalization(
    mut result: NativeActionResult,
    observation: NativeExecFinalizationObservation,
) -> ExecFinalizationDecision {
    let native_state_unproved = observation.native_state_unproved();
    let quarantine = native_state_unproved || native_result_requires_quarantine(result.kind);
    let preserve_symbol_failure = result.kind
        == NativeActionResultKind::ExecBridgeSymbolsClearFailed
        && observation.only_symbol_cleanup_unproved();

    if native_state_unproved
        && result.kind != NativeActionResultKind::DocumentContextRestoreFailed
        && !preserve_symbol_failure
    {
        result.kind = NativeActionResultKind::ExecBridgeFinalizationFailed;
    }

    ExecFinalizationDecision { result, quarantine }
}

pub(super) fn schedule_native_actions() {
    let status = wake_native_actions();

    if status != 0 {
        super::queue::wake_failed(status);
    }
}

#[cfg(not(test))]
fn wake_native_actions() -> i32 {
    unsafe extern "C" {
        fn acadctl_wake_native_actions() -> i32;
    }

    // SAFETY: the native bridge exports this no-argument function for the plugin's lifetime.
    unsafe { acadctl_wake_native_actions() }
}

#[cfg(test)]
fn wake_native_actions() -> i32 {
    0
}
