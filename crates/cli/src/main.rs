mod commands;
mod instances;

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
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Ls { long } => commands::ls::run(long).await,
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
}
