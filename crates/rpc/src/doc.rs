use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocId(u16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseDocIdError;

impl DocId {
    pub const HEX_WIDTH: usize = 4;

    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u16 {
        self.0
    }
}

impl fmt::Display for DocId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:04X}", self.0)
    }
}

impl FromStr for DocId {
    type Err = ParseDocIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != Self::HEX_WIDTH || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ParseDocIdError);
        }

        u16::from_str_radix(value, 16)
            .map(Self)
            .map_err(|_| ParseDocIdError)
    }
}

impl fmt::Display for ParseDocIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected a document ID of exactly 4 hexadecimal digits")
    }
}

impl std::error::Error for ParseDocIdError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_as_four_uppercase_hexadecimal_digits() {
        assert_eq!(DocId::new(0).to_string(), "0000");
        assert_eq!(DocId::new(0x2A79).to_string(), "2A79");
        assert_eq!(DocId::new(u16::MAX).to_string(), "FFFF");
    }

    #[test]
    fn parses_case_insensitively_and_normalizes_on_display() {
        assert_eq!("2a79".parse(), Ok(DocId::new(0x2A79)));
        assert_eq!("0000".parse(), Ok(DocId::new(0)));
    }

    #[test]
    fn rejects_values_outside_the_public_shape() {
        for value in ["", "2A7", "02A79", "2A7G", "+A79", " A79"] {
            assert!(value.parse::<DocId>().is_err(), "accepted {value:?}");
        }
    }
}
