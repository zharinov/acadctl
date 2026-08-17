#[allow(dead_code, reason = "the Cargo build script renders this program")]
const EXECUTION_DRIVER_TEMPLATE: &str = include_str!("../../lisp/exec/driver.lsp");
const FORM_EVALUATOR_TEMPLATE: &str = include_str!("../../lisp/exec/evaluator.lsp");
const VALUE_EMITTER_TEMPLATE: &str = include_str!("../../lisp/exec/emitter.lsp");

pub const OUTPUT_EVENT_FUNCTION: &str = "actl:_output-event";
pub const ADVANCE_EXECUTION_FUNCTION: &str = "actl:_advance-execution";
pub const DRIVE_EXECUTION_FUNCTION: &str = "actl:_drive-execution";
pub const EMIT_VALUE_FUNCTION: &str = "actl:_emit-value";
pub const EMIT_RETAINED_VALUE_FUNCTION: &str = "actl:_emit-retained-value";
pub const INVALID_FORM_SPAN_FUNCTION: &str = "actl:_invalid-form-span";

pub const SOURCE_SYMBOL: &str = "actl:*bridge-source*";
pub const STAGED_FORM_SYMBOL: &str = "actl:*bridge-staged-form*";
pub const STATUS_SYMBOL: &str = "actl:*bridge-status*";
pub const ERROR_SYMBOL: &str = "actl:*bridge-error*";
pub const ERRNO_SYMBOL: &str = "actl:*bridge-errno*";
pub const VALUE_SYMBOL: &str = "actl:*bridge-value*";
pub const PENDING_STATUS: &str = "pending";

pub const VALUE_MAX_DEPTH: usize = 4096;
pub const VALUE_CHUNK_CHARACTERS: usize = 2048;
pub const NATIVE_VALUE_CHUNK_CAPTURE_UNITS: usize = VALUE_CHUNK_CHARACTERS * 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub(crate) enum OutputEventCode {
    BeginList = 1,
    EndList = 2,
    Dot = 3,
    Nil = 4,
    True = 5,
    Integer = 6,
    Real = 7,
    BeginString = 8,
    StringChunk = 9,
    EndString = 10,
    BeginSymbol = 11,
    SymbolChunk = 12,
    EndSymbol = 13,
    Entity = 14,
    SelectionSet = 15,
    VlaObject = 16,
    File = 17,
    Function = 18,
    ErrorObject = 19,
    Object = 20,
    Cycle = 21,
    TooDeep = 22,
    BeginValue = 23,
    EndValue = 24,
    Label = 25,
    InvalidLabel = 26,
}

pub fn execution_driver_expression() -> String {
    format!("({DRIVE_EXECUTION_FUNCTION})")
}

pub fn execution_driver_invocation() -> String {
    format!("{}\n", execution_driver_expression())
}

pub fn eval_value_emitter_expression() -> String {
    format!("({EMIT_RETAINED_VALUE_FUNCTION})")
}

#[allow(dead_code, reason = "the Cargo build script renders this program")]
pub fn execution_driver_source() -> String {
    let emitter = value_emitter_source();
    render(
        EXECUTION_DRIVER_TEMPLATE,
        &[
            ("ADVANCE_EXECUTION_FUNCTION", ADVANCE_EXECUTION_FUNCTION),
            ("DRIVE_EXECUTION_FUNCTION", DRIVE_EXECUTION_FUNCTION),
            ("EMIT_RETAINED_VALUE_FUNCTION", EMIT_RETAINED_VALUE_FUNCTION),
            ("EMIT_VALUE_FUNCTION", EMIT_VALUE_FUNCTION),
            ("ERRNO_SYMBOL", ERRNO_SYMBOL),
            ("ERROR_SYMBOL", ERROR_SYMBOL),
            (
                "INVALID_LABEL_EVENT",
                &(OutputEventCode::InvalidLabel as i32).to_string(),
            ),
            ("LABEL_EVENT", &(OutputEventCode::Label as i32).to_string()),
            ("OUTPUT_EVENT_FUNCTION", OUTPUT_EVENT_FUNCTION),
            ("STATUS_SYMBOL", STATUS_SYMBOL),
            ("STAGED_FORM_SYMBOL", STAGED_FORM_SYMBOL),
            ("VALUE_EMITTER", &emitter),
            ("VALUE_SYMBOL", VALUE_SYMBOL),
        ],
    )
}

fn value_emitter_source() -> String {
    render(
        VALUE_EMITTER_TEMPLATE,
        &[
            (
                "BEGIN_LIST",
                &(OutputEventCode::BeginList as i32).to_string(),
            ),
            (
                "BEGIN_STRING",
                &(OutputEventCode::BeginString as i32).to_string(),
            ),
            (
                "BEGIN_SYMBOL",
                &(OutputEventCode::BeginSymbol as i32).to_string(),
            ),
            (
                "BEGIN_VALUE",
                &(OutputEventCode::BeginValue as i32).to_string(),
            ),
            ("CALLBACK", OUTPUT_EVENT_FUNCTION),
            ("CHUNK_CHARS", &VALUE_CHUNK_CHARACTERS.to_string()),
            ("CYCLE", &(OutputEventCode::Cycle as i32).to_string()),
            ("DOT", &(OutputEventCode::Dot as i32).to_string()),
            ("EMIT_VALUE_FUNCTION", EMIT_VALUE_FUNCTION),
            ("END_LIST", &(OutputEventCode::EndList as i32).to_string()),
            (
                "END_STRING",
                &(OutputEventCode::EndString as i32).to_string(),
            ),
            (
                "END_SYMBOL",
                &(OutputEventCode::EndSymbol as i32).to_string(),
            ),
            ("END_VALUE", &(OutputEventCode::EndValue as i32).to_string()),
            ("ENTITY", &(OutputEventCode::Entity as i32).to_string()),
            (
                "ERROR_OBJECT",
                &(OutputEventCode::ErrorObject as i32).to_string(),
            ),
            ("FILE", &(OutputEventCode::File as i32).to_string()),
            ("FUNCTION", &(OutputEventCode::Function as i32).to_string()),
            ("INTEGER", &(OutputEventCode::Integer as i32).to_string()),
            ("MAX_DEPTH", &VALUE_MAX_DEPTH.to_string()),
            ("NIL", &(OutputEventCode::Nil as i32).to_string()),
            ("OBJECT", &(OutputEventCode::Object as i32).to_string()),
            ("REAL", &(OutputEventCode::Real as i32).to_string()),
            (
                "SELECTION_SET",
                &(OutputEventCode::SelectionSet as i32).to_string(),
            ),
            (
                "STRING_CHUNK",
                &(OutputEventCode::StringChunk as i32).to_string(),
            ),
            (
                "SYMBOL_CHUNK",
                &(OutputEventCode::SymbolChunk as i32).to_string(),
            ),
            ("TOO_DEEP", &(OutputEventCode::TooDeep as i32).to_string()),
            ("TRUE", &(OutputEventCode::True as i32).to_string()),
            (
                "VLA_OBJECT",
                &(OutputEventCode::VlaObject as i32).to_string(),
            ),
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
