//! `runwick` opens the app. `runwick host` runs the background host that keeps CLI runs alive.
//! Inside a Runwick terminal, `runwick delegate|workers|wait|result|send|report|status` let an
//! agent CLI hand work to other CLIs (see `agent`). `runwick menubar` keeps Runwick in the macOS
//! menu bar while the window is closed (see `menubar`).

mod agent;
mod menubar;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "runwick", version, about = "Runwick — AI CLI 작업실")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Open the window showing this first: activity, quit or session:<id> (used by the menu bar).
    #[arg(long, value_name = "WHAT", hide = true)]
    open: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the host that keeps CLI runs alive (started automatically by the app).
    Host {
        /// Detach from the terminal (used when the app starts the host).
        #[arg(long)]
        daemon: bool,
        /// Relay the host protocol over stdin/stdout (used over SSH by another computer).
        #[arg(long)]
        stdio: bool,
    },
    /// Show running work in the macOS menu bar while the window is closed (started by the app).
    Menubar(menubar::MenubarArgs),
    #[command(flatten)]
    Agent(agent::AgentCommand),
}

fn main() -> anyhow::Result<()> {
    // Older macOS versions pass a process serial number to apps started from Finder.
    let cli = Cli::parse_from(std::env::args_os().filter(|a| !a.to_string_lossy().starts_with("-psn_")));
    let exe = std::env::current_exe()?;
    match cli.command {
        None => {
            if let Some(what) = cli.open.as_deref() {
                match runwick_app::presence::OpenRequest::parse(what) {
                    Some(req) => runwick_app::presence::request_open(req),
                    None => eprintln!("--open: 알 수 없는 값이에요 ({what})"),
                }
            }
            runwick_app::run(runwick_app::Options { exe })
        }
        Some(Command::Host { stdio: true, .. }) => runwick_host::bridge::run_stdio(&exe),
        Some(Command::Host { daemon, .. }) => {
            if daemon {
                runwick_host::bridge::daemonize();
            }
            runwick_host::run()
        }
        Some(Command::Menubar(args)) => menubar::run(&exe, args),
        Some(Command::Agent(cmd)) => std::process::exit(agent::run(cmd)),
    }
}
