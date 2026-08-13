mod commands;
mod instances;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

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
    /// Close an open AutoCAD document.
    Close {
        /// Document ID shown by `acadctl ls`.
        id: String,

        /// Discard unsaved changes.
        #[arg(long)]
        discard: bool,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Ls { long } => commands::ls::run(long).await,
        Command::Open { path, pid } => commands::open::run(path, pid).await,
        Command::Save { id } => commands::save::run(id).await,
        Command::Close { id, discard } => commands::close::run(id, discard).await,
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
    fn does_not_keep_superseded_or_unimplemented_commands() {
        assert!(Cli::try_parse_from(["acadctl", "status"]).is_err());
        assert!(Cli::try_parse_from(["acadctl", "exec"]).is_err());
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

        let cli = Cli::try_parse_from(["acadctl", "close", "k7m2qx", "--discard"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Close { id, discard: true } if id == "k7m2qx"
        ));
    }
}
