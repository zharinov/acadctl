use std::fmt;
use std::num::NonZeroU16;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DrawingId(NonZeroU16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseDrawingIdError;

impl DrawingId {
    pub const HEX_WIDTH: usize = 4;

    pub const fn new(value: u16) -> Option<Self> {
        match NonZeroU16::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl From<DrawingId> for u32 {
    fn from(value: DrawingId) -> Self {
        value.get().into()
    }
}

impl TryFrom<u32> for DrawingId {
    type Error = ParseDrawingIdError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        let value = u16::try_from(value).map_err(|_| ParseDrawingIdError)?;
        Self::new(value).ok_or(ParseDrawingIdError)
    }
}

impl fmt::Display for DrawingId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:04X}", self.get())
    }
}

impl FromStr for DrawingId {
    type Err = ParseDrawingIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != Self::HEX_WIDTH || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ParseDrawingIdError);
        }

        let value = u16::from_str_radix(value, 16).map_err(|_| ParseDrawingIdError)?;
        Self::new(value).ok_or(ParseDrawingIdError)
    }
}

impl fmt::Display for ParseDrawingIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected a nonzero drawing ID of exactly 4 hexadecimal digits")
    }
}

impl std::error::Error for ParseDrawingIdError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_as_four_uppercase_hexadecimal_digits() {
        assert_eq!(DrawingId::new(0x2A79).unwrap().to_string(), "2A79");
        assert_eq!(DrawingId::new(u16::MAX).unwrap().to_string(), "FFFF");
    }

    #[test]
    fn parses_case_insensitively_and_normalizes_on_display() {
        assert_eq!("2a79".parse(), Ok(DrawingId::new(0x2A79).unwrap()));
    }

    #[test]
    fn rejects_values_outside_the_public_shape() {
        assert_eq!(DrawingId::new(0), None);

        for value in ["", "0000", "2A7", "02A79", "2A7G", "+A79", " A79"] {
            assert!(value.parse::<DrawingId>().is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn converts_to_and_from_the_wire_integer() {
        assert_eq!(u32::from(DrawingId::new(0x2A79).unwrap()), 0x2A79);
        assert_eq!(
            DrawingId::try_from(0x2A79),
            Ok(DrawingId::new(0x2A79).unwrap())
        );
        assert!(DrawingId::try_from(0).is_err());
        assert!(DrawingId::try_from(u32::from(u16::MAX) + 1).is_err());
    }
}
