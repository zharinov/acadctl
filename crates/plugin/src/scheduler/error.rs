use std::fmt;

use acadctl_rpc::{DrawingErrorKind, DrawingId};

use crate::exec::DrawingOutcome;

use super::operation::HistoryDirection;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    SchedulerStateUnavailable,
    Stopped,
    PluginStopping,
    DrawingNotFound(DrawingId),
    DrawingGone,
    DrawingGenerationChanged,
    NotActive(DrawingId),
    Unnamed(DrawingId),
    ReadOnly(DrawingId),
    DestinationExists(DrawingId),
    SavePathUnavailable,
    Dirty(DrawingId),
    NotDwg,
    OpenFailed(NativeFailure),
    SwitchFailed(NativeFailure),
    SaveFailed(NativeFailure),
    CloseFailed(NativeFailure),
    HistoryFailed {
        direction: HistoryDirection,
        failure: NativeFailure,
    },
    OpenNotPublished,
    SwitchNotPublished,
    SaveNotPublished,
    CloseNotPublished,
    NotQuiescent,
    UndoDisabled,
    DocumentContextFailed(NativeFailure),
    DocumentContextRestoreFailed(NativeFailure),
    ExecBridgeFinalizationFailed(NativeFailure),
    ExecBridgeSymbolsClearFailed(NativeFailure),
    ExecBridgeFailed(NativeFailure),
    ExecNotFinished,
    ReadinessTimedOut(Option<DrawingId>),
    MutationCapacity,
    ExecCapacity,
    NativeMutationStateUnknown,
    UnknownResult(u8),
}

#[derive(Debug, PartialEq, Eq)]
pub struct NativeFailure {
    pub(super) status: i32,
    pub(super) detail: String,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchedulerStateUnavailable => formatter.write_str("mutation scheduler state is unavailable"),
            Self::Stopped => formatter.write_str("the native operation stopped before completion"),
            Self::PluginStopping => formatter.write_str("the acadctl plugin is stopping"),
            Self::DrawingNotFound(id) => write!(formatter, "Drawing '{id}' is not open."),
            Self::DrawingGone => formatter.write_str("The drawing is no longer open"),
            Self::DrawingGenerationChanged => formatter
                .write_str("The drawing was replaced before AutoCAD could perform the operation"),
            Self::NotActive(id) => write!(formatter, "Drawing '{id}' is not active."),
            Self::Unnamed(id) => write!(
                formatter,
                "Drawing '{id}' has no file name; use --as FILE."
            ),
            Self::ReadOnly(id) => write!(formatter, "Drawing '{id}' is read-only."),
            Self::DestinationExists(_) => formatter.write_str(
                "Destination already exists; use another path or omit --as",
            ),
            Self::SavePathUnavailable => formatter.write_str("The save destination is unavailable"),
            Self::Dirty(id) => write!(
                formatter,
                "Drawing '{id}' has unsaved changes."
            ),
            Self::NotDwg => formatter.write_str("Only DWG drawings can be saved"),
            Self::OpenFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not open the drawing")
            }
            Self::SwitchFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not switch to the drawing")
            }
            Self::SaveFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not save the drawing")
            }
            Self::CloseFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not close the drawing")
            }
            Self::HistoryFailed { direction, failure } => failure.fmt_with_context(
                formatter,
                match direction {
                    HistoryDirection::Undo => "Could not undo the drawing's last history step",
                    HistoryDirection::Redo => "Could not redo the drawing's next history step",
                },
            ),
            Self::OpenNotPublished => formatter
                .write_str("AutoCAD opened the drawing but did not publish its drawing state"),
            Self::SwitchNotPublished => formatter
                .write_str("AutoCAD switched drawings but did not publish the active drawing"),
            Self::SaveNotPublished => {
                formatter.write_str("AutoCAD completed the save but still reports unsaved changes")
            }
            Self::CloseNotPublished => {
                formatter.write_str("AutoCAD completed the close but the drawing is still open")
            }
            Self::NotQuiescent => formatter.write_str("The drawing is busy"),
            Self::UndoDisabled => {
                formatter.write_str("Undo recording is disabled for the drawing")
            }
            Self::DocumentContextFailed(failure) => {
                failure.fmt_with_context(formatter, "Could not establish AutoCAD document context")
            }
            Self::DocumentContextRestoreFailed(failure) => failure.fmt_with_context(
                formatter,
                "Could not release AutoCAD document context safely",
            ),
            Self::ExecBridgeFinalizationFailed(failure) => failure.fmt_with_context(
                formatter,
                "Could not release native execution state safely",
            ),
            Self::ExecBridgeSymbolsClearFailed(failure) => failure.fmt_with_context(
                formatter,
                "Could not clear the reserved AutoLISP execution bridge symbols",
            ),
            Self::ExecBridgeFailed(failure) => {
                failure.fmt_with_context(formatter, "The AutoLISP execution bridge failed")
            }
            Self::ExecNotFinished => {
                formatter.write_str("The native execution ended without a terminal outcome")
            }
            Self::ReadinessTimedOut(_) => {
                formatter.write_str("AutoCAD did not become ready within 60 seconds")
            }
            Self::MutationCapacity => {
                formatter.write_str("AutoCAD already has the maximum number of pending operations")
            }
            Self::ExecCapacity => formatter.write_str(
                "AutoCAD already has the maximum number or total size of execution requests",
            ),
            Self::NativeMutationStateUnknown => formatter.write_str(
                "AutoCAD's mutation context is unknown; restart AutoCAD before issuing another mutation",
            ),
            Self::UnknownResult(kind) => {
                write!(
                    formatter,
                    "AutoCAD returned an unknown native result ({kind})"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

impl NativeFailure {
    fn fmt_with_context(&self, formatter: &mut fmt::Formatter<'_>, context: &str) -> fmt::Result {
        if self.detail.is_empty() {
            write!(formatter, "{context} (ObjectARX status {})", self.status)
        } else {
            write!(formatter, "{context}: {}", self.detail)
        }
    }
}

impl Error {
    pub const fn drawing_error_kind(&self) -> Option<DrawingErrorKind> {
        match self {
            Self::DrawingNotFound(_) | Self::DrawingGone => Some(DrawingErrorKind::NotOpen),
            Self::DrawingGenerationChanged => Some(DrawingErrorKind::Replaced),
            Self::NotActive(_) => Some(DrawingErrorKind::NotActive),
            Self::Unnamed(_) => Some(DrawingErrorKind::NoFileName),
            Self::ReadOnly(_) => Some(DrawingErrorKind::ReadOnly),
            Self::DestinationExists(_) => Some(DrawingErrorKind::DestinationExists),
            Self::Dirty(_) => Some(DrawingErrorKind::UnsavedChanges),
            Self::NotQuiescent => Some(DrawingErrorKind::Busy),
            Self::UndoDisabled => Some(DrawingErrorKind::UndoDisabled),
            Self::ReadinessTimedOut(_) => Some(DrawingErrorKind::ReadinessTimedOut),
            _ => None,
        }
    }

    pub const fn drawing_id(&self) -> Option<DrawingId> {
        match self {
            Self::DrawingNotFound(id)
            | Self::NotActive(id)
            | Self::Unnamed(id)
            | Self::ReadOnly(id)
            | Self::DestinationExists(id)
            | Self::Dirty(id) => Some(*id),
            Self::ReadinessTimedOut(id) => *id,
            _ => None,
        }
    }

    pub const fn is_internal(&self) -> bool {
        matches!(
            self,
            Self::SchedulerStateUnavailable
                | Self::Stopped
                | Self::OpenNotPublished
                | Self::SwitchNotPublished
                | Self::SaveNotPublished
                | Self::SavePathUnavailable
                | Self::CloseNotPublished
                | Self::DocumentContextFailed(_)
                | Self::DocumentContextRestoreFailed(_)
                | Self::ExecBridgeFinalizationFailed(_)
                | Self::ExecBridgeSymbolsClearFailed(_)
                | Self::ExecBridgeFailed(_)
                | Self::ExecNotFinished
                | Self::NativeMutationStateUnknown
        )
    }

    pub const fn drawing_outcome(&self) -> DrawingOutcome {
        if matches!(
            self,
            Self::DocumentContextRestoreFailed(_)
                | Self::ExecBridgeFinalizationFailed(_)
                | Self::ExecBridgeSymbolsClearFailed(_)
                | Self::ExecBridgeFailed(_)
                | Self::ExecNotFinished
                | Self::NativeMutationStateUnknown
                | Self::Stopped
                | Self::UnknownResult(_)
        ) {
            DrawingOutcome::Unknown
        } else {
            DrawingOutcome::NotStarted
        }
    }
}
