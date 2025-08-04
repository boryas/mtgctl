mod league;
mod game;
mod db;

use clap::{Parser, Subcommand};
use league::LeagueArgs;
use game::GameArgs;

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
    Game(GameArgs),
}

fn main() {
    // Initialize database
    if let Err(e) = db::create_database_if_not_exists() {
        eprintln!("Database initialization error: {}", e);
        std::process::exit(1);
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::League(args) => league::run(args),
        Commands::Game(args) => game::run(args),
    }
}