use std::fmt;

use crate::MAX_SOURCE_NAME_BYTES;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceName(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceNameError {
    Empty,
    TooLong,
}

impl SourceName {
    pub fn new(value: impl Into<String>) -> Result<Self, SourceNameError> {
        let value = value.into();

        if value.is_empty() {
            return Err(SourceNameError::Empty);
        }

        if value.len() > MAX_SOURCE_NAME_BYTES {
            return Err(SourceNameError::TooLong);
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for SourceName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_diagnostic_labels_and_paths() {
        for value in ["<stdin>", "script.lsp", "/tmp/script.lsp"] {
            let name = SourceName::new(value).unwrap();
            assert_eq!(name.as_str(), value);
        }
    }

    #[test]
    fn rejects_values_outside_the_wire_contract() {
        assert_eq!(SourceName::new(""), Err(SourceNameError::Empty));
        assert_eq!(
            SourceName::new("x".repeat(MAX_SOURCE_NAME_BYTES + 1)),
            Err(SourceNameError::TooLong)
        );
    }
}
