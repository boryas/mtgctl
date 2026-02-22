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

    // Add loss_reason column to games table if it doesn't exist
    add_loss_reason_column_if_missing(connection)?;

    // Add leagues table if it doesn't exist
    add_leagues_table_if_missing(connection)?;

    // Add doomsday_games table if it doesn't exist
    add_doomsday_games_table_if_missing(connection)?;

    // Add missing columns to doomsday_games table
    add_doomsday_games_columns_if_missing(connection)?;

    // Add new doomsday tracking columns (v2)
    add_doomsday_games_v2_columns_if_missing(connection)?;

    // Add league_id column to matches table if it doesn't exist
    add_league_id_column_if_missing(connection)?;

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

fn add_loss_reason_column_if_missing(connection: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    // Check if the column exists by trying to select it
    let column_exists = diesel::sql_query("SELECT loss_reason FROM games LIMIT 0")
        .execute(connection)
        .is_ok();

    if !column_exists {
        diesel::sql_query("ALTER TABLE games ADD COLUMN loss_reason TEXT")
            .execute(connection)?;
    }

    Ok(())
}

fn add_leagues_table_if_missing(connection: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    diesel::sql_query(
        "CREATE TABLE IF NOT EXISTS leagues (
            league_id INTEGER PRIMARY KEY AUTOINCREMENT,
            start_date TEXT NOT NULL,
            end_date TEXT,
            deck_name TEXT NOT NULL,
            status TEXT CHECK(status IN ('in_progress', 'completed', 'dropped')) NOT NULL,
            result TEXT CHECK(result IN ('trophy', 'elimination', 'dropped', 'pending')),
            wins INTEGER DEFAULT 0,
            losses INTEGER DEFAULT 0,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )"
    ).execute(connection)?;

    Ok(())
}

fn add_doomsday_games_table_if_missing(connection: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    diesel::sql_query(
        "CREATE TABLE IF NOT EXISTS doomsday_games (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            game_id INTEGER NOT NULL UNIQUE REFERENCES games(game_id),
            doomsday BOOLEAN,
            pile_cards TEXT,
            pile_plan TEXT,
            juke TEXT,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )"
    ).execute(connection)?;

    Ok(())
}

fn add_doomsday_games_columns_if_missing(connection: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    // Check if doomsday column exists
    let doomsday_exists = diesel::sql_query("SELECT doomsday FROM doomsday_games LIMIT 0")
        .execute(connection)
        .is_ok();

    if !doomsday_exists {
        diesel::sql_query("ALTER TABLE doomsday_games ADD COLUMN doomsday BOOLEAN")
            .execute(connection)?;
    }

    // Check if pile_cards column exists
    let pile_cards_exists = diesel::sql_query("SELECT pile_cards FROM doomsday_games LIMIT 0")
        .execute(connection)
        .is_ok();

    if !pile_cards_exists {
        diesel::sql_query("ALTER TABLE doomsday_games ADD COLUMN pile_cards TEXT")
            .execute(connection)?;
    }

    // Check if pile_plan column exists
    let pile_plan_exists = diesel::sql_query("SELECT pile_plan FROM doomsday_games LIMIT 0")
        .execute(connection)
        .is_ok();

    if !pile_plan_exists {
        diesel::sql_query("ALTER TABLE doomsday_games ADD COLUMN pile_plan TEXT")
            .execute(connection)?;
    }

    // Check if juke column exists
    let juke_exists = diesel::sql_query("SELECT juke FROM doomsday_games LIMIT 0")
        .execute(connection)
        .is_ok();

    if !juke_exists {
        diesel::sql_query("ALTER TABLE doomsday_games ADD COLUMN juke TEXT")
            .execute(connection)?;
    }

    Ok(())
}

fn add_league_id_column_if_missing(connection: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    // Check if the column exists by trying to select it
    let column_exists = diesel::sql_query("SELECT league_id FROM matches LIMIT 0")
        .execute(connection)
        .is_ok();

    if !column_exists {
        diesel::sql_query("ALTER TABLE matches ADD COLUMN league_id INTEGER REFERENCES leagues(league_id)")
            .execute(connection)?;
    }

    Ok(())
}

fn add_doomsday_games_v2_columns_if_missing(connection: &mut SqliteConnection) -> Result<(), Box<dyn std::error::Error>> {
    // Add pile_type column (replaces pile_cards for new entries)
    let pile_type_exists = diesel::sql_query("SELECT pile_type FROM doomsday_games LIMIT 0")
        .execute(connection)
        .is_ok();

    if !pile_type_exists {
        diesel::sql_query("ALTER TABLE doomsday_games ADD COLUMN pile_type TEXT")
            .execute(connection)?;
    }

    // Add better_pile column (boolean for losses where doomsday resolved)
    let better_pile_exists = diesel::sql_query("SELECT better_pile FROM doomsday_games LIMIT 0")
        .execute(connection)
        .is_ok();

    if !better_pile_exists {
        diesel::sql_query("ALTER TABLE doomsday_games ADD COLUMN better_pile INTEGER")
            .execute(connection)?;
    }

    // Add no_doomsday_reason column (why doomsday wasn't cast)
    let no_doomsday_reason_exists = diesel::sql_query("SELECT no_doomsday_reason FROM doomsday_games LIMIT 0")
        .execute(connection)
        .is_ok();

    if !no_doomsday_reason_exists {
        diesel::sql_query("ALTER TABLE doomsday_games ADD COLUMN no_doomsday_reason TEXT")
            .execute(connection)?;
    }

    // Add sb_juke_plan column (renamed from juke - asked BEFORE the game)
    let sb_juke_plan_exists = diesel::sql_query("SELECT sb_juke_plan FROM doomsday_games LIMIT 0")
        .execute(connection)
        .is_ok();

    if !sb_juke_plan_exists {
        diesel::sql_query("ALTER TABLE doomsday_games ADD COLUMN sb_juke_plan TEXT")
            .execute(connection)?;
        // Migrate existing juke data to sb_juke_plan
        diesel::sql_query("UPDATE doomsday_games SET sb_juke_plan = juke WHERE juke IS NOT NULL")
            .execute(connection)?;
    }

    // Add pile_disruption column (disruption faced when doomsday resolved)
    let pile_disruption_exists = diesel::sql_query("SELECT pile_disruption FROM doomsday_games LIMIT 0")
        .execute(connection)
        .is_ok();

    if !pile_disruption_exists {
        diesel::sql_query("ALTER TABLE doomsday_games ADD COLUMN pile_disruption TEXT")
            .execute(connection)?;
    }

    Ok(())
}