//! `ghost-hook` binary: host-adapter lifecycle hook invocations.
//!
//! Hosts (OpenClaw, Claude Code, Gemini, Codex, OpenCode) invoke this
//! binary per lifecycle event, as referenced by each adapter's `install()`
//! payload. See [`ghost_stealth::hook`] for the full contract.

use clap::Parser;
use ghost_stealth::hook::{run, HookCommand};

#[derive(Parser)]
#[command(
    name = "ghost-hook",
    version,
    about = "GHOST-Chimera host-adapter lifecycle hooks: observe session, assemble context, observe outcome"
)]
struct Cli {
    #[command(subcommand)]
    command: HookCommand,
}

fn main() {
    let cli = Cli::parse();
    let config = ghost_core::GhostConfig::from_env();
    std::process::exit(run(cli.command, &config));
}
