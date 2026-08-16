use std::time::Instant;

use crate::doc::NativeDocKey;
use crate::exec::value::writer::NativeValueWriter;
use crate::exec::{ExecStepResult, NativeExecStep, bound_diagnostic};

use super::queue::{MutationJobId, SCHEDULER};

pub(crate) fn take_execution_step(job_id: MutationJobId) -> NativeExecStep {
    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return NativeExecStep::invalid();
    };

    scheduler.take_execution_step(job_id, Instant::now())
}

pub(crate) fn begin_eval_value(
    job_id: MutationJobId,
    document_token: usize,
    database_token: usize,
) -> NativeValueWriter {
    let Ok(scheduler) = SCHEDULER.lock() else {
        return NativeValueWriter::inactive();
    };

    let lease = scheduler.acquire_eval_value_output(
        job_id,
        NativeDocKey {
            document_token,
            database_token,
        },
    );

    lease.map_or_else(NativeValueWriter::inactive, NativeValueWriter::eval_value)
}

pub(crate) fn complete_execution_step(job_id: MutationJobId, mut result: ExecStepResult) -> bool {
    bound_diagnostic(&mut result.detail);

    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return false;
    };

    scheduler.complete_execution_step(job_id, result)
}

pub(crate) fn abandon_execution(job_id: MutationJobId, mut result: ExecStepResult) -> bool {
    bound_diagnostic(&mut result.detail);

    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return false;
    };

    scheduler.abandon_execution(job_id, result)
}
