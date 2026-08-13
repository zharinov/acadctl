use crate::instances::ListReport;

use super::query_error_message;

pub async fn connect(id: &str) -> Result<super::Client, String> {
    let report = crate::instances::list()
        .await
        .map_err(|_| "Could not inspect running AutoCAD instances.".to_owned())?;
    let process_id = resolve_from_report(&report, id)?;
    super::connect(process_id).await
}

fn resolve_from_report(report: &ListReport, id: &str) -> Result<u32, String> {
    let matches = report
        .instances
        .iter()
        .filter_map(|instance| {
            instance.documents.as_ref().ok().and_then(|documents| {
                documents
                    .iter()
                    .any(|document| document.id == id)
                    .then_some(instance.process_id)
            })
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [process_id] => Ok(*process_id),
        [] if report.instances.is_empty() => Err("AutoCAD is not running.".into()),
        [] => {
            if let Some(error) = report
                .instances
                .iter()
                .find_map(|instance| instance.documents.as_ref().err())
            {
                Err(query_error_message(error))
            } else {
                Err(format!("Document '{id}' is not open."))
            }
        }
        _ => Err(format!(
            "Document ID '{id}' identifies more than one open document. Restart AutoCAD to regenerate document IDs."
        )),
    }
}

#[cfg(test)]
mod tests {
    use acadctl_rpc::Document;

    use super::*;
    use crate::instances::Instance;

    #[test]
    fn resolves_a_document_to_its_instance() {
        let report = ListReport {
            instances: vec![Instance {
                process_id: 123,
                documents: Ok(vec![document("k7m2qx")]),
            }],
        };

        assert_eq!(resolve_from_report(&report, "k7m2qx").unwrap(), 123);
        assert_eq!(
            resolve_from_report(&report, "missing").unwrap_err(),
            "Document 'missing' is not open."
        );
    }

    #[test]
    fn rejects_duplicate_document_ids() {
        let report = ListReport {
            instances: vec![
                Instance {
                    process_id: 123,
                    documents: Ok(vec![document("k7m2qx")]),
                },
                Instance {
                    process_id: 456,
                    documents: Ok(vec![document("k7m2qx")]),
                },
            ],
        };

        assert_eq!(
            resolve_from_report(&report, "k7m2qx").unwrap_err(),
            "Document ID 'k7m2qx' identifies more than one open document. Restart AutoCAD to regenerate document IDs."
        );
    }

    fn document(id: &str) -> Document {
        Document {
            id: id.into(),
            path: "/tmp/house.dwg".into(),
            modified: false,
            read_only: false,
        }
    }
}
