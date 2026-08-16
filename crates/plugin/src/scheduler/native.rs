use acadctl_rpc::DrawingPath;

use crate::doc::{DocRegistry, NativeDocKey};
use crate::exec::{ExecOutcome, ExecStepResult};
use crate::ffi::{
    NativeActionKind, NativeActionResult, NativeActionResultKind, NativeExecFinalizationObservation,
};

use super::error::{Error, NativeFailure};
use super::operation::{Operation, OperationOutcome};

pub(super) enum NativeCommand {
    Open(DrawingPath),
    Document {
        target: NativeDocKey,
        operation: NativeDocumentOperation,
    },
}

pub(super) enum NativeDocumentOperation {
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

    pub(super) fn save(target: NativeDocKey) -> Self {
        Self::document(target, NativeDocumentOperation::Save)
    }

    pub(super) fn close(target: NativeDocKey, discard: bool) -> Self {
        Self::document(target, NativeDocumentOperation::Close { discard })
    }

    pub(super) fn undo(target: NativeDocKey) -> Self {
        Self::document(target, NativeDocumentOperation::Undo)
    }

    pub(super) fn redo(target: NativeDocKey) -> Self {
        Self::document(target, NativeDocumentOperation::Redo)
    }

    pub(super) fn queue_exec_driver(target: NativeDocKey) -> Self {
        Self::document(target, NativeDocumentOperation::QueueExecDriver)
    }

    fn document(target: NativeDocKey, operation: NativeDocumentOperation) -> Self {
        Self::Document { target, operation }
    }

    pub(super) fn target(&self) -> Option<NativeDocKey> {
        match self {
            Self::Open(_) => None,
            Self::Document { target, .. } => Some(*target),
        }
    }

    fn kind(&self) -> NativeActionKind {
        match self {
            Self::Open(_) => NativeActionKind::Open,
            Self::Document { operation, .. } => operation.kind(),
        }
    }
}

impl NativeDocumentOperation {
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
        self.document_target().document_token
    }

    pub(crate) fn database_token(&self) -> usize {
        self.document_target().database_token
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
                    NativeCommand::Document {
                        operation: NativeDocumentOperation::Close { discard },
                        ..
                    },
                ..
            } => *discard,
            NativeActionState::Idle | NativeActionState::Issued { .. } => {
                panic!("native action is not close")
            }
        }
    }

    fn document_target(&self) -> NativeDocKey {
        match &self.state {
            NativeActionState::Issued {
                command: NativeCommand::Document { target, .. },
                ..
            } => *target,
            NativeActionState::Idle | NativeActionState::Issued { .. } => {
                panic!("native action has no document target")
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
        documents: &DocRegistry,
        native_target: Option<NativeDocKey>,
    ) -> Result<OperationOutcome, Error> {
        if matches!(
            self,
            Operation::Execute { execution, .. } if execution.outcome().is_some()
        ) && !matches!(
            result.kind,
            NativeActionResultKind::DocContextRestoreFailed
                | NativeActionResultKind::ExecBridgeFinalizationFailed
                | NativeActionResultKind::ExecBridgeSymbolsClearFailed
                | NativeActionResultKind::ExecBridgeFailed
        ) {
            return self.complete(documents, native_target);
        }

        if result.kind == NativeActionResultKind::ExecBridgeSymbolsClearFailed
            && let Operation::Execute { execution, .. } = self
            && matches!(execution.outcome(), Some(ExecOutcome::Failure(_)))
        {
            return self.complete(documents, native_target);
        }

        if matches!(
            result.kind,
            NativeActionResultKind::DocContextRestoreFailed
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

            return self.complete(documents, native_target);
        }

        interpret(result, self)?;
        self.complete(documents, native_target)
    }
}

pub(super) fn interpret(result: NativeActionResult, operation: &Operation) -> Result<(), Error> {
    let failure = NativeFailure {
        status: result.native_status,
        detail: result.native_detail,
    };

    match result.kind {
        NativeActionResultKind::Success => Ok(()),
        NativeActionResultKind::DocGone => Err(Error::DocGone),
        NativeActionResultKind::DocGenerationChanged => Err(Error::DocGenerationChanged),
        NativeActionResultKind::Unnamed => Err(operation
            .document_id()
            .map(Error::Unnamed)
            .unwrap_or(Error::UnknownResult(result.kind.repr))),
        NativeActionResultKind::ReadOnly => Err(operation
            .document_id()
            .map(Error::ReadOnly)
            .unwrap_or(Error::UnknownResult(result.kind.repr))),
        NativeActionResultKind::Dirty => Err(operation
            .document_id()
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
        NativeActionResultKind::DocContextFailed => Err(Error::DocContextFailed(failure)),
        NativeActionResultKind::DocContextRestoreFailed => {
            Err(Error::DocContextRestoreFailed(failure))
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
        NativeActionResultKind::DocContextRestoreFailed
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
        && result.kind != NativeActionResultKind::DocContextRestoreFailed
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
