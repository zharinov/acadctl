use std::process::ExitCode;

use acadctl_rpc::{Drawing, InstanceId};

use crate::instance::{Instance, InstanceSnapshot};

use super::{parse_drawing_id, query_error_message};

pub async fn run(long: bool) -> ExitCode {
    let snapshot = InstanceSnapshot::discover();
    let instances = snapshot.query_instances().await;

    let rendered = render(&instances, long);

    for line in rendered.lines {
        println!("{line}");
    }

    for diagnostic in &rendered.diagnostics {
        eprintln!("{diagnostic}");
    }

    if rendered.diagnostics.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Rendered {
    lines: Vec<String>,
    diagnostics: Vec<String>,
}

fn render(instances: &[Instance], long: bool) -> Rendered {
    let instance_id_width = instances
        .iter()
        .map(|instance| instance.instance_id.hex_width())
        .max()
        .unwrap_or(acadctl_rpc::InstanceId::MIN_HEX_WIDTH);
    let mut lines = Vec::new();
    let mut diagnostics = Vec::new();

    for instance in instances {
        let drawings = match &instance.drawings {
            Ok(drawings) => drawings,
            Err(error) => {
                diagnostics.push(query_error_message(error));
                continue;
            }
        };

        for drawing in drawings {
            match drawing_line(instance.instance_id, instance_id_width, drawing, long) {
                Ok(line) => lines.push(line),
                Err(error) => diagnostics.push(format!(
                    "Could not inspect a drawing in AutoCAD instance {} ({error})",
                    instance.instance_id
                )),
            }
        }
    }

    Rendered { lines, diagnostics }
}

fn drawing_line(
    instance_id: InstanceId,
    instance_id_width: usize,
    drawing: &Drawing,
    long: bool,
) -> Result<String, String> {
    let drawing_id = parse_drawing_id(drawing.id)?;

    let modified = if drawing.modified { "*" } else { "." };
    let mode = if drawing.read_only { "ro" } else { "rw" };
    let name = if long {
        drawing
            .file_path
            .as_deref()
            .unwrap_or(&drawing.display_name)
    } else {
        &drawing.display_name
    };

    Ok(format!(
        "{instance_id:0instance_id_width$X}:{drawing_id}  {modified}  {mode}  {name}"
    ))
}

#[cfg(test)]
mod tests {
    use acadctl_rpc::Drawing;

    use super::*;
    use crate::instance::{Instance, QueryError};

    #[test]
    fn renders_only_actionable_drawing_state() {
        let instances = vec![available(
            0xFA5,
            vec![
                drawing("32F3", "/Users/me/Projects/House/house.dwg", true, false),
                drawing("91B2", "/Users/me/Projects/House/house.dwg", false, true),
                drawing("A04C", "Drawing1", false, false),
            ],
        )];

        assert_eq!(
            render(&instances, false),
            Rendered {
                lines: vec![
                    "0FA5:32F3  *  rw  house.dwg",
                    "0FA5:91B2  .  ro  house.dwg",
                    "0FA5:A04C  .  rw  Drawing1",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
                diagnostics: vec![],
            }
        );
    }

    #[test]
    fn long_listing_uses_full_paths() {
        let instances = vec![available(
            0x1869F,
            vec![drawing(
                "32F3",
                "/Users/me/Projects/House/house.dwg",
                false,
                false,
            )],
        )];

        assert_eq!(
            render(&instances, true).lines,
            ["1869F:32F3  .  rw  /Users/me/Projects/House/house.dwg"]
        );
    }

    #[test]
    fn aligns_instance_ids_to_the_widest_live_value() {
        let instances = vec![
            available(0xFA5, vec![drawing("32F3", "/a.dwg", false, false)]),
            available(0x1869F, vec![drawing("91B2", "/b.dwg", false, false)]),
        ];

        assert_eq!(
            render(&instances, false).lines,
            ["00FA5:32F3  .  rw  a.dwg", "1869F:91B2  .  rw  b.dwg"]
        );
    }

    #[test]
    fn an_empty_listing_is_successful() {
        assert!(render(&[], false).lines.is_empty());
        assert!(render(&[available(123, vec![])], false).lines.is_empty());
    }

    #[test]
    fn preserves_healthy_drawings_and_explains_each_failed_instance() {
        assert_eq!(
            render(&[failed(123, QueryError::CannotConnect)], false).diagnostics,
            ["Plugin unavailable. Install it and restart AutoCAD."]
        );
        assert_eq!(
            render(&[failed(123, QueryError::TimedOut)], false).diagnostics,
            ["Plugin does not respond within 5 seconds."]
        );
        assert_eq!(
            render(&[failed(123, QueryError::OutdatedPlugin)], false).diagnostics,
            ["Plugin incompatible. Update it and restart AutoCAD."]
        );
        assert_eq!(
            render(
                &[failed(123, QueryError::RequestFailed(String::new()))],
                false,
            )
            .diagnostics,
            ["Unknown error."]
        );
        assert_eq!(
            render(
                &[failed(
                    123,
                    QueryError::RequestFailed(
                        "failed to decode Protobuf message: Drawing.id: invalid wire type".into(),
                    ),
                )],
                false,
            )
            .diagnostics,
            ["Plugin incompatible. Update it and restart AutoCAD."]
        );
        assert_eq!(
            render(
                &[failed(
                    123,
                    QueryError::RequestFailed("drawing state is unavailable".into()),
                )],
                false,
            )
            .diagnostics,
            ["Unknown error."]
        );

        let rendered = render(
            &[
                available(123, vec![drawing("32F3", "/a.dwg", false, false)]),
                failed(456, QueryError::TimedOut),
            ],
            false,
        );
        assert_eq!(rendered.lines, ["007B:32F3  .  rw  a.dwg"]);
        assert_eq!(
            rendered.diagnostics,
            ["Plugin does not respond within 5 seconds."]
        );
    }

    fn available(instance_id: u32, drawings: Vec<Drawing>) -> Instance {
        Instance {
            instance_id: acadctl_rpc::InstanceId::new(instance_id).unwrap(),
            drawings: Ok(drawings),
        }
    }

    fn failed(instance_id: u32, error: QueryError) -> Instance {
        Instance {
            instance_id: acadctl_rpc::InstanceId::new(instance_id).unwrap(),
            drawings: Err(error),
        }
    }

    fn drawing(id: &str, path: &str, modified: bool, read_only: bool) -> Drawing {
        let file_path = path.contains('/').then(|| path.to_owned());
        let display_name = file_path
            .as_deref()
            .and_then(|path| std::path::Path::new(path).file_name())
            .and_then(|name| name.to_str())
            .unwrap_or(path)
            .to_owned();
        Drawing {
            id: u32::from(id.parse::<acadctl_rpc::DrawingId>().unwrap()),
            display_name,
            file_path,
            modified,
            read_only,
        }
    }
}
