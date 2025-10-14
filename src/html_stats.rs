use diesel::prelude::*;
use std::collections::HashMap;
use std::fs;

use crate::db::{establish_connection, models::*};
use crate::db::schema::{matches, games};
use crate::game::categorize_deck;

pub fn generate_html_stats(output_path: &str) {
    let connection = &mut establish_connection();

    // Load all matches and games
    let all_matches = matches::table
        .order(matches::date.desc())
        .load::<Match>(connection)
        .expect("Error loading matches");

    if all_matches.is_empty() {
        println!("No matches found in database");
        return;
    }

    let match_ids: Vec<i32> = all_matches.iter().map(|m| m.match_id).collect();
    let all_games = games::table
        .filter(games::match_id.eq_any(&match_ids))
        .load::<Game>(connection)
        .expect("Error loading games");

    // Generate HTML
    let html = generate_html(&all_matches, &all_games);

    // Write to file
    fs::write(output_path, html).expect("Error writing HTML file");

    println!("Generated stats HTML at: {}", output_path);
}

fn generate_html(all_matches: &[Match], all_games: &[Game]) -> String {
    let mut html = String::new();

    // HTML header with bur.io-inspired styling
    html.push_str(r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>MTG Match Statistics</title>
    <style>
        * {
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }

        body {
            font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif;
            line-height: 1.6;
            color: #333;
            max-width: 900px;
            margin: 0 auto;
            padding: 20px;
            background: #fff;
        }

        header {
            margin-bottom: 40px;
            padding-bottom: 20px;
            border-bottom: 1px solid #eee;
        }

        h1 {
            font-size: 24px;
            font-weight: 600;
            margin-bottom: 10px;
        }

        h2 {
            font-size: 18px;
            font-weight: 600;
            margin-top: 40px;
            margin-bottom: 20px;
        }

        .overall-stats {
            margin-bottom: 30px;
        }

        .stat-group {
            margin-bottom: 15px;
        }

        .stat-label {
            font-weight: 500;
            color: #666;
            margin-right: 5px;
        }

        table {
            width: 100%;
            border-collapse: collapse;
            margin-bottom: 40px;
            font-size: 14px;
        }

        thead {
            border-bottom: 2px solid #333;
        }

        th {
            text-align: left;
            padding: 10px 8px;
            font-weight: 600;
        }

        td {
            padding: 8px;
            border-bottom: 1px solid #eee;
        }

        tr:hover {
            background-color: #f9f9f9;
        }

        .win-rate {
            color: #666;
        }

        .section {
            margin-bottom: 60px;
        }

        @media (max-width: 600px) {
            body {
                padding: 15px;
            }

            table {
                font-size: 12px;
            }

            td, th {
                padding: 6px 4px;
            }
        }
    </style>
</head>
<body>
    <header>
        <h1>MTG Match Statistics</h1>
        <p>Generated on "#);

    html.push_str(&chrono::Local::now().format("%Y-%m-%d %H:%M").to_string());
    html.push_str("</p>\n    </header>\n\n");

    // Overall statistics
    html.push_str("    <div class=\"overall-stats\">\n");
    html.push_str(&generate_overall_stats_html(all_matches, all_games));
    html.push_str("    </div>\n\n");

    // Sliced statistics sections
    let slices = vec![
        ("my-deck", "Statistics by My Deck"),
        ("opponent-deck", "Statistics by Opponent Deck"),
        ("deck-category", "Statistics by Deck Category"),
        ("game-number", "Statistics by Game Number"),
        ("mulligans", "Statistics by Mulligan Count"),
        ("game-plan", "Opening Hand Game Plan"),
        ("win-condition", "Win Conditions"),
        ("game-length", "Statistics by Game Length"),
    ];

    for (slice_type, title) in slices {
        html.push_str("    <div class=\"section\">\n");
        html.push_str(&format!("        <h2>{}</h2>\n", title));
        html.push_str(&generate_slice_table(all_matches, all_games, slice_type));
        html.push_str("    </div>\n\n");
    }

    html.push_str("</body>\n</html>");
    html
}

fn generate_overall_stats_html(all_matches: &[Match], all_games: &[Game]) -> String {
    let mut html = String::new();

    // Match statistics
    let total_matches = all_matches.len();
    let wins = all_matches.iter().filter(|m| m.match_winner == "me").count();
    let losses = total_matches - wins;
    let win_rate = if total_matches > 0 { (wins as f64 / total_matches as f64) * 100.0 } else { 0.0 };

    html.push_str("        <div class=\"stat-group\">\n");
    html.push_str(&format!("            <span class=\"stat-label\">Overall Record:</span> {}-{} ({:.1}%)\n", wins, losses, win_rate));
    html.push_str("        </div>\n");

    // Die roll statistics
    let die_roll_wins = all_matches.iter().filter(|m| m.die_roll_winner == "me").count();
    let die_roll_rate = if total_matches > 0 { (die_roll_wins as f64 / total_matches as f64) * 100.0 } else { 0.0 };
    html.push_str("        <div class=\"stat-group\">\n");
    html.push_str(&format!("            <span class=\"stat-label\">Die Roll Win Rate:</span> {:.1}%\n", die_roll_rate));
    html.push_str("        </div>\n");

    // Game statistics
    let total_games = all_games.len();
    let game_wins = all_games.iter().filter(|g| g.game_winner == "me").count();
    let game_losses = total_games - game_wins;
    let game_win_rate = if total_games > 0 { (game_wins as f64 / total_games as f64) * 100.0 } else { 0.0 };

    html.push_str("        <div class=\"stat-group\">\n");
    html.push_str(&format!("            <span class=\"stat-label\">Game Record:</span> {}-{} ({:.1}%)\n", game_wins, game_losses, game_win_rate));
    html.push_str("        </div>\n");

    // Play/Draw statistics
    let play_games = all_games.iter().filter(|g| g.play_draw == "play").collect::<Vec<_>>();
    let draw_games = all_games.iter().filter(|g| g.play_draw == "draw").collect::<Vec<_>>();

    if !play_games.is_empty() {
        let play_wins = play_games.iter().filter(|g| g.game_winner == "me").count();
        let play_win_rate = (play_wins as f64 / play_games.len() as f64) * 100.0;
        html.push_str("        <div class=\"stat-group\">\n");
        html.push_str(&format!("            <span class=\"stat-label\">On the Play:</span> {}-{} ({:.1}%)\n", play_wins, play_games.len() - play_wins, play_win_rate));
        html.push_str("        </div>\n");
    }

    if !draw_games.is_empty() {
        let draw_wins = draw_games.iter().filter(|g| g.game_winner == "me").count();
        let draw_win_rate = (draw_wins as f64 / draw_games.len() as f64) * 100.0;
        html.push_str("        <div class=\"stat-group\">\n");
        html.push_str(&format!("            <span class=\"stat-label\">On the Draw:</span> {}-{} ({:.1}%)\n", draw_wins, draw_games.len() - draw_wins, draw_win_rate));
        html.push_str("        </div>\n");
    }

    // Mulligan statistics
    let winning_games: Vec<&Game> = all_games.iter().filter(|g| g.game_winner == "me").collect();
    let losing_games: Vec<&Game> = all_games.iter().filter(|g| g.game_winner == "opponent").collect();

    let total_mulligans: i32 = all_games.iter().map(|g| g.mulligans).sum();
    let avg_mulligans = if total_games > 0 { total_mulligans as f64 / total_games as f64 } else { 0.0 };

    let win_mulligans: i32 = winning_games.iter().map(|g| g.mulligans).sum();
    let loss_mulligans: i32 = losing_games.iter().map(|g| g.mulligans).sum();

    let avg_win_mulligans = if !winning_games.is_empty() { win_mulligans as f64 / winning_games.len() as f64 } else { 0.0 };
    let avg_loss_mulligans = if !losing_games.is_empty() { loss_mulligans as f64 / losing_games.len() as f64 } else { 0.0 };

    html.push_str("        <div class=\"stat-group\">\n");
    html.push_str(&format!("            <span class=\"stat-label\">Average Mulligans:</span> {:.2} (wins: {:.2}, losses: {:.2})\n", avg_mulligans, avg_win_mulligans, avg_loss_mulligans));
    html.push_str("        </div>\n");

    // Game length statistics
    let games_with_turns: Vec<&Game> = all_games.iter().filter(|g| g.turns.is_some()).collect();
    if !games_with_turns.is_empty() {
        let total_turns: i32 = games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
        let avg_turns = total_turns as f64 / games_with_turns.len() as f64;

        let winning_games_with_turns: Vec<&Game> = games_with_turns.iter()
            .filter(|g| g.game_winner == "me")
            .copied()
            .collect();
        let losing_games_with_turns: Vec<&Game> = games_with_turns.iter()
            .filter(|g| g.game_winner == "opponent")
            .copied()
            .collect();

        let win_turns: i32 = winning_games_with_turns.iter().map(|g| g.turns.unwrap()).sum();
        let loss_turns: i32 = losing_games_with_turns.iter().map(|g| g.turns.unwrap()).sum();

        let avg_win_turns = if !winning_games_with_turns.is_empty() {
            win_turns as f64 / winning_games_with_turns.len() as f64
        } else { 0.0 };
        let avg_loss_turns = if !losing_games_with_turns.is_empty() {
            loss_turns as f64 / losing_games_with_turns.len() as f64
        } else { 0.0 };

        html.push_str("        <div class=\"stat-group\">\n");
        html.push_str(&format!("            <span class=\"stat-label\">Average Game Length:</span> {:.1} turns (wins: {:.1}, losses: {:.1})\n", avg_turns, avg_win_turns, avg_loss_turns));
        html.push_str("        </div>\n");
    }

    html
}

fn generate_slice_table(all_matches: &[Match], all_games: &[Game], slice_type: &str) -> String {
    match slice_type {
        "my-deck" => generate_my_deck_table(all_matches),
        "opponent-deck" => generate_opponent_deck_table(all_matches),
        "deck-category" => generate_deck_category_table(all_matches),
        "game-number" => generate_game_number_table(all_games),
        "mulligans" => generate_mulligan_table(all_games),
        "game-plan" => generate_game_plan_table(all_games),
        "win-condition" => generate_win_condition_table(all_games),
        "game-length" => generate_game_length_table(all_games),
        _ => String::new(),
    }
}

fn generate_my_deck_table(all_matches: &[Match]) -> String {
    let mut deck_stats: HashMap<String, Vec<&Match>> = HashMap::new();
    for m in all_matches {
        deck_stats.entry(m.deck_name.clone()).or_default().push(m);
    }

    let mut deck_vec: Vec<_> = deck_stats.into_iter()
        .map(|(deck, matches)| {
            let wins = matches.iter().filter(|m| m.match_winner == "me").count();
            let total = matches.len();
            let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };
            (deck, wins, total, win_rate)
        })
        .collect();

    deck_vec.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
    });

    generate_table(&["Deck", "Record", "Win Rate"], deck_vec.iter().map(|(deck, wins, total, win_rate)| {
        vec![
            deck.clone(),
            format!("{}-{}", wins, total - wins),
            format!("{:.1}%", win_rate),
        ]
    }).collect())
}

fn generate_opponent_deck_table(all_matches: &[Match]) -> String {
    let mut deck_stats: HashMap<String, Vec<&Match>> = HashMap::new();
    for m in all_matches {
        deck_stats.entry(m.opponent_deck.clone()).or_default().push(m);
    }

    let mut deck_vec: Vec<_> = deck_stats.into_iter()
        .map(|(deck, matches)| {
            let wins = matches.iter().filter(|m| m.match_winner == "me").count();
            let total = matches.len();
            let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };
            (deck, wins, total, win_rate)
        })
        .collect();

    deck_vec.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
    });

    generate_table(&["Opponent Deck", "Record", "Win Rate"], deck_vec.iter().map(|(deck, wins, total, win_rate)| {
        vec![
            deck.clone(),
            format!("{}-{}", wins, total - wins),
            format!("{:.1}%", win_rate),
        ]
    }).collect())
}

fn generate_deck_category_table(all_matches: &[Match]) -> String {
    let mut category_stats: HashMap<String, Vec<&Match>> = HashMap::new();
    for m in all_matches {
        let category = categorize_deck(&m.opponent_deck);
        category_stats.entry(category.to_string().to_string()).or_default().push(m);
    }

    let mut category_vec: Vec<_> = category_stats.into_iter()
        .map(|(category, matches)| {
            let wins = matches.iter().filter(|m| m.match_winner == "me").count();
            let total = matches.len();
            let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };
            (category, wins, total, win_rate)
        })
        .collect();

    category_vec.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
    });

    generate_table(&["Category", "Record", "Win Rate"], category_vec.iter().map(|(category, wins, total, win_rate)| {
        vec![
            category.clone(),
            format!("{}-{}", wins, total - wins),
            format!("{:.1}%", win_rate),
        ]
    }).collect())
}

fn generate_game_number_table(all_games: &[Game]) -> String {
    let mut game_stats: HashMap<i32, Vec<&Game>> = HashMap::new();
    for g in all_games {
        game_stats.entry(g.game_number).or_default().push(g);
    }

    let mut game_vec: Vec<_> = (1..=3)
        .filter_map(|game_num| {
            game_stats.get(&game_num).map(|games| {
                let wins = games.iter().filter(|g| g.game_winner == "me").count();
                let total = games.len();
                let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };
                (game_num, wins, total, win_rate)
            })
        })
        .collect();

    game_vec.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
    });

    generate_table(&["Game", "Record", "Win Rate"], game_vec.iter().map(|(game_num, wins, total, win_rate)| {
        vec![
            format!("Game {}", game_num),
            format!("{}-{}", wins, total - wins),
            format!("{:.1}%", win_rate),
        ]
    }).collect())
}

fn generate_mulligan_table(all_games: &[Game]) -> String {
    let mut mulligan_stats: HashMap<i32, Vec<&Game>> = HashMap::new();
    for g in all_games {
        mulligan_stats.entry(g.mulligans).or_default().push(g);
    }

    let mut mulligan_vec: Vec<_> = mulligan_stats.into_iter()
        .map(|(mulligans, games)| {
            let wins = games.iter().filter(|g| g.game_winner == "me").count();
            let total = games.len();
            let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };
            (mulligans, wins, total, win_rate)
        })
        .collect();

    mulligan_vec.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
    });

    generate_table(&["Mulligans", "Record", "Win Rate"], mulligan_vec.iter().map(|(mulligans, wins, total, win_rate)| {
        vec![
            format!("{}", mulligans),
            format!("{}-{}", wins, total - wins),
            format!("{:.1}%", win_rate),
        ]
    }).collect())
}

fn generate_game_plan_table(all_games: &[Game]) -> String {
    let mut plan_stats: HashMap<String, Vec<&Game>> = HashMap::new();
    for g in all_games {
        let plan = g.opening_hand_plan.as_deref().unwrap_or("No Plan");
        plan_stats.entry(plan.to_string()).or_default().push(g);
    }

    let mut plan_vec: Vec<_> = plan_stats.into_iter()
        .map(|(plan, games)| {
            let wins = games.iter().filter(|g| g.game_winner == "me").count();
            let total = games.len();
            let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };
            (plan, wins, total, win_rate)
        })
        .collect();

    plan_vec.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
    });

    generate_table(&["Game Plan", "Record", "Win Rate"], plan_vec.iter().map(|(plan, wins, total, win_rate)| {
        vec![
            plan.clone(),
            format!("{}-{}", wins, total - wins),
            format!("{:.1}%", win_rate),
        ]
    }).collect())
}

fn generate_win_condition_table(all_games: &[Game]) -> String {
    let mut win_con_stats: HashMap<String, i32> = HashMap::new();

    for g in all_games.iter().filter(|g| g.game_winner == "me") {
        let win_con = g.win_condition.as_deref().unwrap_or("Unknown");
        *win_con_stats.entry(win_con.to_string()).or_insert(0) += 1;
    }

    let mut win_con_vec: Vec<_> = win_con_stats.into_iter().collect();
    win_con_vec.sort_by(|a, b| b.1.cmp(&a.1));

    generate_table(&["Win Condition", "Wins"], win_con_vec.iter().map(|(win_con, wins)| {
        vec![
            win_con.clone(),
            format!("{}", wins),
        ]
    }).collect())
}

fn generate_game_length_table(all_games: &[Game]) -> String {
    let mut length_stats: HashMap<String, Vec<&Game>> = HashMap::new();

    for g in all_games {
        let length_category = match g.turns {
            None => "No turn data".to_string(),
            Some(turns) => {
                match turns {
                    1..=3 => "Very Short (1-3 turns)".to_string(),
                    4..=6 => "Short (4-6 turns)".to_string(),
                    7..=10 => "Medium (7-10 turns)".to_string(),
                    11..=15 => "Long (11-15 turns)".to_string(),
                    _ => "Very Long (16+ turns)".to_string(),
                }
            }
        };
        length_stats.entry(length_category).or_default().push(g);
    }

    let mut length_vec: Vec<_> = length_stats.into_iter()
        .map(|(category, games)| {
            let wins = games.iter().filter(|g| g.game_winner == "me").count();
            let total = games.len();
            let win_rate = if total > 0 { (wins as f64 / total as f64) * 100.0 } else { 0.0 };

            let avg_turns = if category == "No turn data" {
                None
            } else {
                let turns_sum: i32 = games.iter().filter_map(|g| g.turns).sum();
                let turns_count = games.iter().filter(|g| g.turns.is_some()).count();
                if turns_count > 0 {
                    Some(turns_sum as f64 / turns_count as f64)
                } else {
                    None
                }
            };

            (category, wins, total, win_rate, avg_turns)
        })
        .collect();

    // Sort by game length order (Very Short -> Very Long)
    length_vec.sort_by(|a, b| {
        let order_a = match a.0.as_str() {
            "Very Short (1-3 turns)" => 0,
            "Short (4-6 turns)" => 1,
            "Medium (7-10 turns)" => 2,
            "Long (11-15 turns)" => 3,
            "Very Long (16+ turns)" => 4,
            "No turn data" => 5,
            _ => 6,
        };
        let order_b = match b.0.as_str() {
            "Very Short (1-3 turns)" => 0,
            "Short (4-6 turns)" => 1,
            "Medium (7-10 turns)" => 2,
            "Long (11-15 turns)" => 3,
            "Very Long (16+ turns)" => 4,
            "No turn data" => 5,
            _ => 6,
        };
        order_a.cmp(&order_b)
    });

    generate_table(&["Game Length", "Record", "Win Rate", "Avg Turns"], length_vec.iter().map(|(category, wins, total, win_rate, avg_turns)| {
        vec![
            category.clone(),
            format!("{}-{}", wins, total - wins),
            format!("{:.1}%", win_rate),
            avg_turns.map(|a| format!("{:.1}", a)).unwrap_or_else(|| "-".to_string()),
        ]
    }).collect())
}

fn generate_table(headers: &[&str], rows: Vec<Vec<String>>) -> String {
    let mut html = String::new();

    html.push_str("        <table>\n");
    html.push_str("            <thead>\n");
    html.push_str("                <tr>\n");
    for header in headers {
        html.push_str(&format!("                    <th>{}</th>\n", header));
    }
    html.push_str("                </tr>\n");
    html.push_str("            </thead>\n");
    html.push_str("            <tbody>\n");

    for row in rows {
        html.push_str("                <tr>\n");
        for cell in row {
            html.push_str(&format!("                    <td>{}</td>\n", cell));
        }
        html.push_str("                </tr>\n");
    }

    html.push_str("            </tbody>\n");
    html.push_str("        </table>\n");

    html
}
