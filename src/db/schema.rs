// @generated automatically by Diesel CLI.

diesel::table! {
    cards (card_id) {
        card_id -> Integer,
        deck_id -> Integer,
        card_name -> Text,
        quantity -> Integer,
        board -> Text,
    }
}

diesel::table! {
    decks (deck_id) {
        deck_id -> Integer,
        name -> Text,
        moxfield_url -> Nullable<Text>,
        created_at -> Nullable<Text>,
        era -> Nullable<Integer>,
    }
}

diesel::table! {
    games (game_id) {
        game_id -> Integer,
        match_id -> Integer,
        game_number -> Integer,
        play_draw -> Text,
        mulligans -> Integer,
        opening_hand_plan -> Nullable<Text>,
        game_winner -> Text,
        win_condition -> Nullable<Text>,
        loss_reason -> Nullable<Text>,
        turns -> Nullable<Integer>,
        created_at -> Nullable<Text>,
    }
}

diesel::table! {
    matches (match_id) {
        match_id -> Integer,
        date -> Text,
        deck_name -> Text,
        opponent_name -> Text,
        opponent_deck -> Text,
        event_type -> Text,
        die_roll_winner -> Text,
        match_winner -> Text,
        created_at -> Nullable<Text>,
        era -> Nullable<Integer>,
    }
}

diesel::joinable!(cards -> decks (deck_id));
diesel::joinable!(games -> matches (match_id));

diesel::allow_tables_to_appear_in_same_query!(
    cards,
    decks,
    games,
    matches,
);