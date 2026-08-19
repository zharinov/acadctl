use acadctl_rpc::{DrawingId, DrawingPath, SavePath};

use crate::drawing::{Drawing, DrawingRegistry, DrawingTarget, NativeDocumentKey};
use crate::exec::output::OutputSink;
use crate::exec::{Exec, ExecOutcome, ExecStepResult, NativeExecStep, ValueOutputLease};

use super::error::Error;
use super::native::{CaptureRegion, NativeCommand, ViewportCapture, interpret_capture};

pub(super) enum Operation {
    Open {
        path: DrawingPath,
    },
    Switch {
        id: DrawingId,
    },
    Save {
        id: DrawingId,
        path: Option<SavePath>,
    },
    Close {
        id: DrawingId,
        discard: bool,
    },
    Capture {
        id: DrawingId,
        region: CaptureRegion,
        max_long_edge: u32,
        capture: Option<ViewportCapture>,
    },
    History {
        id: DrawingId,
        direction: HistoryDirection,
        context: DocumentContextPolicy,
    },
    Execute {
        id: DrawingId,
        execution: Box<Exec>,
        context: DocumentContextPolicy,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistoryDirection {
    Undo,
    Redo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DocumentContextPolicy {
    RequireActive,
    ForceTemporary,
}

pub(super) enum OperationOutcome {
    Drawing(Drawing),
    Closed,
    Capture(ViewportCapture),
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
            Operation::Switch { id } => match drawings.find_by_id(*id) {
                Some(target) => Prepared::Native(NativeCommand::switch(target.native_key)),
                None => Prepared::Immediate(Err(Error::DrawingNotFound(*id))),
            },
            Operation::Save { id, path } => match drawings.find_by_id(*id) {
                Some(target) => Self::prepare_save(*id, path.as_ref(), target),
                None => Prepared::Immediate(Err(Error::DrawingNotFound(*id))),
            },
            Operation::Close { id, discard } => match drawings.find_by_id(*id) {
                Some(target) if target.drawing.modified && !discard => {
                    Prepared::Immediate(Err(Error::Dirty(*id)))
                }
                Some(target) => Prepared::Native(NativeCommand::close(target.native_key, *discard)),
                None => Prepared::Immediate(Err(Error::DrawingNotFound(*id))),
            },
            Operation::Capture {
                id,
                region,
                max_long_edge,
                capture,
            } => match drawings.find_by_id(*id) {
                Some(_) if capture.is_some() => Prepared::Immediate(Err(Error::CaptureInvalid(
                    "capture operation was dispatched more than once".into(),
                ))),
                Some(target) => Prepared::Native(NativeCommand::capture(
                    target.native_key,
                    *region,
                    *max_long_edge,
                )),
                None => Prepared::Immediate(Err(Error::DrawingNotFound(*id))),
            },
            Operation::History {
                id,
                direction,
                context,
            } => match drawings.find_by_id(*id) {
                Some(target) => Prepared::Native(match direction {
                    HistoryDirection::Undo => NativeCommand::undo(target.native_key, *context),
                    HistoryDirection::Redo => NativeCommand::redo(target.native_key, *context),
                }),
                None => Prepared::Immediate(Err(Error::DrawingNotFound(*id))),
            },
            Operation::Execute {
                id,
                execution,
                context,
            } => match drawings.find_by_id(*id) {
                Some(_) if execution.outcome().is_some() => {
                    Prepared::Immediate(Ok(OperationOutcome::Exec(
                        execution
                            .outcome()
                            .expect("terminal execution has an outcome")
                            .clone(),
                    )))
                }
                Some(target) => Prepared::Native(NativeCommand::queue_exec_driver(
                    target.native_key,
                    *context,
                )),
                None => Prepared::Immediate(Err(Error::DrawingNotFound(*id))),
            },
        }
    }

    fn prepare_save(id: DrawingId, path: Option<&SavePath>, target: DrawingTarget) -> Prepared {
        if target.drawing.read_only {
            return Prepared::Immediate(Err(Error::ReadOnly(id)));
        }

        if let Some(path) = path {
            match path.ensure_available() {
                Ok(()) => {}
                Err(acadctl_rpc::DrawingPathError::AlreadyExists(_)) => {
                    return Prepared::Immediate(Err(Error::DestinationExists(id)));
                }
                Err(_) => return Prepared::Immediate(Err(Error::SavePathUnavailable)),
            }
        } else {
            let Some(file_path) = target.drawing.file_path() else {
                return Prepared::Immediate(Err(Error::Unnamed(id)));
            };

            if !DrawingPath::has_dwg_extension(file_path) {
                return Prepared::Immediate(Err(Error::NotDwg));
            }
        }

        Prepared::Native(NativeCommand::save(target.native_key, path.cloned()))
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
            Operation::Switch { id } => {
                let expected = native_target.ok_or(Error::DrawingGone)?;
                let target = drawings.find_by_id(*id).ok_or(Error::DrawingGone)?;

                if target.native_key != expected {
                    return Err(Error::DrawingGenerationChanged);
                }

                if !target.drawing.active {
                    return Err(Error::SwitchNotPublished);
                }

                Ok(OperationOutcome::Drawing(target.drawing))
            }
            Operation::Save { id, path } => {
                let target = drawings
                    .find_by_id(*id)
                    .ok_or(Error::DrawingNotFound(*id))?;

                if target.native_key != native_target.ok_or(Error::DrawingGone)? {
                    return Err(Error::DrawingGenerationChanged);
                }

                if target.drawing.modified {
                    return Err(Error::SaveNotPublished);
                }

                if let Some(expected_path) = path {
                    let published_path = target.drawing.file_path().map(|path| path.as_str());

                    if published_path != Some(expected_path.as_str()) {
                        return Err(Error::SaveNotPublished);
                    }
                }

                Ok(OperationOutcome::Drawing(target.drawing))
            }
            Operation::Close { id, .. } => {
                if drawings.find_by_id(*id).is_some() {
                    return Err(Error::CloseNotPublished);
                }

                Ok(OperationOutcome::Closed)
            }
            Operation::Capture { id, capture, .. } => {
                let expected = native_target.ok_or(Error::DrawingGone)?;
                let target = drawings.find_by_id(*id).ok_or(Error::DrawingGone)?;

                if target.native_key != expected {
                    return Err(Error::DrawingGenerationChanged);
                }

                capture
                    .take()
                    .map(OperationOutcome::Capture)
                    .ok_or_else(|| Error::CaptureInvalid("native capture returned no frame".into()))
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
            Self::Switch { id }
            | Self::Save { id, .. }
            | Self::Close { id, .. }
            | Self::Capture { id, .. }
            | Self::History { id, .. }
            | Self::Execute { id, .. } => Some(*id),
        }
    }

    pub(super) fn execution_source_bytes(&self) -> Option<usize> {
        self.execution().map(Exec::source_bytes)
    }

    pub(super) fn is_execution(&self) -> bool {
        self.execution().is_some()
    }

    pub(super) fn record_native_capture(
        &mut self,
        result: &crate::ffi::NativeCaptureResult,
        pixels: &[u8],
    ) -> Result<(), Error> {
        let Self::Capture { id, capture, .. } = self else {
            return Err(Error::CaptureInvalid(
                "native capture completed a different operation".into(),
            ));
        };

        *capture = Some(interpret_capture(result, pixels, *id)?);
        Ok(())
    }

    pub(super) fn can_wait_for_readiness(&self) -> bool {
        self.execution()
            .is_none_or(|execution| !execution.has_handed_off_form())
    }

    pub(super) fn execution_readiness_wait_pending(&self) -> bool {
        self.execution().is_some_and(Exec::readiness_wait_pending)
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

    pub(super) fn acquire_form_output(&self) -> Option<ValueOutputLease> {
        self.execution().and_then(Exec::acquire_form_output)
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
            Self::Open { .. }
            | Self::Switch { .. }
            | Self::Save { .. }
            | Self::Close { .. }
            | Self::Capture { .. }
            | Self::History { .. } => None,
        }
    }

    fn execution_mut(&mut self) -> Option<&mut Exec> {
        match self {
            Self::Execute { execution, .. } => Some(execution),
            Self::Open { .. }
            | Self::Switch { .. }
            | Self::Save { .. }
            | Self::Close { .. }
            | Self::Capture { .. }
            | Self::History { .. } => None,
        }
    }
}
