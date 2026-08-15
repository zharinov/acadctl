use std::fmt::Write;
use std::sync::LazyLock;

use super::value_bridge::ValueEvent;

const TEMPLATE: &str = include_str!("../../lisp/eval-value-visitor.lsp");
const CALLBACK_NAME: &str = "acadctl:_value-event";
pub(crate) const MAX_DEPTH: usize = 4096;
pub(crate) const CHUNK_CHARACTERS: usize = 2048;
static PROGRAM: LazyLock<Program> = LazyLock::new(Program::new);

struct Program {
    source: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub(crate) enum EventCode {
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
}

pub(crate) enum Payload<'a> {
    Invalid,
    Nil,
    Integer(i64),
    Real(f64),
    String(&'a str),
    Entity(Option<&'a str>),
}

impl Program {
    fn new() -> Self {
        Self {
            source: render(CALLBACK_NAME),
        }
    }
}

impl EventCode {
    const ALL: [Self; 22] = [
        Self::BeginList,
        Self::EndList,
        Self::Dot,
        Self::Nil,
        Self::True,
        Self::Integer,
        Self::Real,
        Self::BeginString,
        Self::StringChunk,
        Self::EndString,
        Self::BeginSymbol,
        Self::SymbolChunk,
        Self::EndSymbol,
        Self::Entity,
        Self::SelectionSet,
        Self::VlaObject,
        Self::File,
        Self::Function,
        Self::ErrorObject,
        Self::Object,
        Self::Cycle,
        Self::TooDeep,
    ];

    pub(crate) fn from_raw(code: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|event| *event as i32 == code)
    }
}

pub(crate) fn source() -> &'static str {
    &PROGRAM.source
}

pub(crate) fn value_event<'a>(code: i32, payload: Payload<'a>) -> ValueEvent<'a> {
    match (EventCode::from_raw(code), payload) {
        (Some(EventCode::BeginList), Payload::Nil) => ValueEvent::BeginList,
        (Some(EventCode::EndList), Payload::Nil) => ValueEvent::EndList,
        (Some(EventCode::Dot), Payload::Nil) => ValueEvent::Dot,
        (Some(EventCode::Nil), Payload::Nil) => ValueEvent::Nil,
        (Some(EventCode::True), Payload::Nil) => ValueEvent::True,
        (Some(EventCode::Integer), Payload::Integer(value)) => ValueEvent::Integer(value),
        (Some(EventCode::Real), Payload::Real(value)) => ValueEvent::Real(value),
        (Some(EventCode::BeginString), Payload::Nil) => ValueEvent::BeginString,
        (Some(EventCode::StringChunk), Payload::String(text)) => ValueEvent::StringChunk(text),
        (Some(EventCode::EndString), Payload::Nil) => ValueEvent::EndString,
        (Some(EventCode::BeginSymbol), Payload::Nil) => ValueEvent::BeginSymbol,
        (Some(EventCode::SymbolChunk), Payload::String(text)) => ValueEvent::SymbolChunk(text),
        (Some(EventCode::EndSymbol), Payload::Nil) => ValueEvent::EndSymbol,
        (Some(EventCode::Entity), Payload::Entity(handle)) => ValueEvent::Entity(handle),
        (Some(EventCode::SelectionSet), Payload::Nil) => ValueEvent::SelectionSet,
        (Some(EventCode::VlaObject), Payload::Nil) => ValueEvent::VlaObject,
        (Some(EventCode::File), Payload::Nil) => ValueEvent::File,
        (Some(EventCode::Function), Payload::Nil) => ValueEvent::Function,
        (Some(EventCode::ErrorObject), Payload::Nil) => ValueEvent::ErrorObject,
        (Some(EventCode::Object), Payload::String(label)) => ValueEvent::Object(Some(label)),
        (Some(EventCode::Cycle), Payload::Nil) => ValueEvent::Cycle,
        (Some(EventCode::TooDeep), Payload::Nil) => ValueEvent::TooDeep,
        _ => ValueEvent::Invalid,
    }
}

fn render(callback_name: &str) -> String {
    let mut output = String::with_capacity(TEMPLATE.len());
    let mut remaining = TEMPLATE;

    while let Some(start) = remaining.find("{{") {
        output.push_str(&remaining[..start]);
        let marker = &remaining[start + 2..];
        let end = marker
            .find("}}")
            .expect("embedded eval value visitor marker is closed");
        write_marker(&mut output, &marker[..end], callback_name);
        remaining = &marker[end + 2..];
    }

    output.push_str(remaining);
    output
}

fn write_marker(output: &mut String, marker: &str, callback_name: &str) {
    let value = match marker {
        "CALLBACK" => {
            output.push_str(callback_name);

            return;
        }
        "MAX_DEPTH" => MAX_DEPTH,
        "CHUNK_CHARS" => CHUNK_CHARACTERS,
        "BEGIN_LIST" => EventCode::BeginList as usize,
        "END_LIST" => EventCode::EndList as usize,
        "DOT" => EventCode::Dot as usize,
        "NIL" => EventCode::Nil as usize,
        "TRUE" => EventCode::True as usize,
        "INTEGER" => EventCode::Integer as usize,
        "REAL" => EventCode::Real as usize,
        "BEGIN_STRING" => EventCode::BeginString as usize,
        "STRING_CHUNK" => EventCode::StringChunk as usize,
        "END_STRING" => EventCode::EndString as usize,
        "BEGIN_SYMBOL" => EventCode::BeginSymbol as usize,
        "SYMBOL_CHUNK" => EventCode::SymbolChunk as usize,
        "END_SYMBOL" => EventCode::EndSymbol as usize,
        "ENTITY" => EventCode::Entity as usize,
        "SELECTION_SET" => EventCode::SelectionSet as usize,
        "VLA_OBJECT" => EventCode::VlaObject as usize,
        "FILE" => EventCode::File as usize,
        "FUNCTION" => EventCode::Function as usize,
        "ERROR_OBJECT" => EventCode::ErrorObject as usize,
        "OBJECT" => EventCode::Object as usize,
        "CYCLE" => EventCode::Cycle as usize,
        "TOO_DEEP" => EventCode::TooDeep as usize,
        _ => panic!("unknown embedded eval value visitor marker: {marker}"),
    };

    write!(output, "{value}").expect("writing to a String cannot fail");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_one_complete_form_from_rust_owned_protocol_values() {
        let source = source();

        assert_eq!(acadctl_lisp::validate(source), Ok(1));
        assert!(!source.contains("{{"));
        assert!(source.contains(CALLBACK_NAME));
    }

    #[test]
    fn owns_private_event_codes_and_payload_validation() {
        assert!(matches!(
            value_event(EventCode::StringChunk as i32, Payload::String("x")),
            ValueEvent::StringChunk("x")
        ));
        assert!(matches!(
            value_event(EventCode::StringChunk as i32, Payload::Nil),
            ValueEvent::Invalid
        ));
        assert!(matches!(
            value_event(EventCode::Cycle as i32, Payload::Nil),
            ValueEvent::Cycle
        ));
        assert!(matches!(
            value_event(EventCode::TooDeep as i32, Payload::Nil),
            ValueEvent::TooDeep
        ));
    }
}
