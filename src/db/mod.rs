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
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (match_id) REFERENCES matches (match_id),
            UNIQUE(match_id, game_number)
        )"
    ).execute(connection)?;
    
    Ok(())
}