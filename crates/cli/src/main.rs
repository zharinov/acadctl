mod commands;
mod instances;
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
    /// List open AutoCAD documents.
    Ls {
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
        pid: Option<u32>,
    },
    /// Save an open AutoCAD document in place.
    Save {
        /// Document ID shown by `acadctl ls`.
        id: String,
    },
    /// Undo the drawing's last AutoCAD history step.
    Undo {
        /// Document ID shown by `acadctl ls`.
        id: String,
    },
    /// Redo the drawing's next AutoCAD history step.
    Redo {
        /// Document ID shown by `acadctl ls`.
        id: String,
    },
    /// Close an open AutoCAD document.
    Close {
        /// Document ID shown by `acadctl ls`.
        id: String,

        /// Discard unsaved changes.
        #[arg(long)]
        discard: bool,
    },
    /// Evaluate one AutoLISP form and print its value.
    Eval(ExecutionArgs),
    /// Execute an AutoLISP batch without implicit value output.
    Exec(ExecutionArgs),
    /// Terminate an AutoCAD instance.
    Kill {
        /// Target AutoCAD process when more than one instance is running.
        pid: Option<u32>,

        /// Terminate immediately without waiting for AutoCAD to close normally.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args)]
struct ExecutionArgs {
    /// Document ID shown by `acadctl ls`.
    id: String,

    /// AutoLISP file, or - for stdin. Reads stdin when omitted.
    file: Option<PathBuf>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Ls { long } => commands::ls::run(long).await,
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
            commands::execute::run(
                arguments.id,
                arguments.file,
                acadctl_rpc::ExecutionMode::Eval,
            )
            .await
        }
        Command::Exec(arguments) => {
            commands::execute::run(
                arguments.id,
                arguments.file,
                acadctl_rpc::ExecutionMode::Exec,
            )
            .await
        }
        Command::Kill { pid, force } => commands::kill::run(pid, force).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ls_command() {
        let cli = Cli::try_parse_from(["acadctl", "ls"]).unwrap();

        assert!(matches!(cli.command, Command::Ls { long: false }));

        let cli = Cli::try_parse_from(["acadctl", "ls", "--long"]).unwrap();

        assert!(matches!(cli.command, Command::Ls { long: true }));
    }

    #[test]
    fn does_not_keep_superseded_commands() {
        assert!(Cli::try_parse_from(["acadctl", "status"]).is_err());
    }

    #[test]
    fn parses_document_lifecycle_commands() {
        let cli =
            Cli::try_parse_from(["acadctl", "open", "/tmp/house.dwg", "--pid", "123"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Open {
                path,
                pid: Some(123)
            } if path == std::path::Path::new("/tmp/house.dwg")
        ));

        let cli = Cli::try_parse_from(["acadctl", "save", "k7m2qx"]).unwrap();
        assert!(matches!(cli.command, Command::Save { id } if id == "k7m2qx"));

        let cli = Cli::try_parse_from(["acadctl", "undo", "k7m2qx"]).unwrap();
        assert!(matches!(cli.command, Command::Undo { id } if id == "k7m2qx"));

        let cli = Cli::try_parse_from(["acadctl", "redo", "k7m2qx"]).unwrap();
        assert!(matches!(cli.command, Command::Redo { id } if id == "k7m2qx"));

        assert!(Cli::try_parse_from(["acadctl", "undo", "k7m2qx", "--force"]).is_err());
        assert!(Cli::try_parse_from(["acadctl", "redo", "k7m2qx", "2"]).is_err());

        let cli = Cli::try_parse_from(["acadctl", "close", "k7m2qx", "--discard"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Close { id, discard: true } if id == "k7m2qx"
        ));
    }

    #[test]
    fn parses_eval_and_exec_source_selection() {
        let cli = Cli::try_parse_from(["acadctl", "eval", "k7m2qx"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Eval(ExecutionArgs { id, file: None }) if id == "k7m2qx"
        ));

        let cli = Cli::try_parse_from(["acadctl", "exec", "k7m2qx", "script.lsp"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Exec(ExecutionArgs { id, file: Some(file) })
                if id == "k7m2qx" && file == std::path::Path::new("script.lsp")
        ));

        assert!(Cli::try_parse_from(["acadctl", "exec", "k7m2qx", "a.lsp", "b.lsp"]).is_err());
    }

    #[test]
    fn parses_process_termination() {
        let cli = Cli::try_parse_from(["acadctl", "kill"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Kill {
                pid: None,
                force: false
            }
        ));

        let cli = Cli::try_parse_from(["acadctl", "kill", "123", "--force"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Kill {
                pid: Some(123),
                force: true
            }
        ));

        assert!(Cli::try_parse_from(["acadctl", "kill", "123", "456"]).is_err());
    }
}
