use acadctl_rpc::{DocId, DrawingPath};

use crate::doc::{Doc, DocRegistry, DocTarget, NativeDocKey};
use crate::exec::output::OutputSink;
use crate::exec::{Exec, ExecOutcome, ExecStepResult};
use crate::ffi::{NativeAction, NativeActionKind, NativeActionResult, NativeActionResultKind};

use super::error::{Error, NativeFailure};

pub(super) enum Operation {
    Open {
        path: DrawingPath,
    },
    Save {
        id: DocId,
    },
    Close {
        id: DocId,
        discard: bool,
    },
    History {
        id: DocId,
        direction: HistoryDirection,
    },
    Execute {
        id: DocId,
        execution: Box<Exec>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistoryDirection {
    Undo,
    Redo,
}

pub(super) enum OperationOutcome {
    Doc(Doc),
    Closed,
    Exec(ExecOutcome),
}

pub(super) enum Prepared {
    Immediate(Result<OperationOutcome, Error>),
    Native(NativeRequest),
}

pub(super) enum NativeRequest {
    Open(DrawingPath),
    Save(NativeDocKey),
    Close { target: NativeDocKey, discard: bool },
    Undo(NativeDocKey),
    Redo(NativeDocKey),
    QueueExecDriver(NativeDocKey),
}

pub(super) fn prepare(operation: &Operation, documents: &DocRegistry) -> Prepared {
    match operation {
        Operation::Open { path } => documents.find_by_path(path).map_or_else(
            || Prepared::Native(NativeRequest::Open(path.clone())),
            |target| Prepared::Immediate(Ok(OperationOutcome::Doc(target.document))),
        ),
        Operation::Save { id } => match documents.find_by_id(*id) {
            Some(target) => prepare_save(*id, target),
            None => Prepared::Immediate(Err(Error::DocNotFound(*id))),
        },
        Operation::Close { id, discard } => match documents.find_by_id(*id) {
            Some(target) if target.document.modified && !discard => {
                Prepared::Immediate(Err(Error::Dirty(*id)))
            }
            Some(target) => Prepared::Native(NativeRequest::Close {
                target: target.native_key,
                discard: *discard,
            }),
            None => Prepared::Immediate(Err(Error::DocNotFound(*id))),
        },
        Operation::History { id, direction } => match documents.find_by_id(*id) {
            Some(target) => Prepared::Native(match direction {
                HistoryDirection::Undo => NativeRequest::Undo(target.native_key),
                HistoryDirection::Redo => NativeRequest::Redo(target.native_key),
            }),
            None => Prepared::Immediate(Err(Error::DocNotFound(*id))),
        },
        Operation::Execute { id, execution } => match documents.find_by_id(*id) {
            Some(_) if execution.outcome().is_some() => {
                Prepared::Immediate(Ok(OperationOutcome::Exec(
                    execution
                        .outcome()
                        .expect("terminal execution has an outcome")
                        .clone(),
                )))
            }
            Some(target) => Prepared::Native(NativeRequest::QueueExecDriver(target.native_key)),
            None => Prepared::Immediate(Err(Error::DocNotFound(*id))),
        },
    }
}

pub(super) fn prepare_save(id: DocId, target: DocTarget) -> Prepared {
    if target.document.read_only {
        return Prepared::Immediate(Err(Error::ReadOnly(id)));
    }

    let Some(file_path) = target.document.file_path() else {
        return Prepared::Immediate(Err(Error::Unnamed(id)));
    };

    if !DrawingPath::has_dwg_extension(file_path) {
        return Prepared::Immediate(Err(Error::NotDwg));
    }

    if !target.document.modified {
        return Prepared::Immediate(Ok(OperationOutcome::Doc(target.document)));
    }

    Prepared::Native(NativeRequest::Save(target.native_key))
}

pub(super) fn finalize(
    operation: &mut Operation,
    documents: &DocRegistry,
    native_target: Option<NativeDocKey>,
) -> Result<OperationOutcome, Error> {
    match operation {
        Operation::Open { path } => documents
            .find_by_path(path)
            .map(|target| OperationOutcome::Doc(target.document))
            .ok_or(Error::OpenNotPublished),
        Operation::Save { id } => {
            let target = documents.find_by_id(*id).ok_or(Error::DocNotFound(*id))?;

            if target.document.modified {
                Err(Error::SaveNotPublished)
            } else {
                Ok(OperationOutcome::Doc(target.document))
            }
        }
        Operation::Close { id, .. } => {
            if documents.find_by_id(*id).is_none() {
                Ok(OperationOutcome::Closed)
            } else {
                Err(Error::CloseNotPublished)
            }
        }
        Operation::History { id, .. } => {
            let expected = native_target.ok_or(Error::DocGone)?;
            let target = documents.find_by_id(*id).ok_or(Error::DocGone)?;

            if target.native_key != expected {
                Err(Error::DocGenerationChanged)
            } else {
                Ok(OperationOutcome::Doc(target.document))
            }
        }
        Operation::Execute { execution, .. } => execution
            .take_outcome()
            .map(OperationOutcome::Exec)
            .ok_or(Error::ExecNotFinished),
    }
}

pub(super) fn complete_operation(
    mut result: NativeActionResult,
    operation: &mut Operation,
    documents: &DocRegistry,
    native_target: Option<NativeDocKey>,
) -> Result<OperationOutcome, Error> {
    if matches!(
        operation,
        Operation::Execute { execution, .. } if execution.outcome().is_some()
    ) && !matches!(
        result.kind,
        NativeActionResultKind::DocContextRestoreFailed
            | NativeActionResultKind::ExecBridgeFinalizationFailed
            | NativeActionResultKind::ExecBridgeSymbolsClearFailed
            | NativeActionResultKind::ExecBridgeFailed
    ) {
        return finalize(operation, documents, native_target);
    }

    if result.kind == NativeActionResultKind::ExecBridgeSymbolsClearFailed
        && let Operation::Execute { execution, .. } = operation
        && matches!(execution.outcome(), Some(ExecOutcome::Failure(_)))
    {
        return finalize(operation, documents, native_target);
    }

    if matches!(
        result.kind,
        NativeActionResultKind::DocContextRestoreFailed
            | NativeActionResultKind::ExecBridgeFinalizationFailed
    ) && let Operation::Execute { execution, .. } = operation
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

        return finalize(operation, documents, native_target);
    }

    interpret(result, operation)?;
    finalize(operation, documents, native_target)
}

impl NativeRequest {
    pub(super) fn into_action(self, job_id: u64) -> (NativeAction, Option<NativeDocKey>) {
        let (kind, target, path, discard) = match self {
            Self::Open(path) => (NativeActionKind::Open, None, path.into_string(), false),
            Self::Save(target) => (NativeActionKind::Save, Some(target), String::new(), false),
            Self::Close { target, discard } => (
                NativeActionKind::Close,
                Some(target),
                String::new(),
                discard,
            ),
            Self::Undo(target) => (NativeActionKind::Undo, Some(target), String::new(), false),
            Self::Redo(target) => (NativeActionKind::Redo, Some(target), String::new(), false),
            Self::QueueExecDriver(target) => (
                NativeActionKind::QueueExecDriver,
                Some(target),
                String::new(),
                false,
            ),
        };
        let native_target = target.unwrap_or(NativeDocKey {
            document_token: 0,
            database_token: 0,
        });

        (
            NativeAction {
                job_id,
                kind,
                document_token: native_target.document_token,
                database_token: native_target.database_token,
                path,
                discard,
            },
            target,
        )
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

impl Operation {
    pub(super) fn output_sink(&self) -> Option<OutputSink> {
        match self {
            Self::Execute { execution, .. } => Some(execution.output_sink()),
            Self::Open { .. } | Self::Save { .. } | Self::Close { .. } | Self::History { .. } => {
                None
            }
        }
    }

    pub(super) fn document_id(&self) -> Option<DocId> {
        match self {
            Self::Open { .. } => None,
            Self::Save { id }
            | Self::Close { id, .. }
            | Self::History { id, .. }
            | Self::Execute { id, .. } => Some(*id),
        }
    }
}
