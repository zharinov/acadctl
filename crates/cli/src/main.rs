mod commands;
mod instance;
mod source;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::{ContextKind, ErrorKind};
use clap::{Args, CommandFactory, Parser, Subcommand};

use commands::target::Target;

const ABOUT: &str = r#"Command line utility to control AutoCAD

Examples:

$ acadctl ps
6A84:36C8  *  rw  foo.dwg
6A84:91B2  .  ro  bar.dwg

`foo.dwg` (6A84:36C8) contains unsaved changes while `bar.dwg` (6A84:91B2) is open read-only and contains no unsaved changes. They're open in the same AutoCAD instance (6A84).

$ acadctl exec 6A84:36C8 <<'LISP'
(defun square (x)
  (* x x))
LISP

$ acadctl eval 6A84:36C8 '(square 7)'
49"#;

#[derive(Parser)]
#[command(version, about = ABOUT, disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List instances and drawings.
    Ps {
        /// Show the full path of named drawings.
        #[arg(long)]
        long: bool,
    },
    /// Open a DWG file.
    Open {
        /// DWG file to open.
        #[arg(value_name = "FILE")]
        path: PathBuf,

        /// AutoCAD instance to use when more than one is running.
        #[arg(long, value_name = "INSTANCE")]
        instance: Option<acadctl_rpc::InstanceId>,
    },
    /// Save changes.
    Save {
        /// INSTANCE:DRAWING target shown by `acadctl ps`.
        #[arg(value_name = "TARGET")]
        target: Target,

        /// Save to a new DWG file and make it the drawing's current path.
        #[arg(long = "as", value_name = "FILE")]
        path: Option<PathBuf>,
    },
    /// Undo the previous action.
    Undo {
        /// INSTANCE:DRAWING target shown by `acadctl ps`.
        #[arg(value_name = "TARGET")]
        target: Target,
    },
    /// Redo the previous action.
    Redo {
        /// INSTANCE:DRAWING target shown by `acadctl ps`.
        #[arg(value_name = "TARGET")]
        target: Target,
    },
    /// Close a drawing.
    Close {
        /// INSTANCE:DRAWING target shown by `acadctl ps`.
        #[arg(value_name = "TARGET")]
        target: Target,

        /// Discard unsaved changes.
        #[arg(long)]
        discard: bool,
    },
    /// Evaluate an AutoLISP expression.
    Eval(EvalArgs),
    /// Execute an AutoLISP script.
    Exec(ExecArgs),
    /// Stop an instance.
    Kill {
        /// AutoCAD instance to stop when more than one is running.
        #[arg(value_name = "INSTANCE")]
        instance: Option<acadctl_rpc::InstanceId>,

        /// Stop immediately without waiting for AutoCAD to close normally.
        #[arg(long)]
        force: bool,
    },
    /// Print help for a command.
    Help {
        /// Command to describe.
        #[arg(value_name = "COMMAND")]
        command: Option<String>,
    },
}

#[derive(Args)]
struct EvalArgs {
    /// INSTANCE:DRAWING target shown by `acadctl ps`.
    #[arg(value_name = "TARGET")]
    target: Target,

    /// AutoLISP expression. Reads stdin when omitted.
    #[arg(
        value_name = "EXPRESSION",
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
    /// INSTANCE:DRAWING target shown by `acadctl ps`.
    #[arg(value_name = "TARGET")]
    target: Target,

    /// AutoLISP script. Reads stdin when omitted.
    #[arg(
        value_name = "SCRIPT",
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
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return report_parse_error(error),
    };

    match cli.command {
        Command::Ps { long } => commands::ps::run(long).await,
        Command::Open { path, instance } => commands::open::run(path, instance).await,
        Command::Save { target, path } => commands::save::run(target, path).await,
        Command::Undo { target } => {
            commands::history::run(target, commands::history::Direction::Undo).await
        }
        Command::Redo { target } => {
            commands::history::run(target, commands::history::Direction::Redo).await
        }
        Command::Close { target, discard } => commands::close::run(target, discard).await,
        Command::Eval(arguments) => {
            commands::exec::run(
                arguments.target,
                source_spec(arguments.inline, arguments.file),
                acadctl_rpc::ExecMode::Eval,
            )
            .await
        }
        Command::Exec(arguments) => {
            commands::exec::run(
                arguments.target,
                source_spec(arguments.inline, arguments.file),
                acadctl_rpc::ExecMode::Exec,
            )
            .await
        }
        Command::Kill { instance, force } => commands::kill::run(instance, force).await,
        Command::Help { command } => print_help(command),
    }
}

fn report_parse_error(error: clap::Error) -> ExitCode {
    match error.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            let _ = error.print();
            ExitCode::SUCCESS
        }
        ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            let mut command = Cli::command();
            let _ = command.print_help();
            println!();
            ExitCode::from(2)
        }
        ErrorKind::ValueValidation => {
            let argument = error_context(&error, ContextKind::InvalidArg).unwrap_or_default();
            let value = error_context(&error, ContextKind::InvalidValue).unwrap_or_default();

            if argument.contains("TARGET") {
                eprintln!("Invalid target '{value}': expected INSTANCE:DRAWING from acadctl ps.");
            } else if argument.contains("INSTANCE") {
                eprintln!("Invalid instance '{value}': expected 4–8 hexadecimal digits.");
            } else {
                eprintln!("Invalid value '{value}' for {argument}.");
            }

            ExitCode::from(2)
        }
        ErrorKind::UnknownArgument => {
            let argument = error_context(&error, ContextKind::InvalidArg).unwrap_or_default();
            eprintln!("Unknown argument '{argument}'.");
            ExitCode::from(2)
        }
        ErrorKind::InvalidSubcommand => {
            let command = error_context(&error, ContextKind::InvalidSubcommand).unwrap_or_default();
            eprintln!("Unknown command '{command}'.");
            ExitCode::from(2)
        }
        ErrorKind::MissingRequiredArgument => {
            let argument = error_context(&error, ContextKind::InvalidArg).unwrap_or_default();
            eprintln!("Missing required argument {argument}.");
            ExitCode::from(2)
        }
        ErrorKind::ArgumentConflict => {
            let argument = error_context(&error, ContextKind::InvalidArg).unwrap_or_default();
            let prior = error_context(&error, ContextKind::PriorArg).unwrap_or_default();
            eprintln!("{argument} cannot be used with {prior}.");
            ExitCode::from(2)
        }
        _ => {
            eprintln!("Invalid command arguments.");
            ExitCode::from(2)
        }
    }
}

fn error_context(error: &clap::Error, kind: ContextKind) -> Option<String> {
    error.get(kind).map(ToString::to_string)
}

fn print_help(command_name: Option<String>) -> ExitCode {
    let mut command = Cli::command();
    let command = match command_name {
        Some(name) => match command.find_subcommand_mut(&name) {
            Some(command) => {
                command.set_bin_name(format!("acadctl {name}"));
                command
            }
            None => {
                eprintln!("Unknown command '{name}'.");
                return ExitCode::from(2);
            }
        },
        None => &mut command,
    };

    match command.print_help() {
        Ok(()) => {
            println!();
            ExitCode::SUCCESS
        }
        Err(_) => ExitCode::FAILURE,
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
    fn help_uses_public_target_and_source_names() {
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

        assert!(eval_help.contains("<TARGET> [EXPRESSION]"));
        assert!(exec_help.contains("<TARGET> [SCRIPT]"));
        assert!(eval_help.contains("-f, --file <FILE>"));
        assert!(exec_help.contains("-f, --file <FILE>"));
        let save_help = command
            .find_subcommand_mut("save")
            .unwrap()
            .render_help()
            .to_string();
        assert!(save_help.contains("--as <FILE>"));
    }

    #[test]
    fn top_level_help_teaches_the_cli_through_a_terminal_session() {
        let help = Cli::command().render_help().to_string();

        assert!(help.starts_with("Command line utility to control AutoCAD\n"));
        assert!(help.contains("$ acadctl ps\n6A84:36C8  *  rw  foo.dwg"));
        assert!(help.contains("$ acadctl exec 6A84:36C8 <<'LISP'"));
        assert!(help.contains("$ acadctl eval 6A84:36C8 '(square 7)'\n49"));
        assert!(!help.contains("--file script.lsp"));
        assert!(!help.contains("document"));
        assert!(!help.contains("process"));
        assert!(!help.contains("pid"));
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
