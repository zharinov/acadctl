use acadctl_rpc::{DocId, DrawingPath};

use crate::doc::{Doc, DocRegistry, DocTarget, NativeDocKey};
use crate::exec::output::OutputSink;
use crate::exec::{Exec, ExecOutcome};

use super::error::Error;
use super::native::NativeCommand;

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
    Native(NativeCommand),
}

pub(super) fn prepare(operation: &Operation, documents: &DocRegistry) -> Prepared {
    match operation {
        Operation::Open { path } => documents.find_by_path(path).map_or_else(
            || Prepared::Native(NativeCommand::open(path.clone())),
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
            Some(target) => Prepared::Native(NativeCommand::close(target.native_key, *discard)),
            None => Prepared::Immediate(Err(Error::DocNotFound(*id))),
        },
        Operation::History { id, direction } => match documents.find_by_id(*id) {
            Some(target) => Prepared::Native(match direction {
                HistoryDirection::Undo => NativeCommand::undo(target.native_key),
                HistoryDirection::Redo => NativeCommand::redo(target.native_key),
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
            Some(target) => Prepared::Native(NativeCommand::queue_exec_driver(target.native_key)),
            None => Prepared::Immediate(Err(Error::DocNotFound(*id))),
        },
    }
}

fn prepare_save(id: DocId, target: DocTarget) -> Prepared {
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

    Prepared::Native(NativeCommand::save(target.native_key))
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
