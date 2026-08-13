use std::collections::HashSet;
use std::process::ExitCode;

use crate::instances::{ListError, ListReport};

use super::{document_line, fail, query_error_message};

pub async fn run(long: bool) -> ExitCode {
    let report = match crate::instances::list().await {
        Ok(report) => report,
        Err(error) => return fail(list_error_message(error)),
    };
    match render(&report, long) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => fail(error),
    }
}

fn render(report: &ListReport, long: bool) -> Result<Vec<String>, String> {
    let mut ids = HashSet::new();
    let mut lines = Vec::new();
    for instance in &report.instances {
        let documents = instance.documents.as_ref().map_err(query_error_message)?;
        for document in documents {
            if !ids.insert(document.id.as_str()) {
                return Err(format!(
                    "Document ID '{}' identifies more than one open document. Restart AutoCAD to regenerate document IDs.",
                    document.id
                ));
            }
            lines.push(document_line(document, long));
        }
    }
    Ok(lines)
}

fn list_error_message(error: ListError) -> String {
    match error {
        ListError::QueryTaskFailed => {
            "An internal task failed while querying AutoCAD. Run `acadctl ls` again.".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use acadctl_rpc::Document;

    use super::*;
    use crate::instances::{Instance, QueryError};

    #[test]
    fn renders_only_actionable_document_state() {
        let report = report(vec![available(
            123,
            vec![
                document("k7m2qx", "/Users/me/Projects/House/house.dwg", true, false),
                document("p8z4cw", "/Users/me/Projects/House/house.dwg", false, true),
                document("m4tc8r", "Drawing1", false, false),
            ],
        )]);

        assert_eq!(
            render(&report, false).unwrap(),
            [
                "k7m2qx  *  w  house.dwg",
                "p8z4cw  -  r  house.dwg",
                "m4tc8r  -  w  Drawing1",
            ]
        );
    }

    #[test]
    fn long_listing_uses_full_paths() {
        let report = report(vec![available(
            123,
            vec![document(
                "k7m2qx",
                "/Users/me/Projects/House/house.dwg",
                false,
                false,
            )],
        )]);

        assert_eq!(
            render(&report, true).unwrap(),
            ["k7m2qx  -  w  /Users/me/Projects/House/house.dwg"]
        );
    }

    #[test]
    fn an_empty_listing_is_successful() {
        assert!(render(&report(vec![]), false).unwrap().is_empty());
        assert!(
            render(&report(vec![available(123, vec![])]), false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn explains_each_query_failure() {
        assert_eq!(
            render(&report(vec![failed(123, QueryError::CannotConnect)]), false).unwrap_err(),
            "Could not connect to the acadctl plugin. Install it and restart AutoCAD."
        );
        assert_eq!(
            render(&report(vec![failed(123, QueryError::TimedOut)]), false).unwrap_err(),
            "AutoCAD did not respond within 5 seconds. Try again when it is idle."
        );
        assert_eq!(
            render(
                &report(vec![failed(123, QueryError::OutdatedPlugin)]),
                false
            )
            .unwrap_err(),
            "The acadctl plugin is outdated. Install the current version and restart AutoCAD."
        );
        assert_eq!(
            render(
                &report(vec![failed(123, QueryError::RequestFailed(String::new()))]),
                false,
            )
            .unwrap_err(),
            "The acadctl plugin could not list documents."
        );
        assert_eq!(
            render(
                &report(vec![failed(
                    123,
                    QueryError::RequestFailed("document state is unavailable".into()),
                )]),
                false,
            )
            .unwrap_err(),
            "Could not list AutoCAD documents: document state is unavailable"
        );
    }

    #[test]
    fn explains_ambiguous_document_ids() {
        let report = report(vec![
            available(123, vec![document("k7m2qx", "/a.dwg", false, false)]),
            available(321, vec![document("k7m2qx", "/b.dwg", false, false)]),
        ]);
        assert_eq!(
            render(&report, false).unwrap_err(),
            "Document ID 'k7m2qx' identifies more than one open document. Restart AutoCAD to regenerate document IDs."
        );
    }

    fn report(instances: Vec<Instance>) -> ListReport {
        ListReport { instances }
    }

    fn available(process_id: u32, documents: Vec<Document>) -> Instance {
        Instance {
            process_id,
            documents: Ok(documents),
        }
    }

    fn failed(process_id: u32, error: QueryError) -> Instance {
        Instance {
            process_id,
            documents: Err(error),
        }
    }

    fn document(id: &str, path: &str, modified: bool, read_only: bool) -> Document {
        Document {
            id: id.into(),
            path: path.into(),
            modified,
            read_only,
        }
    }
}
