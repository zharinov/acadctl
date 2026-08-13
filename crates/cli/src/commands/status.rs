use std::process::ExitCode;

use crate::instances::StatusReport;

pub async fn run() -> ExitCode {
    let report = crate::instances::status().await;
    for line in render(&report) {
        println!("{line}");
    }

    if report.process_count != 0 && report.instances.len() == report.process_count {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn render(report: &StatusReport) -> Vec<String> {
    if report.process_count == 0 {
        return vec!["AutoCAD is not running.".into()];
    }
    if report.instances.is_empty() {
        return vec![
            "AutoCAD is running, but the acadctl plugin does not appear to be running.".into(),
        ];
    }

    let show_process_ids = report.process_count > 1;
    let documents = report
        .instances
        .iter()
        .flat_map(|instance| {
            instance.documents.iter().map(move |document| {
                (
                    document.path.as_str(),
                    document.modified,
                    instance.process_id,
                )
            })
        })
        .collect::<Vec<_>>();

    if !show_process_ids {
        return documents
            .into_iter()
            .map(|(path, modified, _)| format!("{path}{}", marker(modified)))
            .collect();
    }

    let width = documents
        .iter()
        .map(|(path, modified, _)| path.chars().count() + marker(*modified).len())
        .max()
        .unwrap_or(0);
    documents
        .into_iter()
        .map(|(path, modified, process_id)| {
            let marker = marker(modified);
            let padding = " ".repeat(width.saturating_sub(path.chars().count() + marker.len()));
            format!("{path}{marker}{padding} [{process_id}]")
        })
        .collect()
}

fn marker(modified: bool) -> &'static str {
    if modified { " *" } else { "" }
}

#[cfg(test)]
mod tests {
    use acadctl_rpc::Document;

    use super::*;
    use crate::instances::Instance;

    #[test]
    fn omits_the_process_id_for_one_instance() {
        let report = StatusReport {
            process_count: 1,
            instances: vec![Instance {
                process_id: 123,
                documents: vec![
                    document("/Users/me/Projects/House/house.dwg", false),
                    document("/Users/me/Projects/Site/site.dwg", true),
                    document("/Users/me/Projects/Site/temp.dwg", false),
                ],
            }],
        };

        assert_eq!(
            render(&report),
            [
                "/Users/me/Projects/House/house.dwg",
                "/Users/me/Projects/Site/site.dwg *",
                "/Users/me/Projects/Site/temp.dwg",
            ]
        );
    }

    #[test]
    fn adds_aligned_process_ids_for_multiple_instances() {
        let report = StatusReport {
            process_count: 2,
            instances: vec![
                Instance {
                    process_id: 123,
                    documents: vec![
                        document("/Users/me/Projects/House/house.dwg", true),
                        document("/Users/me/Projects/Site/site.dwg", false),
                    ],
                },
                Instance {
                    process_id: 321,
                    documents: vec![document("/Users/me/Projects/Site/temp.dwg", false)],
                },
            ],
        };

        assert_eq!(
            render(&report),
            [
                "/Users/me/Projects/House/house.dwg * [123]",
                "/Users/me/Projects/Site/site.dwg     [123]",
                "/Users/me/Projects/Site/temp.dwg     [321]",
            ]
        );
    }

    #[test]
    fn explains_when_autocad_or_the_plugin_is_absent() {
        assert_eq!(
            render(&StatusReport {
                process_count: 1,
                instances: vec![],
            }),
            ["AutoCAD is running, but the acadctl plugin does not appear to be running."]
        );
        assert_eq!(
            render(&StatusReport {
                process_count: 0,
                instances: vec![],
            }),
            ["AutoCAD is not running."]
        );
    }

    fn document(path: &str, modified: bool) -> Document {
        Document {
            path: path.into(),
            modified,
        }
    }
}
