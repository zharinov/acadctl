use std::process::ExitCode;

use acadctl_rpc::{Doc, ProcessId};

use crate::instance::Instance;

use super::{fail, query_error_message};

pub async fn run(long: bool) -> ExitCode {
    let instances = match crate::instance::list().await {
        Ok(instances) => instances,
        Err(_) => return fail("Could not inspect registered AutoCAD endpoints.".into()),
    };

    match render(&instances, long) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }

            ExitCode::SUCCESS
        }
        Err(error) => fail(error),
    }
}

fn render(instances: &[Instance], long: bool) -> Result<Vec<String>, String> {
    let process_id_width = instances
        .iter()
        .map(|instance| instance.process_id.hex_width())
        .max()
        .unwrap_or(acadctl_rpc::ProcessId::MIN_HEX_WIDTH);
    let mut lines = Vec::new();

    for instance in instances {
        let documents = instance.documents.as_ref().map_err(query_error_message)?;

        for document in documents {
            lines.push(document_line(
                instance.process_id,
                process_id_width,
                document,
                long,
            ));
        }
    }

    Ok(lines)
}

fn document_line(
    process_id: ProcessId,
    process_id_width: usize,
    document: &Doc,
    long: bool,
) -> String {
    let modified = if document.modified { "*" } else { "." };
    let mode = if document.read_only { "ro" } else { "rw" };
    let name = if long {
        document
            .file_path
            .as_deref()
            .unwrap_or(&document.display_name)
    } else {
        &document.display_name
    };

    format!(
        "{process_id:0process_id_width$X}:{}  {modified}  {mode}  {name}",
        document.id
    )
}

#[cfg(test)]
mod tests {
    use acadctl_rpc::Doc;

    use super::*;
    use crate::instance::{Instance, QueryError};

    #[test]
    fn renders_only_actionable_document_state() {
        let instances = vec![available(
            0xFA5,
            vec![
                document("32F3", "/Users/me/Projects/House/house.dwg", true, false),
                document("91B2", "/Users/me/Projects/House/house.dwg", false, true),
                document("A04C", "Drawing1", false, false),
            ],
        )];

        assert_eq!(
            render(&instances, false).unwrap(),
            [
                "0FA5:32F3  *  rw  house.dwg",
                "0FA5:91B2  .  ro  house.dwg",
                "0FA5:A04C  .  rw  Drawing1",
            ]
        );
    }

    #[test]
    fn long_listing_uses_full_paths() {
        let instances = vec![available(
            0x1869F,
            vec![document(
                "32F3",
                "/Users/me/Projects/House/house.dwg",
                false,
                false,
            )],
        )];

        assert_eq!(
            render(&instances, true).unwrap(),
            ["1869F:32F3  .  rw  /Users/me/Projects/House/house.dwg"]
        );
    }

    #[test]
    fn aligns_process_ids_to_the_widest_live_value() {
        let instances = vec![
            available(0xFA5, vec![document("32F3", "/a.dwg", false, false)]),
            available(0x1869F, vec![document("91B2", "/b.dwg", false, false)]),
        ];

        assert_eq!(
            render(&instances, false).unwrap(),
            ["00FA5:32F3  .  rw  a.dwg", "1869F:91B2  .  rw  b.dwg"]
        );
    }

    #[test]
    fn an_empty_listing_is_successful() {
        assert!(render(&[], false).unwrap().is_empty());
        assert!(render(&[available(123, vec![])], false).unwrap().is_empty());
    }

    #[test]
    fn explains_each_query_failure() {
        assert_eq!(
            render(&[failed(123, QueryError::CannotConnect)], false).unwrap_err(),
            "Could not connect to the acadctl plugin. Install it and restart AutoCAD."
        );
        assert_eq!(
            render(&[failed(123, QueryError::TimedOut)], false).unwrap_err(),
            "AutoCAD did not respond within 5 seconds. Try again when it is idle."
        );
        assert_eq!(
            render(&[failed(123, QueryError::OutdatedPlugin)], false).unwrap_err(),
            "The acadctl plugin is outdated. Install the current version and restart AutoCAD."
        );
        assert_eq!(
            render(
                &[failed(123, QueryError::RequestFailed(String::new()))],
                false,
            )
            .unwrap_err(),
            "The acadctl plugin could not list documents."
        );
        assert_eq!(
            render(
                &[failed(
                    123,
                    QueryError::RequestFailed("document state is unavailable".into()),
                )],
                false,
            )
            .unwrap_err(),
            "Could not list AutoCAD documents: document state is unavailable"
        );
    }

    fn available(process_id: u32, documents: Vec<Doc>) -> Instance {
        Instance {
            process_id: acadctl_rpc::ProcessId::new(process_id).unwrap(),
            documents: Ok(documents),
        }
    }

    fn failed(process_id: u32, error: QueryError) -> Instance {
        Instance {
            process_id: acadctl_rpc::ProcessId::new(process_id).unwrap(),
            documents: Err(error),
        }
    }

    fn document(id: &str, path: &str, modified: bool, read_only: bool) -> Doc {
        let file_path = path.contains('/').then(|| path.to_owned());
        let display_name = file_path
            .as_deref()
            .and_then(|path| std::path::Path::new(path).file_name())
            .and_then(|name| name.to_str())
            .unwrap_or(path)
            .to_owned();
        Doc {
            id: id.into(),
            display_name,
            file_path,
            modified,
            read_only,
        }
    }
}
