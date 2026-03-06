mod league;
mod game;
mod db;
mod deck;
mod html_stats;
mod pilegen;

use clap::{Parser, Subcommand};
use league::LeagueArgs;
use game::GameArgs;
use deck::DeckArgs;
use pilegen::PilegenArgs;

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
    Deck(DeckArgs),
    /// Generate Doomsday pile scenarios for practice
    Pilegen(PilegenArgs),
}

fn main() {
    // Initialize database
    if let Err(e) = db::create_database_if_not_exists() {
        eprintln!("Database initialization error: {}", e);
        std::process::exit(1);
    }

    // One-time backfill: populate deck_types from toml files
    let connection = &mut db::establish_connection();
    game::backfill_deck_types(connection);

    let cli = Cli::parse();

    match cli.command {
        Commands::League(args) => league::run(args),
        Commands::Game(args) => game::run(args),
        Commands::Deck(args) => deck::run(args),
        Commands::Pilegen(args) => pilegen::run(args),
    }
}