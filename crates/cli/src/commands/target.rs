use std::fmt;
use std::str::FromStr;

use acadctl_rpc::{DocId, ProcessId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub process_id: ProcessId,
    pub document_id: DocId,
}

#[derive(Debug)]
pub struct ParseTargetError;

impl FromStr for Target {
    type Err = ParseTargetError;

    fn from_str(target: &str) -> Result<Self, Self::Err> {
        let (process_id, document_id) = target.split_once(':').ok_or(ParseTargetError)?;
        let process_id = process_id.parse().map_err(|_| ParseTargetError)?;

        let document_id = document_id.parse().map_err(|_| ParseTargetError)?;

        Ok(Self {
            process_id,
            document_id,
        })
    }
}

impl fmt::Display for ParseTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected a document target shown by `acadctl ps`")
    }
}

impl std::error::Error for ParseTargetError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_targets_without_both_ids() {
        assert!("32f3".parse::<Target>().is_err());
        assert!("0fa5:32f".parse::<Target>().is_err());
        assert!("000:32f3".parse::<Target>().is_err());
    }

    #[test]
    fn parses_and_normalizes_composite_targets() {
        let target = "00fa5:32f3".parse::<Target>().unwrap();
        assert_eq!(target.process_id, ProcessId::new(0xFA5).unwrap());
        assert_eq!(target.document_id, DocId::new(0x32F3).unwrap());
    }
}
