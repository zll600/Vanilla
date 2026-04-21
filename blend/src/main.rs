mod cli;
mod commands;
mod compose;
mod context;
mod diff;
mod formats;
mod metadata;
mod nickel;
mod output;
mod sync;
mod upgrade;

use clap::Parser;

use cli::{Cli, Commands};
use commands::{cmd_status, cmd_sync, cmd_table, cmd_view};
use context::Context;
use output::log;
use sync::{SyncMode, TerminalPrompter};

fn main() {
    let cli = Cli::parse();
    let ctx = Context::new(&cli);

    if ctx.verbose {
        log::info(&format!("Home directory: {}", ctx.home_dir.display()));
        log::info(&format!("Orders directory: {}", ctx.orders_dir.display()));
        log::info(&format!(
            "OS: {}, Arch: {}",
            ctx.metadata.os, ctx.metadata.arch
        ));
    }

    let result = match cli.command {
        Some(Commands::Sync {
            packages,
            push,
            pull,
            no_rewrite,
        }) => {
            let mode = if push {
                SyncMode::PushAll
            } else if pull {
                SyncMode::PullAll
            } else {
                SyncMode::Interactive
            };
            cmd_sync(&ctx, &packages, mode, no_rewrite, &TerminalPrompter)
        }
        Some(Commands::View {
            packages,
            content_only,
            all,
            short,
        }) => cmd_view(&ctx, &packages, content_only, all, short),
        Some(Commands::Table) => cmd_table(&ctx),
        Some(Commands::Upgrade { step }) => upgrade::cmd_upgrade(&ctx, &step, |ctx| {
            cmd_sync(ctx, &[], SyncMode::Interactive, false, &TerminalPrompter)
        }),
        None => cmd_status(&ctx),
    };

    if let Err(e) = result {
        log::error(&format!("Error: {e}"));
        std::process::exit(1);
    }
}
