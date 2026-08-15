use acadctl_rpc::{DocumentId, ProcessId};

pub struct Target {
    pub process_id: ProcessId,
    pub document_id: DocumentId,
}

pub fn resolve(target: &str) -> Result<Target, String> {
    let (process_id, document_id) = target
        .split_once(':')
        .ok_or_else(|| invalid_target(target))?;
    let process_id = process_id.parse().map_err(|_| invalid_target(target))?;

    let document_id = document_id.parse().map_err(|_| invalid_target(target))?;

    Ok(Target {
        process_id,
        document_id,
    })
}

fn invalid_target(target: &str) -> String {
    format!("Document target '{target}' is invalid. Use an ID shown by `acadctl ps`.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_targets_without_both_ids() {
        assert!(resolve("32f3").is_err());
        assert!(resolve("0fa5:32f").is_err());
        assert!(resolve("000:32f3").is_err());
    }

    #[test]
    fn parses_and_normalizes_composite_targets() {
        let target = resolve("00fa5:32f3").unwrap();
        assert_eq!(target.process_id, ProcessId::new(0xFA5).unwrap());
        assert_eq!(target.document_id, DocumentId::new(0x32F3));
    }
}
