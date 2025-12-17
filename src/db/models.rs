use diesel::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Winner {
    Me,
    Opponent,
}

impl Winner {
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub created_at: Option<String>,
    pub era: Option<i32>,
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
}

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = crate::db::schema::games)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Game {
    #[allow(dead_code)]
    pub game_id: i32,
    #[allow(dead_code)]
    pub match_id: i32,
    pub game_number: i32,
    pub play_draw: String,
    pub mulligans: i32,
    pub opening_hand_plan: Option<String>,
    pub game_winner: String,
    pub win_condition: Option<String>,
    pub loss_reason: Option<String>,
    pub turns: Option<i32>,
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub card_id: i32,
    #[allow(dead_code)]
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