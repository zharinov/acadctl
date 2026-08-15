#[allow(dead_code, reason = "the Cargo build script renders this program")]
const EXECUTION_DRIVER_TEMPLATE: &str = include_str!("lisp/execution-driver.lsp");
const FORM_EVALUATOR_TEMPLATE: &str = include_str!("lisp/form-evaluator.lsp");

pub const VALUE_EVENT_FUNCTION: &str = "acadctl:_value-event";
pub const ADVANCE_EXECUTION_FUNCTION: &str = "acadctl:_advance-execution";
pub const DRIVE_EXECUTION_FUNCTION: &str = "acadctl:_drive-execution";
pub const INVALID_FORM_SPAN_FUNCTION: &str = "acadctl:_invalid-form-span";

pub const SOURCE_SYMBOL: &str = "acadctl:*bridge-source*";
pub const STAGED_FORM_SYMBOL: &str = "acadctl:*bridge-staged-form*";
pub const STATUS_SYMBOL: &str = "acadctl:*bridge-status*";
pub const ERROR_SYMBOL: &str = "acadctl:*bridge-error*";
pub const ERRNO_SYMBOL: &str = "acadctl:*bridge-errno*";
pub const VALUE_SYMBOL: &str = "acadctl:*bridge-value*";
pub const PENDING_STATUS: &str = "pending";

pub const VALUE_MAX_DEPTH: usize = 4096;
pub const VALUE_CHUNK_CHARACTERS: usize = 2048;
pub const NATIVE_VALUE_CHUNK_CAPTURE_UNITS: usize = VALUE_CHUNK_CHARACTERS * 2;

pub fn execution_driver_expression() -> String {
    format!("({DRIVE_EXECUTION_FUNCTION})")
}

pub fn execution_driver_invocation() -> String {
    format!("{}\n", execution_driver_expression())
}

#[allow(dead_code, reason = "the Cargo build script renders this program")]
pub fn execution_driver_source() -> String {
    render(
        EXECUTION_DRIVER_TEMPLATE,
        &[
            ("ADVANCE_EXECUTION_FUNCTION", ADVANCE_EXECUTION_FUNCTION),
            ("DRIVE_EXECUTION_FUNCTION", DRIVE_EXECUTION_FUNCTION),
            ("STAGED_FORM_SYMBOL", STAGED_FORM_SYMBOL),
        ],
    )
}

pub fn form_evaluator_source() -> String {
    render(
        FORM_EVALUATOR_TEMPLATE,
        &[
            ("SOURCE_SYMBOL", SOURCE_SYMBOL),
            ("STATUS_SYMBOL", STATUS_SYMBOL),
            ("ERROR_SYMBOL", ERROR_SYMBOL),
            ("ERRNO_SYMBOL", ERRNO_SYMBOL),
            ("VALUE_SYMBOL", VALUE_SYMBOL),
            ("INVALID_FORM_SPAN_FUNCTION", INVALID_FORM_SPAN_FUNCTION),
        ],
    )
}

fn render(template: &str, values: &[(&str, &str)]) -> String {
    let mut source = template.to_owned();

    for (marker, value) in values {
        let placeholder = format!("{{{{{marker}}}}}");
        assert!(
            source.contains(&placeholder),
            "unused private AutoLISP protocol marker: {marker}"
        );
        source = source.replace(&placeholder, value);
    }

    assert!(
        !source.contains("{{"),
        "unknown private AutoLISP protocol marker"
    );
    source
}
