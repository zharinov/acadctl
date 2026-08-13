use std::process::ExitCode;

use acadctl_rpc::Document;

use crate::instances::{Instance, PluginState, StatusReport};

pub async fn run() -> ExitCode {
    let report = crate::instances::status().await;
    for line in render(&report) {
        println!("{line}");
    }

    if successful(&report) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn successful(report: &StatusReport) -> bool {
    !report.instances.is_empty()
        && report
            .instances
            .iter()
            .all(|instance| matches!(instance.plugin, PluginState::Available(_)))
}

fn render(report: &StatusReport) -> Vec<String> {
    match report.instances.as_slice() {
        [] => vec!["AutoCAD is not running.".into()],
        [instance] => render_single(instance),
        instances => render_multiple(instances),
    }
}

fn render_single(instance: &Instance) -> Vec<String> {
    match &instance.plugin {
        PluginState::Unavailable => {
            vec!["AutoCAD is running, but the acadctl plugin does not appear to be running.".into()]
        }
        PluginState::Available(documents) if documents.is_empty() => {
            vec!["AutoCAD is running, but no drawings are open.".into()]
        }
        PluginState::Available(documents) => documents.iter().map(document_line).collect(),
    }
}

fn render_multiple(instances: &[Instance]) -> Vec<String> {
    let lines = instances
        .iter()
        .flat_map(instance_lines)
        .collect::<Vec<_>>();
    let width = lines
        .iter()
        .map(|(text, _)| text.chars().count())
        .max()
        .unwrap_or(0);

    lines
        .into_iter()
        .map(|(text, process_id)| {
            let padding = " ".repeat(width.saturating_sub(text.chars().count()));
            format!("{text}{padding} [{process_id}]")
        })
        .collect()
}

fn instance_lines(instance: &Instance) -> Vec<(String, u32)> {
    let process_id = instance.process_id;
    match &instance.plugin {
        PluginState::Unavailable => {
            vec![("acadctl plugin unavailable".to_owned(), process_id)]
        }
        PluginState::Available(documents) if documents.is_empty() => {
            vec![("No drawings are open.".to_owned(), process_id)]
        }
        PluginState::Available(documents) => documents
            .iter()
            .map(|document| (document_line(document), process_id))
            .collect(),
    }
}

fn document_line(document: &Document) -> String {
    let marker = if document.modified { " *" } else { "" };
    format!("{}{marker}", document.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_single_instance_states_without_a_process_id() {
        assert_eq!(render(&report(vec![])), ["AutoCAD is not running."]);
        assert_eq!(
            render(&report(vec![unavailable(123)])),
            ["AutoCAD is running, but the acadctl plugin does not appear to be running."]
        );
        assert_eq!(
            render(&report(vec![available(123, vec![])])),
            ["AutoCAD is running, but no drawings are open."]
        );
        assert_eq!(
            render(&report(vec![available(
                123,
                vec![
                    document("/Users/me/Projects/House/house.dwg", false),
                    document("/Users/me/Projects/Site/site.dwg", true),
                ],
            )])),
            [
                "/Users/me/Projects/House/house.dwg",
                "/Users/me/Projects/Site/site.dwg *",
            ]
        );
    }

    #[test]
    fn renders_every_state_for_multiple_instances() {
        let report = report(vec![
            available(123, vec![document("/a.dwg", true)]),
            available(321, vec![]),
            unavailable(654),
        ]);

        assert_eq!(
            render(&report),
            [
                "/a.dwg *                   [123]",
                "No drawings are open.      [321]",
                "acadctl plugin unavailable [654]",
            ]
        );
    }

    #[test]
    fn succeeds_only_when_every_running_instance_has_the_plugin() {
        assert!(!successful(&report(vec![])));
        assert!(successful(&report(vec![available(123, vec![])])));
        assert!(successful(&report(vec![available(
            123,
            vec![document("/a.dwg", false)],
        )])));
        assert!(!successful(&report(vec![unavailable(123)])));
        assert!(!successful(&report(vec![
            available(123, vec![]),
            unavailable(321),
        ])));
    }

    fn report(instances: Vec<Instance>) -> StatusReport {
        StatusReport { instances }
    }

    fn available(process_id: u32, documents: Vec<Document>) -> Instance {
        Instance {
            process_id,
            plugin: PluginState::Available(documents),
        }
    }

    fn unavailable(process_id: u32) -> Instance {
        Instance {
            process_id,
            plugin: PluginState::Unavailable,
        }
    }

    fn document(path: &str, modified: bool) -> Document {
        Document {
            path: path.into(),
            modified,
        }
    }
}
