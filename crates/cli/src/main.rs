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
    /// Show the AutoCAD plugin connection status.
    Status,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Status => commands::status::run().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_command() {
        let cli = Cli::try_parse_from(["acadctl", "status"]).unwrap();

        assert!(matches!(cli.command, Command::Status));
    }

    #[test]
    fn does_not_advertise_unimplemented_commands() {
        assert!(Cli::try_parse_from(["acadctl", "exec"]).is_err());
    }
}
