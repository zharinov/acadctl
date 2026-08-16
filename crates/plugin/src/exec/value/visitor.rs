use std::fmt::Write;
use std::sync::LazyLock;

use crate::exec::protocol;

use super::writer::ValueEvent;

const TEMPLATE: &str = include_str!("../../../lisp/exec/visitor.lsp");
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
        Self { source: render() }
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

    fn decode<'a>(self, payload: Payload<'a>) -> ValueEvent<'a> {
        match (self, payload) {
            (Self::BeginList, Payload::Nil) => ValueEvent::BeginList,
            (Self::EndList, Payload::Nil) => ValueEvent::EndList,
            (Self::Dot, Payload::Nil) => ValueEvent::Dot,
            (Self::Nil, Payload::Nil) => ValueEvent::Nil,
            (Self::True, Payload::Nil) => ValueEvent::True,
            (Self::Integer, Payload::Integer(value)) => ValueEvent::Integer(value),
            (Self::Real, Payload::Real(value)) => ValueEvent::Real(value),

            (Self::BeginString, Payload::Nil) => ValueEvent::BeginString,
            (Self::StringChunk, Payload::String(text)) => ValueEvent::StringChunk(text),
            (Self::EndString, Payload::Nil) => ValueEvent::EndString,

            (Self::BeginSymbol, Payload::Nil) => ValueEvent::BeginSymbol,
            (Self::SymbolChunk, Payload::String(text)) => ValueEvent::SymbolChunk(text),
            (Self::EndSymbol, Payload::Nil) => ValueEvent::EndSymbol,

            (Self::Entity, Payload::Entity(handle)) => ValueEvent::Entity(handle),
            (Self::SelectionSet, Payload::Nil) => ValueEvent::SelectionSet,
            (Self::VlaObject, Payload::Nil) => ValueEvent::VlaObject,
            (Self::File, Payload::Nil) => ValueEvent::File,
            (Self::Function, Payload::Nil) => ValueEvent::Function,
            (Self::ErrorObject, Payload::Nil) => ValueEvent::ErrorObject,
            (Self::Object, Payload::String(label)) => ValueEvent::Object(Some(label)),

            (Self::Cycle, Payload::Nil) => ValueEvent::Cycle,
            (Self::TooDeep, Payload::Nil) => ValueEvent::TooDeep,

            _ => ValueEvent::Invalid,
        }
    }
}

impl TryFrom<i32> for EventCode {
    type Error = ();

    fn try_from(code: i32) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|event| *event as i32 == code)
            .ok_or(())
    }
}

pub(crate) fn source() -> &'static str {
    &PROGRAM.source
}

pub(crate) fn value_event<'a>(code: i32, payload: Payload<'a>) -> ValueEvent<'a> {
    EventCode::try_from(code).map_or(ValueEvent::Invalid, |event| event.decode(payload))
}

fn render() -> String {
    let mut output = String::with_capacity(TEMPLATE.len());
    let mut remaining = TEMPLATE;

    while let Some(start) = remaining.find("{{") {
        output.push_str(&remaining[..start]);
        let marker = &remaining[start + 2..];
        let end = marker
            .find("}}")
            .expect("embedded eval value visitor marker is closed");
        write_marker(&mut output, &marker[..end]);
        remaining = &marker[end + 2..];
    }

    output.push_str(remaining);
    output
}

fn write_marker(output: &mut String, marker: &str) {
    let text = match marker {
        "CALLBACK" => Some(protocol::VALUE_EVENT_FUNCTION),
        "VALUE_SYMBOL" => Some(protocol::VALUE_SYMBOL),
        "STATUS_SYMBOL" => Some(protocol::STATUS_SYMBOL),
        "ERROR_SYMBOL" => Some(protocol::ERROR_SYMBOL),
        "ERRNO_SYMBOL" => Some(protocol::ERRNO_SYMBOL),
        _ => None,
    };

    if let Some(text) = text {
        output.push_str(text);

        return;
    }

    let value = match marker {
        "MAX_DEPTH" => protocol::VALUE_MAX_DEPTH,
        "CHUNK_CHARS" => protocol::VALUE_CHUNK_CHARACTERS,
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
    fn embedded_value_visitor_is_one_complete_form() {
        assert_eq!(acadctl_lisp::validate(source()), Ok(1));
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
