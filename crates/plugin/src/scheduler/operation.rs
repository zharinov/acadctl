use acadctl_rpc::{DrawingId, DrawingPath};

use crate::drawing::{Drawing, DrawingRegistry, DrawingTarget, NativeDocumentKey};
use crate::exec::output::OutputSink;
use crate::exec::{Exec, ExecOutcome, ExecStepResult, NativeExecStep, ValueOutputLease};

use super::error::Error;
use super::native::NativeCommand;

pub(super) enum Operation {
    Open {
        path: DrawingPath,
    },
    Save {
        id: DrawingId,
    },
    Close {
        id: DrawingId,
        discard: bool,
    },
    History {
        id: DrawingId,
        direction: HistoryDirection,
    },
    Execute {
        id: DrawingId,
        execution: Box<Exec>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistoryDirection {
    Undo,
    Redo,
}

pub(super) enum OperationOutcome {
    Drawing(Drawing),
    Closed,
    Exec(ExecOutcome),
}

pub(super) enum Prepared {
    Immediate(Result<OperationOutcome, Error>),
    Native(NativeCommand),
}

impl Operation {
    pub(super) fn prepare(&self, drawings: &DrawingRegistry) -> Prepared {
        match self {
            Operation::Open { path } => drawings.find_by_path(path).map_or_else(
                || Prepared::Native(NativeCommand::open(path.clone())),
                |target| Prepared::Immediate(Ok(OperationOutcome::Drawing(target.drawing))),
            ),
            Operation::Save { id } => match drawings.find_by_id(*id) {
                Some(target) => Self::prepare_save(*id, target),
                None => Prepared::Immediate(Err(Error::DrawingNotFound(*id))),
            },
            Operation::Close { id, discard } => match drawings.find_by_id(*id) {
                Some(target) if target.drawing.modified && !discard => {
                    Prepared::Immediate(Err(Error::Dirty(*id)))
                }
                Some(target) => Prepared::Native(NativeCommand::close(target.native_key, *discard)),
                None => Prepared::Immediate(Err(Error::DrawingNotFound(*id))),
            },
            Operation::History { id, direction } => match drawings.find_by_id(*id) {
                Some(target) => Prepared::Native(match direction {
                    HistoryDirection::Undo => NativeCommand::undo(target.native_key),
                    HistoryDirection::Redo => NativeCommand::redo(target.native_key),
                }),
                None => Prepared::Immediate(Err(Error::DrawingNotFound(*id))),
            },
            Operation::Execute { id, execution } => match drawings.find_by_id(*id) {
                Some(_) if execution.outcome().is_some() => {
                    Prepared::Immediate(Ok(OperationOutcome::Exec(
                        execution
                            .outcome()
                            .expect("terminal execution has an outcome")
                            .clone(),
                    )))
                }
                Some(target) => {
                    Prepared::Native(NativeCommand::queue_exec_driver(target.native_key))
                }
                None => Prepared::Immediate(Err(Error::DrawingNotFound(*id))),
            },
        }
    }

    fn prepare_save(id: DrawingId, target: DrawingTarget) -> Prepared {
        if target.drawing.read_only {
            return Prepared::Immediate(Err(Error::ReadOnly(id)));
        }

        let Some(file_path) = target.drawing.file_path() else {
            return Prepared::Immediate(Err(Error::Unnamed(id)));
        };

        if !DrawingPath::has_dwg_extension(file_path) {
            return Prepared::Immediate(Err(Error::NotDwg));
        }

        if !target.drawing.modified {
            return Prepared::Immediate(Ok(OperationOutcome::Drawing(target.drawing)));
        }

        Prepared::Native(NativeCommand::save(target.native_key))
    }

    pub(super) fn complete(
        &mut self,
        drawings: &DrawingRegistry,
        native_target: Option<NativeDocumentKey>,
    ) -> Result<OperationOutcome, Error> {
        match self {
            Operation::Open { path } => drawings
                .find_by_path(path)
                .map(|target| OperationOutcome::Drawing(target.drawing))
                .ok_or(Error::OpenNotPublished),
            Operation::Save { id } => {
                let target = drawings
                    .find_by_id(*id)
                    .ok_or(Error::DrawingNotFound(*id))?;

                if target.drawing.modified {
                    return Err(Error::SaveNotPublished);
                }

                Ok(OperationOutcome::Drawing(target.drawing))
            }
            Operation::Close { id, .. } => {
                if drawings.find_by_id(*id).is_some() {
                    return Err(Error::CloseNotPublished);
                }

                Ok(OperationOutcome::Closed)
            }
            Operation::History { id, .. } => {
                let expected = native_target.ok_or(Error::DrawingGone)?;
                let target = drawings.find_by_id(*id).ok_or(Error::DrawingGone)?;

                if target.native_key != expected {
                    return Err(Error::DrawingGenerationChanged);
                }

                Ok(OperationOutcome::Drawing(target.drawing))
            }
            Operation::Execute { execution, .. } => execution
                .take_outcome()
                .map(OperationOutcome::Exec)
                .ok_or(Error::ExecNotFinished),
        }
    }

    pub(super) fn output_sink(&self) -> Option<OutputSink> {
        self.execution().map(Exec::output_sink)
    }

    pub(super) fn drawing_id(&self) -> Option<DrawingId> {
        match self {
            Self::Open { .. } => None,
            Self::Save { id }
            | Self::Close { id, .. }
            | Self::History { id, .. }
            | Self::Execute { id, .. } => Some(*id),
        }
    }

    pub(super) fn execution_source_bytes(&self) -> Option<usize> {
        self.execution().map(Exec::source_bytes)
    }

    pub(super) fn execution_start_pending(&self) -> bool {
        self.execution().is_some_and(Exec::start_deadline_pending)
    }

    pub(super) fn execution_has_not_handed_off_form(&self) -> bool {
        self.execution()
            .is_some_and(|execution| !execution.has_handed_off_form())
    }

    pub(super) fn execution_has_outcome(&self) -> bool {
        self.execution()
            .is_some_and(|execution| execution.outcome().is_some())
    }

    pub(super) fn expire_before_start(&mut self, detail: String) -> bool {
        self.execution_mut()
            .is_some_and(|execution| execution.expire_before_start(detail))
    }

    pub(super) fn finish_cancel_before_start(&mut self) -> bool {
        self.execution_mut().is_some_and(|execution| {
            !execution.has_handed_off_form()
                && execution.cancellation_requested()
                && execution.cancel_before_start()
        })
    }

    pub(super) fn request_cancel(&mut self) -> Option<(bool, OutputSink)> {
        let execution = self.execution_mut()?;
        Some((execution.request_cancel(), execution.output_sink()))
    }

    pub(super) fn cancel_before_start(&mut self) -> Option<OutputSink> {
        let execution = self.execution_mut()?;
        execution
            .cancel_before_start()
            .then(|| execution.output_sink())
    }

    pub(super) fn take_execution_step(&mut self) -> NativeExecStep {
        self.execution_mut()
            .map_or_else(NativeExecStep::invalid, Exec::take_step)
    }

    pub(super) fn acquire_eval_value_output(&self) -> Option<ValueOutputLease> {
        self.execution().and_then(Exec::acquire_eval_value_output)
    }

    pub(super) fn complete_execution_step(&mut self, result: ExecStepResult) -> bool {
        self.execution_mut()
            .is_some_and(|execution| execution.complete_step(result))
    }

    pub(super) fn abandon_execution(&mut self, result: ExecStepResult) -> bool {
        self.execution_mut()
            .is_some_and(|execution| execution.abandon(result))
    }

    fn execution(&self) -> Option<&Exec> {
        match self {
            Self::Execute { execution, .. } => Some(execution),
            Self::Open { .. } | Self::Save { .. } | Self::Close { .. } | Self::History { .. } => {
                None
            }
        }
    }

    fn execution_mut(&mut self) -> Option<&mut Exec> {
        match self {
            Self::Execute { execution, .. } => Some(execution),
            Self::Open { .. } | Self::Save { .. } | Self::Close { .. } | Self::History { .. } => {
                None
            }
        }
    }
}
