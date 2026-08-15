mod commands;
mod instance;
mod source;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "Control AutoCAD from the command line")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List acadctl-enabled AutoCAD instances and their documents.
    Ps {
        /// Show the full path of named drawings.
        #[arg(long)]
        long: bool,
    },
    /// Open a DWG in AutoCAD.
    Open {
        /// Drawing to open.
        path: PathBuf,

        /// Target AutoCAD process when more than one instance is running.
        #[arg(long)]
        pid: Option<acadctl_rpc::ProcessId>,
    },
    /// Save an open AutoCAD document in place.
    Save {
        /// Document target shown by `acadctl ps`.
        id: String,
    },
    /// Undo the drawing's last AutoCAD history step.
    Undo {
        /// Document target shown by `acadctl ps`.
        id: String,
    },
    /// Redo the drawing's next AutoCAD history step.
    Redo {
        /// Document target shown by `acadctl ps`.
        id: String,
    },
    /// Close an open AutoCAD document.
    Close {
        /// Document target shown by `acadctl ps`.
        id: String,

        /// Discard unsaved changes.
        #[arg(long)]
        discard: bool,
    },
    /// Evaluate one AutoLISP form and print its value.
    Eval(EvalArgs),
    /// Execute an AutoLISP batch without implicit value output.
    Exec(ExecArgs),
    /// Terminate an AutoCAD instance.
    Kill {
        /// Target AutoCAD process when more than one instance is running.
        pid: Option<acadctl_rpc::ProcessId>,

        /// Terminate immediately without waiting for AutoCAD to close normally.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args)]
struct EvalArgs {
    /// Document target shown by `acadctl ps`.
    id: String,

    /// AutoLISP form. Reads stdin when omitted.
    #[arg(
        value_name = "FORM",
        allow_hyphen_values = true,
        conflicts_with = "file"
    )]
    inline: Option<String>,

    /// Read AutoLISP from a file.
    #[arg(
        short = 'f',
        long = "file",
        value_name = "FILE",
        conflicts_with = "inline"
    )]
    file: Option<PathBuf>,
}

#[derive(Args)]
struct ExecArgs {
    /// Document target shown by `acadctl ps`.
    id: String,

    /// AutoLISP forms. Reads stdin when omitted.
    #[arg(
        value_name = "FORMS",
        allow_hyphen_values = true,
        conflicts_with = "file"
    )]
    inline: Option<String>,

    /// Read AutoLISP from a file.
    #[arg(
        short = 'f',
        long = "file",
        value_name = "FILE",
        conflicts_with = "inline"
    )]
    file: Option<PathBuf>,
}

fn source_spec(inline: Option<String>, file: Option<PathBuf>) -> source::SourceSpec {
    match (inline, file) {
        (Some(source), None) => source::SourceSpec::CommandLine(source),
        (None, Some(path)) => source::SourceSpec::File(path),
        (None, None) => source::SourceSpec::Stdin,
        (Some(_), Some(_)) => unreachable!("clap rejects inline source combined with a file"),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Ps { long } => commands::ps::run(long).await,
        Command::Open { path, pid } => commands::open::run(path, pid).await,
        Command::Save { id } => commands::save::run(id).await,
        Command::Undo { id } => {
            commands::history::run(id, commands::history::Direction::Undo).await
        }
        Command::Redo { id } => {
            commands::history::run(id, commands::history::Direction::Redo).await
        }
        Command::Close { id, discard } => commands::close::run(id, discard).await,
        Command::Eval(arguments) => {
            commands::exec::run(
                arguments.id,
                source_spec(arguments.inline, arguments.file),
                acadctl_rpc::ExecMode::Eval,
            )
            .await
        }
        Command::Exec(arguments) => {
            commands::exec::run(
                arguments.id,
                source_spec(arguments.inline, arguments.file),
                acadctl_rpc::ExecMode::Exec,
            )
            .await
        }
        Command::Kill { pid, force } => commands::kill::run(pid, force).await,
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, error::ErrorKind};

    use super::*;

    #[test]
    fn eval_and_exec_accept_positional_source() {
        for command_name in ["eval", "exec"] {
            for source in ["-", "-1", "(+ 20 22)"] {
                let cli =
                    Cli::try_parse_from(["acadctl", command_name, "1234:5678", source]).unwrap();

                let inline = match cli.command {
                    Command::Eval(arguments) => arguments.inline,
                    Command::Exec(arguments) => arguments.inline,
                    _ => unreachable!(),
                };
                assert_eq!(inline.as_deref(), Some(source));
            }
        }
    }

    #[test]
    fn eval_and_exec_accept_file_options() {
        for command_name in ["eval", "exec"] {
            for flag in ["-f", "--file"] {
                let cli =
                    Cli::try_parse_from(["acadctl", command_name, "1234:5678", flag, "script.lsp"])
                        .unwrap();

                let file = match cli.command {
                    Command::Eval(arguments) => arguments.file,
                    Command::Exec(arguments) => arguments.file,
                    _ => unreachable!(),
                };
                assert_eq!(file.as_deref(), Some(std::path::Path::new("script.lsp")));
            }
        }
    }

    #[test]
    fn help_uses_mode_specific_source_value_names() {
        let mut command = Cli::command();
        let eval_help = command
            .find_subcommand_mut("eval")
            .unwrap()
            .render_help()
            .to_string();
        let exec_help = command
            .find_subcommand_mut("exec")
            .unwrap()
            .render_help()
            .to_string();

        assert!(eval_help.contains("[FORM]"));
        assert!(exec_help.contains("[FORMS]"));
        assert!(eval_help.contains("-f, --file <FILE>"));
        assert!(exec_help.contains("-f, --file <FILE>"));
    }

    #[test]
    fn inline_source_conflicts_with_a_file() {
        let result = Cli::try_parse_from([
            "acadctl",
            "exec",
            "1234:5678",
            "(+ 20 22)",
            "-f",
            "script.lsp",
        ]);
        let error = match result {
            Ok(_) => panic!("file and inline source were accepted together"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }
}
