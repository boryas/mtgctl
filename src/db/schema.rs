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
    deck_types (type_id) {
        type_id -> Integer,
        category -> Text,
        archetype -> Text,
        subtype -> Nullable<Text>,
        flow_type -> Nullable<Text>,
    }
}

diesel::table! {
    decks (deck_id) {
        deck_id -> Integer,
        list_name -> Text,
        moxfield_url -> Nullable<Text>,
        created_at -> Nullable<Text>,
        era -> Nullable<Integer>,
        type_id -> Nullable<Integer>,
    }
}

diesel::table! {
    doomsday_games (id) {
        id -> Integer,
        game_id -> Integer,
        doomsday -> Nullable<Bool>,
        pile_cards -> Nullable<Text>,
        pile_plan -> Nullable<Text>,
        juke -> Nullable<Text>,
        created_at -> Nullable<Text>,
        pile_type -> Nullable<Text>,
        better_pile -> Nullable<Integer>,
        no_doomsday_reason -> Nullable<Text>,
        sb_juke_plan -> Nullable<Text>,
        pile_disruption -> Nullable<Text>,
        dd_intent -> Nullable<Integer>,
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
    leagues (league_id) {
        league_id -> Integer,
        start_date -> Text,
        end_date -> Nullable<Text>,
        deck_name -> Text,
        status -> Text,
        result -> Nullable<Text>,
        wins -> Integer,
        losses -> Integer,
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
        league_id -> Nullable<Integer>,
        my_deck_id -> Nullable<Integer>,
        opponent_type_id -> Nullable<Integer>,
    }
}

diesel::joinable!(cards -> decks (deck_id));
diesel::joinable!(decks -> deck_types (type_id));
diesel::joinable!(doomsday_games -> games (game_id));
diesel::joinable!(games -> matches (match_id));
diesel::joinable!(matches -> leagues (league_id));

diesel::allow_tables_to_appear_in_same_query!(
    cards,
    deck_types,
    decks,
    doomsday_games,
    games,
    leagues,
    matches,
);