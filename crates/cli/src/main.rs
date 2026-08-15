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
    Eval(ExecArgs),
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
struct ExecArgs {
    /// Document target shown by `acadctl ps`.
    id: String,

    /// AutoLISP file, or - for stdin. Reads stdin when omitted.
    file: Option<PathBuf>,
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
            commands::exec::run(arguments.id, arguments.file, acadctl_rpc::ExecMode::Eval).await
        }
        Command::Exec(arguments) => {
            commands::exec::run(arguments.id, arguments.file, acadctl_rpc::ExecMode::Exec).await
        }
        Command::Kill { pid, force } => commands::kill::run(pid, force).await,
    }
}
