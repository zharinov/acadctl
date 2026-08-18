use bytes::Bytes;

use super::diagnostic::append_diagnostic;
use super::value::event::OutputEvent;
use super::value::port::{NativeOutputPort, ValueEvent, WriteResult};
use super::*;

fn source_name(value: &str) -> acadctl_rpc::SourceName {
    acadctl_rpc::SourceName::new(value).unwrap()
}

#[test]
fn diagnostic_composition_preserves_the_stored_byte_limit() {
    let mut message = bounded_diagnostic("é".repeat(acadctl_rpc::MAX_DIAGNOSTIC_BYTES));
    append_diagnostic(&mut message, &"x".repeat(acadctl_rpc::MAX_DIAGNOSTIC_BYTES));
    assert!(message.len() <= acadctl_rpc::MAX_DIAGNOSTIC_BYTES);
    assert!(message.ends_with("... [truncated]"));
    assert!(std::str::from_utf8(message.as_bytes()).is_ok());
}

#[test]
fn explicit_native_truncation_is_preserved_without_padding() {
    let message = bounded_native_diagnostic("short".into(), true);

    assert_eq!(message, "short... [truncated]");
}

#[test]
fn validates_source_before_native_admission() {
    assert!(
        Exec::new(
            ExecMode::Exec,
            source_name("<stdin>"),
            "x".repeat(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES).into(),
        )
        .is_ok()
    );
    assert!(
        Exec::new(
            ExecMode::Exec,
            source_name("<stdin>"),
            format!(
                "\u{feff}{}",
                "x".repeat(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES)
            )
            .into(),
        )
        .is_ok()
    );
    assert_eq!(
        Exec::new(
            ExecMode::Exec,
            source_name("<stdin>"),
            "x".repeat(acadctl_rpc::MAX_EXECUTION_SOURCE_BYTES + 1)
                .into(),
        )
        .err()
        .unwrap(),
        SourceValidationError::SourceTooLarge
    );
    assert_eq!(
        Exec::new(ExecMode::Exec, source_name("<stdin>"), "x\0y".into())
            .err()
            .unwrap(),
        SourceValidationError::NullCharacter
    );
    assert_eq!(
        Exec::new(
            ExecMode::Exec,
            source_name("<stdin>"),
            Bytes::from_static(b"\xff"),
        )
        .err()
        .unwrap(),
        SourceValidationError::InvalidUtf8
    );
    assert!(matches!(
        Exec::new(ExecMode::Exec, source_name("<stdin>"), "(unfinished".into(),),
        Err(SourceValidationError::Scan(_))
    ));
}

#[test]
fn eval_requires_exactly_one_form_while_exec_accepts_a_batch() {
    assert_eq!(
        Exec::new(ExecMode::Eval, source_name("<stdin>"), "".into())
            .err()
            .unwrap(),
        SourceValidationError::ExpectedOneForm { actual: 0 }
    );
    assert_eq!(
        Exec::new(ExecMode::Eval, source_name("<stdin>"), "a b".into())
            .err()
            .unwrap(),
        SourceValidationError::ExpectedOneForm { actual: 2 }
    );
    assert!(Exec::new(ExecMode::Eval, source_name("<stdin>"), "a".into()).is_ok());
    assert!(Exec::new(ExecMode::Exec, source_name("<stdin>"), "a b".into()).is_ok());
}

#[test]
fn bundled_lisp_sources_parse() {
    fn validate_file(path: &std::path::Path) {
        let source = std::fs::read_to_string(path).unwrap();
        if let Err(error) = acadctl_lisp::validate(&source) {
            panic!(
                "{}:{}:{}: {}",
                path.display(),
                error.line,
                error.column,
                error.kind.message()
            );
        }
    }

    fn validate_directory(directory: &std::path::Path) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                validate_directory(&path);
            } else if path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("lsp"))
            {
                validate_file(&path);
            }
        }
    }

    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    validate_directory(&manifest.join("lisp"));
    validate_file(&manifest.join("native/loader.lsp"));
}

#[test]
fn inspection_library_files_define_only_their_public_entry_points() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let cases = [
        ("dict.lsp", &["actl:dict", "actl:extdict"][..]),
        ("group.lsp", &["actl:groups"][..]),
        ("layer.lsp", &["actl:layers"][..]),
        ("order.lsp", &["actl:order"][..]),
    ];

    for (file, expected) in cases {
        let source = std::fs::read_to_string(manifest.join("lisp/lib").join(file)).unwrap();
        let definitions = source
            .lines()
            .filter_map(|line| line.strip_prefix("(defun "))
            .map(|line| line.split_whitespace().next().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(acadctl_lisp::validate(&source).unwrap(), expected.len());
        assert_eq!(definitions, expected, "unexpected definitions in {file}");
    }
}

#[test]
fn yields_exact_forms_then_commits() {
    let mut execution = Exec::new(
        ExecMode::Exec,
        source_name("batch.lsp"),
        "(setq x 1) ; keep with separator\n(+ x 2)".into(),
    )
    .unwrap()
    .0;

    assert_eq!(execution.take_step().kind(), ExecStepKind::BeginUndoGroup);
    assert!(execution.complete_step(success()));

    let first = execution.take_step();
    assert_eq!(first.kind(), ExecStepKind::EvaluateForm);
    assert_eq!(first.source(), "(setq x 1)");
    assert!(execution.complete_step(success()));

    let second = execution.take_step();
    assert_eq!(second.kind(), ExecStepKind::EvaluateForm);
    assert_eq!(second.source(), "(+ x 2)");
    assert!(execution.complete_step(success()));

    assert_eq!(execution.take_step().kind(), ExecStepKind::CommitUndoGroup);
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    assert_eq!(execution.outcome(), Some(&ExecOutcome::Success));
}

#[test]
fn eval_retains_its_form_value_and_emits_it_only_after_commit() {
    let (mut execution, _output) =
        Exec::new(ExecMode::Eval, source_name("inspect.lsp"), "(+ 1 2)".into()).unwrap();
    begin(&mut execution);

    let form = execution.take_step();
    assert_eq!(form.kind(), ExecStepKind::EvaluateForm);
    assert!(form.retain_value());
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::CommitUndoGroup);
    assert!(!execution.request_cancel());
    assert!(execution.complete_step(success()));

    assert_eq!(execution.take_step().kind(), ExecStepKind::EmitEvalValue);
    let lease = execution
        .acquire_eval_value_output()
        .expect("the post-commit value epoch is open");
    let mut port = NativeOutputPort::eval_value(lease);
    assert_eq!(
        port.write(Ok(OutputEvent::BeginValue)),
        WriteResult::Continue
    );
    assert_eq!(
        port.write(Ok(OutputEvent::Value(ValueEvent::Integer(3)))),
        WriteResult::Continue
    );
    assert_eq!(port.write(Ok(OutputEvent::EndValue)), WriteResult::Continue);
    assert_eq!(port.finish(), WriteResult::Continue);
    assert!(execution.complete_step(success()));

    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    assert_eq!(execution.outcome(), Some(&ExecOutcome::Success));
}

#[test]
fn exec_forms_never_request_value_retention() {
    let mut execution = Exec::new(ExecMode::Exec, source_name("batch.lsp"), "(+ 1 2)".into())
        .unwrap()
        .0;
    begin(&mut execution);

    let form = execution.take_step();
    assert_eq!(form.kind(), ExecStepKind::EvaluateForm);
    assert!(!form.retain_value());
}

#[test]
fn a_private_output_protocol_violation_rolls_back_its_form() {
    let (mut execution, _output) = Exec::new(
        ExecMode::Exec,
        source_name("batch.lsp"),
        "(actl:print value)".into(),
    )
    .unwrap();
    begin(&mut execution);
    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);

    let lease = execution
        .acquire_form_output()
        .expect("form output is open while AutoLISP runs");
    let mut port = NativeOutputPort::form(lease);
    assert_eq!(
        port.write(Ok(OutputEvent::BeginValue)),
        WriteResult::Continue
    );
    assert_eq!(
        port.write(Ok(OutputEvent::Value(ValueEvent::Integer(1)))),
        WriteResult::Continue
    );
    assert_eq!(
        port.write(Ok(OutputEvent::Value(ValueEvent::Integer(2)))),
        WriteResult::Continue
    );
    assert_eq!(
        port.write(Ok(OutputEvent::EndValue)),
        WriteResult::InvalidSequence
    );
    assert_eq!(port.finish(), WriteResult::InvalidSequence);

    assert!(execution.complete_step(success()));
    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::RollbackUndoGroup
    );
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);

    let Some(ExecOutcome::Failure(failure)) = execution.outcome() else {
        panic!("expected explicit output failure");
    };
    assert_eq!(
        failure.message,
        "the AutoLISP output bridge emitted an invalid value sequence"
    );
    assert_eq!(failure.drawing_outcome, DrawingOutcome::RolledBack);
}

#[test]
fn a_missing_post_commit_output_port_is_a_committed_failure() {
    let mut execution = eval_through_commit();
    assert_eq!(execution.take_step().kind(), ExecStepKind::EmitEvalValue);
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);

    assert_eq!(
        execution.outcome(),
        Some(&ExecOutcome::Failure(ExecFailure {
            message: "the AutoLISP evaluator did not emit its result value".into(),
            form_index: Some(1),
            location: Some(SourceLocation {
                source_name: source_name("inspect.lsp"),
                line: 1,
                column: 1,
            }),
            drawing_outcome: DrawingOutcome::Committed,
            drawing_error: None,
        }))
    );
}

#[test]
fn a_post_commit_native_failure_never_requests_rollback() {
    let mut execution = eval_through_commit();
    assert_eq!(execution.take_step().kind(), ExecStepKind::EmitEvalValue);
    assert!(execution.complete_step(native_error("value emitter failed", -5001)));

    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    let Some(ExecOutcome::Failure(failure)) = execution.outcome() else {
        panic!("expected committed serialization failure");
    };

    assert_eq!(failure.message, "value emitter failed");
    assert_eq!(failure.form_index, Some(1));
    assert_eq!(failure.drawing_outcome, DrawingOutcome::Committed);
}

#[test]
fn post_commit_failure_keeps_emitter_and_cleanup_evidence() {
    let mut execution = eval_through_commit();
    assert_eq!(execution.take_step().kind(), ExecStepKind::EmitEvalValue);
    assert!(execution.complete_step(with_cleanup(lisp_error("value emitter failed", 7), -5001,)));

    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    let Some(ExecOutcome::Failure(failure)) = execution.outcome() else {
        panic!("expected committed serialization failure");
    };

    assert_eq!(
        failure.message,
        "value emitter failed; could not clear the reserved AutoLISP execution bridge symbols (native status -5001)"
    );
    assert_eq!(failure.drawing_outcome, DrawingOutcome::Committed);
}

#[test]
fn post_commit_bridge_failure_keeps_cleanup_evidence() {
    let mut execution = eval_through_commit();
    assert_eq!(execution.take_step().kind(), ExecStepKind::EmitEvalValue);
    assert!(execution.complete_step(with_cleanup(success(), -5001)));

    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    let Some(ExecOutcome::Failure(failure)) = execution.outcome() else {
        panic!("expected committed serialization failure");
    };

    assert_eq!(
        failure.message,
        "the AutoLISP evaluator did not emit its result value; could not clear the reserved AutoLISP execution bridge symbols (native status -5001)"
    );
    assert_eq!(failure.drawing_outcome, DrawingOutcome::Committed);
}

#[test]
fn eval_cancellation_clears_the_retained_value_before_rollback() {
    let (mut execution, _output) =
        Exec::new(ExecMode::Eval, source_name("inspect.lsp"), "form".into()).unwrap();
    begin(&mut execution);
    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.request_cancel());
    assert!(execution.complete_step(success()));

    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::ClearRetainedEvalValue
    );
    assert!(execution.complete_step(success()));
    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::RollbackUndoGroup
    );
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    assert_eq!(execution.outcome(), Some(&ExecOutcome::Cancelled));
}

#[test]
fn eval_value_cleanup_failure_is_preserved_when_rollback_succeeds() {
    let (mut execution, _output) =
        Exec::new(ExecMode::Eval, source_name("inspect.lsp"), "form".into()).unwrap();
    begin(&mut execution);
    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.request_cancel());
    assert!(execution.complete_step(success()));

    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::ClearRetainedEvalValue
    );
    assert!(execution.complete_step(native_error("value cleanup failed", -5001)));
    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::RollbackUndoGroup
    );
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);

    let Some(ExecOutcome::Failure(failure)) = execution.outcome() else {
        panic!("expected cleanup failure");
    };

    assert_eq!(failure.message, "value cleanup failed");
    assert_eq!(failure.form_index, Some(1));
    assert_eq!(failure.drawing_outcome, DrawingOutcome::RolledBack);
}

#[test]
fn form_failure_keeps_lisp_and_cleanup_evidence() {
    let (mut execution, _output) =
        Exec::new(ExecMode::Eval, source_name("inspect.lsp"), "form".into()).unwrap();
    begin(&mut execution);
    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.complete_step(with_cleanup(lisp_error("bad argument type", 7), -5001,)));

    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::ClearRetainedEvalValue
    );
    assert!(execution.complete_step(success()));
    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::RollbackUndoGroup
    );
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);

    let Some(ExecOutcome::Failure(failure)) = execution.outcome() else {
        panic!("expected evaluation failure");
    };

    assert_eq!(
        failure.message,
        "bad argument type; could not clear the reserved AutoLISP execution bridge symbols (native status -5001)"
    );
    assert_eq!(failure.drawing_outcome, DrawingOutcome::RolledBack);
}

#[test]
fn rolls_back_a_lisp_failure_at_its_form_location() {
    let mut execution = Exec::new(ExecMode::Exec, source_name("batch.lsp"), "ok\n  bad".into())
        .unwrap()
        .0;
    begin(&mut execution);
    assert_eq!(execution.take_step().source(), "ok");
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().source(), "bad");
    assert!(execution.complete_step(lisp_error("bad argument type", 7)));

    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::RollbackUndoGroup
    );
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    assert_eq!(
        execution.outcome(),
        Some(&ExecOutcome::Failure(ExecFailure {
            message: "bad argument type".into(),
            form_index: Some(2),
            location: Some(SourceLocation {
                source_name: source_name("batch.lsp"),
                line: 2,
                column: 3,
            }),
            drawing_outcome: DrawingOutcome::RolledBack,
            drawing_error: None,
        }))
    );
}

#[test]
fn rollback_failure_preserves_the_original_error_and_marks_unknown() {
    let mut execution = Exec::new(ExecMode::Exec, source_name("batch.lsp"), "bad".into())
        .unwrap()
        .0;
    begin(&mut execution);
    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.complete_step(lisp_error("boom", 0)));
    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::RollbackUndoGroup
    );
    assert!(execution.complete_step(native_error("U failed", -5001)));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);

    let Some(ExecOutcome::Failure(failure)) = execution.outcome() else {
        panic!("expected failure");
    };

    assert_eq!(failure.message, "boom; U failed");
    assert_eq!(failure.drawing_outcome, DrawingOutcome::Unknown);
}

#[test]
fn commit_failure_is_rolled_back() {
    let mut execution = Exec::new(ExecMode::Exec, source_name("batch.lsp"), "ok".into())
        .unwrap()
        .0;
    begin(&mut execution);
    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::CommitUndoGroup);
    assert!(execution.complete_step(native_error("End failed", -5001)));
    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::RollbackUndoGroup
    );
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);

    assert_eq!(
        execution.outcome(),
        Some(&ExecOutcome::Failure(ExecFailure {
            message: "End failed".into(),
            form_index: None,
            location: None,
            drawing_outcome: DrawingOutcome::RolledBack,
            drawing_error: None,
        }))
    );
}

#[test]
fn eval_commit_failure_clears_its_value_before_rollback() {
    let mut execution = Exec::new(ExecMode::Eval, source_name("inspect.lsp"), "ok".into())
        .unwrap()
        .0;
    begin(&mut execution);
    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::CommitUndoGroup);
    assert!(execution.complete_step(native_error("End failed", -5001)));

    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::ClearRetainedEvalValue
    );
    assert!(execution.complete_step(success()));
    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::RollbackUndoGroup
    );
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    let Some(ExecOutcome::Failure(failure)) = execution.outcome() else {
        panic!("expected commit failure");
    };

    assert_eq!(failure.message, "End failed");
    assert_eq!(failure.drawing_outcome, DrawingOutcome::RolledBack);
}

#[test]
fn begin_failure_never_claims_that_drawing_work_started() {
    let mut execution = Exec::new(ExecMode::Exec, source_name("batch.lsp"), "ok".into())
        .unwrap()
        .0;
    assert_eq!(execution.take_step().kind(), ExecStepKind::BeginUndoGroup);
    assert!(execution.complete_step(native_error("Begin failed", -5001)));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);

    assert_eq!(
        execution.outcome(),
        Some(&ExecOutcome::Failure(ExecFailure {
            message: "Begin failed".into(),
            form_index: None,
            location: None,
            drawing_outcome: DrawingOutcome::NotStarted,
            drawing_error: None,
        }))
    );
}

#[test]
fn cleanup_failure_overrides_success_with_an_unknown_drawing_outcome() {
    let mut execution = successful_execution();
    assert!(execution.record_bridge_finalization_failure(native_error("unlock failed", 42)));
    assert_eq!(
        execution.outcome(),
        Some(&ExecOutcome::Failure(ExecFailure {
            message: "unlock failed".into(),
            form_index: None,
            location: None,
            drawing_outcome: DrawingOutcome::Unknown,
            drawing_error: None,
        }))
    );
}

#[test]
fn cleanup_failure_preserves_an_existing_execution_failure() {
    let mut execution = Exec::new(ExecMode::Exec, source_name("batch.lsp"), "bad".into())
        .unwrap()
        .0;
    begin(&mut execution);
    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.complete_step(lisp_error("boom", 0)));
    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::RollbackUndoGroup
    );
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);

    assert!(execution.record_bridge_finalization_failure(native_error("restore failed", 43)));
    let Some(ExecOutcome::Failure(failure)) = execution.outcome() else {
        panic!("expected failure");
    };

    assert_eq!(failure.message, "boom; restore failed");
    assert_eq!(failure.drawing_outcome, DrawingOutcome::Unknown);
}

#[test]
fn abandonment_terminalizes_an_in_flight_form_as_unknown() {
    let mut execution = Exec::new(
        ExecMode::Exec,
        source_name("batch.lsp"),
        "ok\nchanged".into(),
    )
    .unwrap()
    .0;
    begin(&mut execution);
    assert_eq!(execution.take_step().source(), "ok");
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().source(), "changed");

    assert!(execution.abandon(native_error(
        "the target database changed during execution",
        -5001,
    )));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    assert_eq!(
        execution.outcome(),
        Some(&ExecOutcome::Failure(ExecFailure {
            message: "the target database changed during execution".into(),
            form_index: Some(2),
            location: Some(SourceLocation {
                source_name: source_name("batch.lsp"),
                line: 2,
                column: 1,
            }),
            drawing_outcome: DrawingOutcome::Unknown,
            drawing_error: None,
        }))
    );
}

#[test]
fn abandonment_preserves_the_failure_that_started_rollback() {
    let mut execution = Exec::new(ExecMode::Exec, source_name("batch.lsp"), "bad".into())
        .unwrap()
        .0;
    begin(&mut execution);
    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.complete_step(lisp_error("boom", 0)));
    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::RollbackUndoGroup
    );

    assert!(execution.abandon(native_error("database replaced", -5001)));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    let Some(ExecOutcome::Failure(failure)) = execution.outcome() else {
        panic!("expected failure");
    };

    assert_eq!(failure.message, "boom; database replaced");
    assert_eq!(failure.drawing_outcome, DrawingOutcome::Unknown);
}

#[test]
fn cancellation_before_begin_never_opens_an_undo_group() {
    let (mut execution, _output) =
        Exec::new(ExecMode::Exec, source_name("batch.lsp"), "form".into()).unwrap();

    assert!(execution.request_cancel());
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    assert_eq!(execution.outcome(), Some(&ExecOutcome::Cancelled));
}

#[test]
fn cancellation_during_begin_closes_the_empty_group_without_u() {
    let (mut execution, _output) =
        Exec::new(ExecMode::Exec, source_name("batch.lsp"), "form".into()).unwrap();

    assert_eq!(execution.take_step().kind(), ExecStepKind::BeginUndoGroup);
    assert!(execution.request_cancel());
    assert!(execution.complete_step(success()));
    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::CloseEmptyUndoGroup
    );
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    assert_eq!(execution.outcome(), Some(&ExecOutcome::Cancelled));
}

#[test]
fn cancellation_after_a_form_uses_rollback() {
    let (mut execution, _output) =
        Exec::new(ExecMode::Exec, source_name("batch.lsp"), "form".into()).unwrap();
    begin(&mut execution);

    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.request_cancel());
    assert!(execution.complete_step(success()));
    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::RollbackUndoGroup
    );
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    assert_eq!(execution.outcome(), Some(&ExecOutcome::Cancelled));
}

#[test]
fn evaluator_failure_wins_over_concurrent_cancellation() {
    let (mut execution, _output) =
        Exec::new(ExecMode::Exec, source_name("batch.lsp"), "bad".into()).unwrap();
    begin(&mut execution);

    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.request_cancel());
    assert!(execution.complete_step(lisp_error("boom", 0)));
    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::RollbackUndoGroup
    );
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    let Some(ExecOutcome::Failure(failure)) = execution.outcome() else {
        panic!("expected the evaluator failure");
    };

    assert_eq!(failure.message, "boom");
    assert_eq!(failure.drawing_outcome, DrawingOutcome::RolledBack);
}

#[test]
fn cancellation_after_commit_handoff_is_too_late() {
    let (mut execution, _output) =
        Exec::new(ExecMode::Exec, source_name("batch.lsp"), "form".into()).unwrap();
    begin(&mut execution);
    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.complete_step(success()));

    assert_eq!(execution.take_step().kind(), ExecStepKind::CommitUndoGroup);
    assert!(!execution.request_cancel());
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    assert_eq!(execution.outcome(), Some(&ExecOutcome::Success));
}

#[test]
fn rollback_failure_overrides_cancellation() {
    let (mut execution, _output) =
        Exec::new(ExecMode::Exec, source_name("batch.lsp"), "form".into()).unwrap();
    begin(&mut execution);
    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.request_cancel());
    assert!(execution.complete_step(success()));
    assert_eq!(
        execution.take_step().kind(),
        ExecStepKind::RollbackUndoGroup
    );
    assert!(execution.complete_step(native_error("U failed", -5001)));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);

    assert_eq!(
        execution.outcome(),
        Some(&ExecOutcome::Failure(ExecFailure {
            message: "U failed".into(),
            form_index: None,
            location: None,
            drawing_outcome: DrawingOutcome::Unknown,
            drawing_error: None,
        }))
    );
}

#[test]
fn empty_batch_finishes_without_an_undo_group() {
    let mut execution = Exec::new(
        ExecMode::Exec,
        source_name("<stdin>"),
        "; only a comment".into(),
    )
    .unwrap()
    .0;

    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    assert_eq!(execution.outcome(), Some(&ExecOutcome::Success));
}

fn begin(execution: &mut Exec) {
    assert_eq!(execution.take_step().kind(), ExecStepKind::BeginUndoGroup);
    assert!(execution.complete_step(success()));
}

fn successful_execution() -> Exec {
    let mut execution = Exec::new(ExecMode::Exec, source_name("batch.lsp"), "ok".into())
        .unwrap()
        .0;
    begin(&mut execution);
    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::CommitUndoGroup);
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::Done);
    execution
}

fn eval_through_commit() -> Exec {
    let mut execution = Exec::new(ExecMode::Eval, source_name("inspect.lsp"), "(+ 1 2)".into())
        .unwrap()
        .0;
    begin(&mut execution);
    assert_eq!(execution.take_step().kind(), ExecStepKind::EvaluateForm);
    assert!(execution.complete_step(success()));
    assert_eq!(execution.take_step().kind(), ExecStepKind::CommitUndoGroup);
    assert!(execution.complete_step(success()));
    execution
}

fn success() -> ExecStepResult {
    ExecStepResult {
        kind: ExecStepResultKind::Success,
        native_status: 0,
        lisp_errno: 0,
        detail: String::new(),
        bridge_symbols_clear_status: 0,
    }
}

fn lisp_error(detail: &str, lisp_errno: i32) -> ExecStepResult {
    ExecStepResult {
        kind: ExecStepResultKind::LispError,
        native_status: 0,
        lisp_errno,
        detail: detail.into(),
        bridge_symbols_clear_status: 0,
    }
}

fn native_error(detail: &str, native_status: i32) -> ExecStepResult {
    ExecStepResult {
        kind: ExecStepResultKind::NativeError,
        native_status,
        lisp_errno: 0,
        detail: detail.into(),
        bridge_symbols_clear_status: 0,
    }
}

fn with_cleanup(mut result: ExecStepResult, bridge_symbols_clear_status: i32) -> ExecStepResult {
    result.bridge_symbols_clear_status = bridge_symbols_clear_status;
    result
}
