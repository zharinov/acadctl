use crate::exec::protocol::OutputEventCode;

use super::port::ValueEvent;

pub(crate) enum OutputEvent<'a> {
    BeginValue,
    EndValue,
    Label(&'a str),
    InvalidLabel,
    Value(ValueEvent<'a>),
}

pub(crate) enum Payload<'a> {
    Nil,
    Integer(i64),
    Real(f64),
    String(&'a str),
    Entity(Option<&'a str>),
}

impl OutputEventCode {
    const ALL: [Self; 26] = [
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
        Self::BeginValue,
        Self::EndValue,
        Self::Label,
        Self::InvalidLabel,
    ];

    fn decode<'a>(self, payload: Payload<'a>) -> Result<OutputEvent<'a>, ProtocolViolation> {
        use OutputEvent::Value;

        match (self, payload) {
            (Self::BeginList, Payload::Nil) => Ok(Value(ValueEvent::BeginList)),
            (Self::EndList, Payload::Nil) => Ok(Value(ValueEvent::EndList)),
            (Self::Dot, Payload::Nil) => Ok(Value(ValueEvent::Dot)),
            (Self::Nil, Payload::Nil) => Ok(Value(ValueEvent::Nil)),
            (Self::True, Payload::Nil) => Ok(Value(ValueEvent::True)),
            (Self::Integer, Payload::Integer(value)) => Ok(Value(ValueEvent::Integer(value))),
            (Self::Real, Payload::Real(value)) => Ok(Value(ValueEvent::Real(value))),

            (Self::BeginString, Payload::Nil) => Ok(Value(ValueEvent::BeginString)),
            (Self::StringChunk, Payload::String(text)) => Ok(Value(ValueEvent::StringChunk(text))),
            (Self::EndString, Payload::Nil) => Ok(Value(ValueEvent::EndString)),

            (Self::BeginSymbol, Payload::Nil) => Ok(Value(ValueEvent::BeginSymbol)),
            (Self::SymbolChunk, Payload::String(text)) => Ok(Value(ValueEvent::SymbolChunk(text))),
            (Self::EndSymbol, Payload::Nil) => Ok(Value(ValueEvent::EndSymbol)),

            (Self::Entity, Payload::Entity(handle)) => Ok(Value(ValueEvent::Entity(handle))),
            (Self::SelectionSet, Payload::Nil) => Ok(Value(ValueEvent::SelectionSet)),
            (Self::VlaObject, Payload::Nil) => Ok(Value(ValueEvent::VlaObject)),
            (Self::File, Payload::Nil) => Ok(Value(ValueEvent::File)),
            (Self::Function, Payload::Nil) => Ok(Value(ValueEvent::Function)),
            (Self::ErrorObject, Payload::Nil) => Ok(Value(ValueEvent::ErrorObject)),
            (Self::Object, Payload::String(label)) => Ok(Value(ValueEvent::Object(Some(label)))),

            (Self::Cycle, Payload::Nil) => Ok(Value(ValueEvent::Cycle)),
            (Self::TooDeep, Payload::Nil) => Ok(Value(ValueEvent::TooDeep)),
            (Self::BeginValue, Payload::Nil) => Ok(OutputEvent::BeginValue),
            (Self::EndValue, Payload::Nil) => Ok(OutputEvent::EndValue),
            (Self::Label, Payload::String(text)) => Ok(OutputEvent::Label(text)),
            (Self::InvalidLabel, Payload::Nil) => Ok(OutputEvent::InvalidLabel),

            _ => Err(ProtocolViolation),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProtocolViolation;

impl TryFrom<i32> for OutputEventCode {
    type Error = ProtocolViolation;

    fn try_from(code: i32) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|event| *event as i32 == code)
            .ok_or(ProtocolViolation)
    }
}

pub(crate) fn output_event<'a>(
    code: i32,
    payload: Payload<'a>,
) -> Result<OutputEvent<'a>, ProtocolViolation> {
    OutputEventCode::try_from(code)?.decode(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owns_private_event_codes_and_payload_validation() {
        assert!(matches!(
            output_event(OutputEventCode::StringChunk as i32, Payload::String("x")),
            Ok(OutputEvent::Value(ValueEvent::StringChunk("x")))
        ));
        assert!(matches!(
            output_event(OutputEventCode::StringChunk as i32, Payload::Nil),
            Err(ProtocolViolation)
        ));
        assert!(matches!(
            output_event(OutputEventCode::Cycle as i32, Payload::Nil),
            Ok(OutputEvent::Value(ValueEvent::Cycle))
        ));
        assert!(matches!(
            output_event(OutputEventCode::Label as i32, Payload::String("x")),
            Ok(OutputEvent::Label("x"))
        ));
    }
}
