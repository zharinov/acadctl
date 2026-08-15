use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(NonZeroU32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseProcessIdError;

impl ProcessId {
    pub const MIN_HEX_WIDTH: usize = 4;
    pub const MAX_HEX_WIDTH: usize = 8;

    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }

    pub const fn hex_width(self) -> usize {
        let digits = ((u32::BITS - self.get().leading_zeros()) as usize).div_ceil(4);

        if digits < Self::MIN_HEX_WIDTH {
            Self::MIN_HEX_WIDTH
        } else {
            digits
        }
    }
}

impl fmt::Display for ProcessId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:04X}", self.get())
    }
}

impl fmt::UpperHex for ProcessId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.get(), formatter)
    }
}

impl FromStr for ProcessId {
    type Err = ParseProcessIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !(Self::MIN_HEX_WIDTH..=Self::MAX_HEX_WIDTH).contains(&value.len())
            || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ParseProcessIdError);
        }

        let value = u32::from_str_radix(value, 16).map_err(|_| ParseProcessIdError)?;
        Self::new(value).ok_or(ParseProcessIdError)
    }
}

impl fmt::Display for ParseProcessIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected a nonzero process ID of 4 to 8 hexadecimal digits")
    }
}

impl std::error::Error for ParseProcessIdError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_as_uppercase_hex_with_at_least_four_digits() {
        assert_eq!(ProcessId::new(1).unwrap().to_string(), "0001");
        assert_eq!(ProcessId::new(0xFA5).unwrap().to_string(), "0FA5");
        assert_eq!(ProcessId::new(0x1869F).unwrap().to_string(), "1869F");
        assert_eq!(ProcessId::new(u32::MAX).unwrap().to_string(), "FFFFFFFF");
    }

    #[test]
    fn parses_case_insensitively_and_ignores_leading_zeroes() {
        assert_eq!("0fa5".parse(), Ok(ProcessId::new(0xFA5).unwrap()));
        assert_eq!("00FA5".parse(), Ok(ProcessId::new(0xFA5).unwrap()));
        assert_eq!("FFFFFFFF".parse(), Ok(ProcessId::new(u32::MAX).unwrap()));
    }

    #[test]
    fn rejects_values_outside_the_public_shape() {
        assert_eq!(ProcessId::new(0), None);

        for value in ["", "123", "0000", "000000000", "12G4", "+1234", " 1234"] {
            assert!(value.parse::<ProcessId>().is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn reports_the_width_needed_for_aligned_output() {
        assert_eq!(ProcessId::new(1).unwrap().hex_width(), 4);
        assert_eq!(ProcessId::new(0xFFFF).unwrap().hex_width(), 4);
        assert_eq!(ProcessId::new(0x10000).unwrap().hex_width(), 5);
        assert_eq!(ProcessId::new(u32::MAX).unwrap().hex_width(), 8);
    }
}
