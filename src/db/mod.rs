pub mod models;
pub mod schema;

use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use std::env;

pub fn establish_connection() -> SqliteConnection {
    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "mtgctl.db".to_string());
    
    SqliteConnection::establish(&database_url)
        .unwrap_or_else(|_| panic!("Error connecting to {}", database_url))
}

pub fn create_database_if_not_exists() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "mtgctl.db".to_string());
    
    // Create the database file if it doesn't exist
    if !std::path::Path::new(&database_url).exists() {
        std::fs::File::create(&database_url)?;
        
        let connection = &mut establish_connection();
        create_tables(connection)?;
    } else {
        // Database exists, check if we need to add new tables
        let connection = &mut establish_connection();
        add_missing_tables(connection)?;
    }
    
    Ok(())
}

fn create_tables(connection: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    diesel::sql_query(
        "CREATE TABLE matches (
            match_id INTEGER PRIMARY KEY AUTOINCREMENT,
            date TEXT NOT NULL,
            deck_name TEXT NOT NULL,
            opponent_name TEXT NOT NULL,
            opponent_deck TEXT NOT NULL,
            event_type TEXT NOT NULL,
            die_roll_winner TEXT CHECK(die_roll_winner IN ('me', 'opponent')) NOT NULL,
            match_winner TEXT CHECK(match_winner IN ('me', 'opponent', 'unknown')) NOT NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )"
    ).execute(connection)?;
    
    diesel::sql_query(
        "CREATE TABLE games (
            game_id INTEGER PRIMARY KEY AUTOINCREMENT,
            match_id INTEGER NOT NULL,
            game_number INTEGER CHECK(game_number IN (1, 2, 3)) NOT NULL,
            play_draw TEXT CHECK(play_draw IN ('play', 'draw')) NOT NULL,
            mulligans INTEGER CHECK(mulligans >= 0 AND mulligans <= 7) NOT NULL,
            opening_hand_plan TEXT,
            game_winner TEXT CHECK(game_winner IN ('me', 'opponent')) NOT NULL,
            win_condition TEXT,
            turns INTEGER CHECK(turns > 0),
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (match_id) REFERENCES matches (match_id),
            UNIQUE(match_id, game_number)
        )"
    ).execute(connection)?;
    
    diesel::sql_query(
        "CREATE TABLE decks (
            deck_id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            moxfield_url TEXT,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )"
    ).execute(connection)?;
    
    diesel::sql_query(
        "CREATE TABLE cards (
            card_id INTEGER PRIMARY KEY AUTOINCREMENT,
            deck_id INTEGER NOT NULL,
            card_name TEXT NOT NULL,
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            board TEXT CHECK(board IN ('main', 'side')) NOT NULL,
            FOREIGN KEY (deck_id) REFERENCES decks (deck_id) ON DELETE CASCADE
        )"
    ).execute(connection)?;
    
    Ok(())
}

fn add_missing_tables(connection: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    // Just try to create tables with IF NOT EXISTS - simpler and safer
    diesel::sql_query(
        "CREATE TABLE IF NOT EXISTS decks (
            deck_id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            moxfield_url TEXT,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )"
    ).execute(connection)?;

    diesel::sql_query(
        "CREATE TABLE IF NOT EXISTS cards (
            card_id INTEGER PRIMARY KEY AUTOINCREMENT,
            deck_id INTEGER NOT NULL,
            card_name TEXT NOT NULL,
            quantity INTEGER NOT NULL CHECK(quantity > 0),
            board TEXT CHECK(board IN ('main', 'side')) NOT NULL,
            FOREIGN KEY (deck_id) REFERENCES decks (deck_id) ON DELETE CASCADE
        )"
    ).execute(connection)?;

    // Add turns column to games table if it doesn't exist
    add_turns_column_if_missing(connection)?;

    // Add era columns if they don't exist
    add_era_columns_if_missing(connection)?;

    Ok(())
}

fn add_turns_column_if_missing(connection: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    // Check if the column exists by trying to select it
    let column_exists = diesel::sql_query("SELECT turns FROM games LIMIT 0")
        .execute(connection)
        .is_ok();

    if !column_exists {
        diesel::sql_query("ALTER TABLE games ADD COLUMN turns INTEGER CHECK(turns > 0)")
            .execute(connection)?;
    }

    Ok(())
}

fn add_era_columns_if_missing(connection: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    // Check if era column exists in decks table
    let decks_era_exists = diesel::sql_query("SELECT era FROM decks LIMIT 0")
        .execute(connection)
        .is_ok();

    if !decks_era_exists {
        diesel::sql_query("ALTER TABLE decks ADD COLUMN era INTEGER")
            .execute(connection)?;
    }

    // Check if era column exists in matches table
    let matches_era_exists = diesel::sql_query("SELECT era FROM matches LIMIT 0")
        .execute(connection)
        .is_ok();

    if !matches_era_exists {
        diesel::sql_query("ALTER TABLE matches ADD COLUMN era INTEGER")
            .execute(connection)?;
    }

    Ok(())
}