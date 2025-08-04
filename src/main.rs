mod league;

use clap::{Parser, Subcommand};
use league::LeagueArgs;

#[derive(Parser)]
#[command(name = "mtgctl")]
#[command(about = "A CLI tool for Magic: The Gathering tournament analysis")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    League(LeagueArgs),
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::League(args) => league::run(args),
    }
}