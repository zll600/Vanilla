use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "blend",
    version,
    about = "Cross-platform dotfiles manager with Nickel DSL"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Show what would be done without making changes
    #[arg(short = 'n', long, global = true)]
    pub dry_run: bool,

    /// Verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Override home directory (for testing)
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,

    /// Override orders directory (default: ../orders relative to blend)
    #[arg(long, global = true)]
    pub orders: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Bidirectional sync: push repo configs to targets or pull deployed changes back
    Sync {
        /// Packages to sync (default: all)
        packages: Vec<String>,

        /// Auto-resolve: push all without prompting (repo wins)
        #[arg(long)]
        push: bool,

        /// Auto-resolve: pull all deployed changes back (deployed wins)
        #[arg(long)]
        pull: bool,

        /// Disable .ncl rewrite on pull; show diff and Nickel snippets for manual merge instead
        #[arg(long)]
        no_rewrite: bool,
    },

    /// Preview generated config and diff from deployed
    View {
        /// Packages to view (default: all)
        packages: Vec<String>,

        /// Only show generated content (no diff)
        #[arg(short = 'c', long)]
        content_only: bool,

        /// Show both generated content and diff
        #[arg(short = 'a', long)]
        all: bool,

        /// Omit up-to-date files from output (only show files with changes)
        #[arg(short = 's', long)]
        short: bool,
    },

    /// Output package info as HTML table (for README generation)
    Table,

    /// System upgrade: update packages, tools, and dotfiles
    #[command(alias = "s")]
    Upgrade {
        #[command(subcommand)]
        step: Option<UpgradeStep>,
    },
}

#[derive(Subcommand)]
pub enum UpgradeStep {
    /// Update Homebrew packages (macOS only)
    Homebrew,
    /// Update system packages via paru (Linux/Arch only)
    Pacman,
    /// Update Proto toolchain versions
    Proto,
}
