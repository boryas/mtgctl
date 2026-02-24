#![allow(dead_code)]

use diesel::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Winner {
    Me,
    Opponent,
}

impl Winner {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "me" => Some(Winner::Me),
            "opponent" => Some(Winner::Opponent),
            _ => None,
        }
    }

    pub fn to_string(&self) -> String {
        match self {
            Winner::Me => "me".to_string(),
            Winner::Opponent => "opponent".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlayDraw {
    Play,
    Draw,
}

impl PlayDraw {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "play" => Some(PlayDraw::Play),
            "draw" => Some(PlayDraw::Draw),
            _ => None,
        }
    }

    pub fn to_string(&self) -> String {
        match self {
            PlayDraw::Play => "play".to_string(),
            PlayDraw::Draw => "draw".to_string(),
        }
    }
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = crate::db::schema::matches)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Match {
    pub match_id: i32,
    pub date: String,
    pub deck_name: String,
    pub opponent_name: String,
    pub opponent_deck: String,
    pub event_type: String,
    pub die_roll_winner: String,
    pub match_winner: String,
    pub created_at: Option<String>,
    pub era: Option<i32>,
    pub league_id: Option<i32>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::db::schema::matches)]
pub struct NewMatch {
    pub date: String,
    pub deck_name: String,
    pub opponent_name: String,
    pub opponent_deck: String,
    pub event_type: String,
    pub die_roll_winner: String,
    pub match_winner: String,
    pub era: Option<i32>,
    pub league_id: Option<i32>,
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = crate::db::schema::games)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Game {
    pub game_id: i32,
    pub match_id: i32,
    pub game_number: i32,
    pub play_draw: String,
    pub mulligans: i32,
    pub opening_hand_plan: Option<String>,
    pub game_winner: String,
    pub win_condition: Option<String>,
    pub loss_reason: Option<String>,
    pub turns: Option<i32>,
    pub created_at: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::db::schema::games)]
pub struct NewGame {
    pub match_id: i32,
    pub game_number: i32,
    pub play_draw: String,
    pub mulligans: i32,
    pub opening_hand_plan: Option<String>,
    pub game_winner: String,
    pub win_condition: Option<String>,
    pub loss_reason: Option<String>,
    pub turns: Option<i32>,
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = crate::db::schema::decks)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Deck {
    pub deck_id: i32,
    pub name: String,
    pub moxfield_url: Option<String>,
    pub created_at: Option<String>,
    pub era: Option<i32>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::db::schema::decks)]
pub struct NewDeck {
    pub name: String,
    pub moxfield_url: Option<String>,
    pub era: Option<i32>,
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = crate::db::schema::cards)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Card {
    pub card_id: i32,
    pub deck_id: i32,
    pub card_name: String,
    pub quantity: i32,
    pub board: String,
}

#[derive(Insertable)]
#[diesel(table_name = crate::db::schema::cards)]
pub struct NewCard {
    pub deck_id: i32,
    pub card_name: String,
    pub quantity: i32,
    pub board: String,
}

// League status enum
#[derive(Debug, Clone, PartialEq)]
pub enum LeagueStatus {
    InProgress,
    Completed,
    Dropped,
}

impl LeagueStatus {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "in_progress" => Some(LeagueStatus::InProgress),
            "completed" => Some(LeagueStatus::Completed),
            "dropped" => Some(LeagueStatus::Dropped),
            _ => None,
        }
    }

    pub fn to_string(&self) -> &'static str {
        match self {
            LeagueStatus::InProgress => "in_progress",
            LeagueStatus::Completed => "completed",
            LeagueStatus::Dropped => "dropped",
        }
    }
}

// League result enum
#[derive(Debug, Clone, PartialEq)]
pub enum LeagueResult {
    Trophy,
    Elimination,
    Completed,  // Finished 5 matches without trophy or elimination (4-1, 3-2)
    Dropped,
    Pending,
}

impl LeagueResult {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "trophy" => Some(LeagueResult::Trophy),
            "elimination" => Some(LeagueResult::Elimination),
            "completed" => Some(LeagueResult::Completed),
            "dropped" => Some(LeagueResult::Dropped),
            "pending" => Some(LeagueResult::Pending),
            _ => None,
        }
    }

    pub fn to_string(&self) -> &'static str {
        match self {
            LeagueResult::Trophy => "trophy",
            LeagueResult::Elimination => "elimination",
            LeagueResult::Completed => "completed",
            LeagueResult::Dropped => "dropped",
            LeagueResult::Pending => "pending",
        }
    }
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = crate::db::schema::leagues)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct League {
    pub league_id: i32,
    pub start_date: String,
    pub end_date: Option<String>,
    pub deck_name: String,
    pub status: String,
    pub result: Option<String>,
    pub wins: i32,
    pub losses: i32,
    pub created_at: Option<String>,
}

impl League {
    pub fn get_status(&self) -> LeagueStatus {
        LeagueStatus::from_str(&self.status).unwrap_or(LeagueStatus::InProgress)
    }

    pub fn get_result(&self) -> Option<LeagueResult> {
        self.result.as_ref().and_then(|r| LeagueResult::from_str(r))
    }
}

#[derive(Insertable)]
#[diesel(table_name = crate::db::schema::leagues)]
pub struct NewLeague {
    pub start_date: String,
    pub end_date: Option<String>,
    pub deck_name: String,
    pub status: String,
    pub result: Option<String>,
    pub wins: i32,
    pub losses: i32,
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = crate::db::schema::doomsday_games)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct DoomsdayGame {
    pub id: i32,
    pub game_id: i32,
    pub doomsday: Option<bool>,
    pub pile_cards: Option<String>,
    pub pile_plan: Option<String>,
    pub juke: Option<String>,
    pub created_at: Option<String>,
    // New v2 columns
    pub pile_type: Option<String>,
    pub better_pile: Option<i32>,  // SQLite stores booleans as integers
    pub no_doomsday_reason: Option<String>,
    pub sb_juke_plan: Option<String>,
    pub pile_disruption: Option<String>,
    pub dd_intent: Option<i32>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::db::schema::doomsday_games)]
pub struct NewDoomsdayGame {
    pub game_id: i32,
    pub doomsday: Option<bool>,
    pub pile_cards: Option<String>,
    pub pile_plan: Option<String>,
    pub juke: Option<String>,
    // New v2 columns
    pub pile_type: Option<String>,
    pub better_pile: Option<i32>,
    pub no_doomsday_reason: Option<String>,
    pub sb_juke_plan: Option<String>,
    pub pile_disruption: Option<String>,
    pub dd_intent: Option<i32>,
}
