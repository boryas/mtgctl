use clap::Args;
use dialoguer::Confirm;
use diesel::prelude::*;
use rand::Rng;
use serde::Deserialize;
use skim::prelude::*;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use crate::db::schema::{cards, deck_types, decks};
use crate::db::{establish_connection, models::*};

// ── Config deserialization ────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct PilegenConfig {
    #[serde(default)]
    cards: Vec<CardDef>,
    #[serde(default)]
    archetypes: HashMap<String, ArchetypeConfig>,
}

/// A card the generator knows about. Cards not in the catalog are treated as
/// generic non-land spells: hand-eligible, not permanent candidates, not exileable.
#[derive(Deserialize, Clone, Default)]
struct CardDef {
    name: String,
    /// "land" | "creature" | "planeswalker" | "artifact" | "instant" | "sorcery" | "enchantment"
    card_type: String,

    // Land-specific
    #[serde(default)]
    is_fetch: bool,
    #[serde(default)]
    produces_black: bool,
    #[serde(default)]
    annotation_options: Vec<String>,

    // DFC: back-face properties
    #[serde(default)]
    flipped_name: Option<String>,
    #[serde(default)]
    flipped_card_type: Option<String>,
    #[serde(default)]
    flipped_loyalty: Option<i32>,

    // Planeswalker
    #[serde(default)]
    loyalty: Option<i32>,

    // Hand / exile rules
    #[serde(default)]
    exileable: bool,

    /// Relative likelihood of appearing as a permanent in play.
    /// Default 100. Set to 1 for cards that are almost never played out (100:1 odds).
    #[serde(default)]
    play_weight: Option<u32>,

    /// Land is usually cracked before Doomsday — filtered by stage probability.
    #[serde(default)]
    cracked_land: bool,

    /// Token creature name placed when this permanent enters (e.g. "Orc Army (1/1)").
    #[serde(default)]
    entry_token: Option<String>,
    /// Percent probability of entry token appearing (default 62).
    #[serde(default)]
    entry_token_prob: Option<u32>,

    /// Card may leave clue tokens even if no longer in play (e.g. Tamiyo).
    #[serde(default)]
    generates_clues: bool,
}

#[derive(Deserialize, Default)]
struct ArchetypeConfig {
    #[serde(default)]
    lands: Vec<String>,
    #[serde(default)]
    hand: Vec<String>,
    #[serde(default)]
    pressure: Vec<String>,
    #[serde(default)]
    disruption: Vec<String>,
    #[serde(default)]
    other: Vec<String>,
    #[serde(default)]
    used: Vec<String>,

    /// Weights for number of pressure/disruption/other/used cards in play.
    /// Index = count, value = relative weight. Falls back to global defaults if empty.
    #[serde(default)]
    pressure_weights: Vec<u32>,
    #[serde(default)]
    disruption_weights: Vec<u32>,
    #[serde(default)]
    other_weights: Vec<u32>,
    #[serde(default)]
    used_weights: Vec<u32>,

    /// Offset from our land count when estimating opponent's game stage.
    #[serde(default)]
    lands_delta: i32,

    /// Percent chance (0–100) of revealing one known card in opponent's hand. Default 15.
    #[serde(default)]
    known_hand_prob: Option<u32>,
}

// ── Game stage (derived from turn number) ────────────────────────────────────

#[derive(Clone, Copy)]
enum GameStage {
    Early,
    Mid,
    Late,
}

fn stage_from_turn(turn: u8) -> GameStage {
    match turn {
        0..=3 => GameStage::Early,
        4..=5 => GameStage::Mid,
        _ => GameStage::Late,
    }
}

fn stage_label(turn: u8) -> &'static str {
    match turn {
        0..=3 => "Early",
        4..=5 => "Mid",
        _ => "Late",
    }
}

// ── Game state ────────────────────────────────────────────────────────────────

enum PermanentKind {
    Creature { tapped: bool },
    Planeswalker { loyalty: i32, activated: bool },
    Artifact,
}

struct Permanent {
    name: String,
    kind: PermanentKind,
}

struct GameState {
    my_deck_name: String,
    opponent_archetype: String,
    turn: u8,
    on_play: bool,
    // Resource accounting debug fields
    our_cards_seen: i32,
    our_mulls: u8,
    our_noncantrip_spells: i32,
    opp_cards_seen: i32,
    opp_mulls: u8,
    life_total: i32,
    lands_in_play: Vec<(String, bool)>, // (display_name, is_tapped)
    hand: Vec<String>,
    my_permanents: Vec<Permanent>,
    clue_tokens: i32,
    exile: Vec<String>,
    opponent_hand_unknown: i32,
    opponent_known_hand: Vec<String>,
    opponent_lands: Vec<(String, bool)>, // (name, is_tapped)
    opponent_permanents: Vec<String>,
    opponent_clue_tokens: i32,
    opponent_used: Vec<String>,
}

// ── Clap args ─────────────────────────────────────────────────────────────────

#[derive(Args)]
pub struct PilegenArgs {}

// ── Public entry point ────────────────────────────────────────────────────────

pub fn run(_args: PilegenArgs) {
    let config = load_config();

    let deck_name = match select_my_doomsday_deck() {
        Some(d) => d,
        None => return,
    };

    let opponent = match select_opponent_archetype(&config) {
        Some(o) => o,
        None => return,
    };

    let all_cards = load_deck_cards(&deck_name);
    if all_cards.is_empty() {
        println!(
            "No cards found for '{}'. Import its card list first: mtgctl deck import",
            deck_name
        );
        return;
    }

    loop {
        let state = generate_scenario(&deck_name, &opponent, &config, &all_cards);
        display_scenario(&state);

        println!();
        if !Confirm::new()
            .with_prompt("Generate another scenario?")
            .default(true)
            .interact()
            .unwrap_or(false)
        {
            break;
        }
    }
}

// ── Config loading ────────────────────────────────────────────────────────────

fn load_config() -> PilegenConfig {
    let path = "definitions/pilegen.toml";
    match std::fs::read_to_string(path) {
        Ok(content) => toml::from_str(&content).unwrap_or_else(|e| {
            eprintln!("Warning: could not parse {}: {}", path, e);
            PilegenConfig::default()
        }),
        Err(_) => {
            eprintln!("Warning: {} not found, using defaults", path);
            PilegenConfig::default()
        }
    }
}

// ── Selection helpers ─────────────────────────────────────────────────────────

fn fuzzy_select(prompt: &str, options: &[String]) -> Option<String> {
    if options.is_empty() {
        return None;
    }
    let prompt_str = format!("{}: ", prompt);
    let skim_options = SkimOptionsBuilder::default()
        .prompt(Some(&prompt_str))
        .build()
        .unwrap();
    let input = options.join("\n");
    let item_reader = SkimItemReader::default();
    let items = item_reader.of_bufread(Cursor::new(input));
    match Skim::run_with(&skim_options, Some(items)) {
        Some(output) if !output.is_abort => {
            if output.selected_items.is_empty() {
                let q = output.query.trim().to_string();
                if q.is_empty() { None } else { Some(q) }
            } else {
                Some(output.selected_items[0].output().to_string())
            }
        }
        _ => None,
    }
}

fn select_my_doomsday_deck() -> Option<String> {
    let conn = &mut establish_connection();

    // Doomsday decks with at least one card imported
    let dd_type_ids: Vec<i32> = deck_types::table
        .filter(deck_types::flow_type.eq("doomsday"))
        .select(deck_types::type_id)
        .load(conn)
        .unwrap_or_default();

    let candidates: Vec<Deck> = decks::table
        .filter(decks::type_id.eq_any(&dd_type_ids))
        .load(conn)
        .unwrap_or_default();

    // Only keep decks that have cards imported
    let available: Vec<String> = candidates
        .into_iter()
        .filter(|d| {
            cards::table
                .filter(cards::deck_id.eq(d.deck_id))
                .count()
                .get_result::<i64>(conn)
                .unwrap_or(0) > 0
        })
        .map(|d| d.list_name)
        .collect();

    if available.is_empty() {
        println!("No Doomsday decks with imported cards found. Run: mtgctl deck import");
        return None;
    }

    if available.len() == 1 {
        println!("Using deck: {}", available[0]);
        return Some(available[0].clone());
    }

    fuzzy_select("Select your Doomsday deck", &available)
}

fn select_opponent_archetype(config: &PilegenConfig) -> Option<String> {
    // Only offer archetypes we have config for — names must match TOML keys
    let mut names: Vec<String> = config.archetypes.keys().cloned().collect();
    names.sort();

    if names.is_empty() {
        println!("No opponent archetypes configured in definitions/pilegen.toml.");
        return None;
    }

    fuzzy_select("Select opponent archetype", &names)
}

// ── Deck card loading ─────────────────────────────────────────────────────────

fn load_deck_cards(deck_name: &str) -> Vec<(String, i32, String)> {
    let conn = &mut establish_connection();

    let deck: Deck = decks::table
        .filter(decks::list_name.eq(deck_name))
        .first(conn)
        .expect("Deck not found");

    let deck_cards: Vec<Card> = cards::table
        .filter(cards::deck_id.eq(deck.deck_id))
        .load(conn)
        .unwrap_or_default();

    deck_cards
        .into_iter()
        .map(|c| (c.card_name, c.quantity, c.board))
        .collect()
}

// ── Scenario generation ───────────────────────────────────────────────────────

fn generate_scenario(
    deck_name: &str,
    opponent: &str,
    config: &PilegenConfig,
    all_cards: &[(String, i32, String)],
) -> GameState {
    let mut rng = rand::thread_rng();

    // Turn number is the primary consistency anchor — both players share it.
    let turn = gen_turn(&mut rng);
    let stage = stage_from_turn(turn);
    let life_total = gen_life_total(&mut rng);

    // Build catalog lookup once — used throughout generation
    let catalog_map: HashMap<&str, &CardDef> = config.cards.iter()
        .map(|c| (c.name.as_str(), c))
        .collect();

    // On the play: no draw on turn 1, so one fewer card seen.
    // Opponent always has the complement: if we're on the play they're on the draw, and vice versa.
    let on_play = rng.gen_bool(0.5);
    let our_mulls = gen_mulligans(&mut rng);
    let our_cards_seen = (7 - our_mulls as i32 + turn as i32 - if on_play { 1 } else { 0 }).max(0);
    let opp_mulls = gen_mulligans(&mut rng);
    // Opponent has the opposite play/draw position, so always T-1 draws vs T draws (or vice versa)
    let opp_cards_seen = (7 - opp_mulls as i32 + turn as i32 - if on_play { 0 } else { 1 }).max(0);

    // Land density from actual deck composition
    let total_main: i32 = all_cards.iter()
        .filter(|(_, _, b)| b == "main").map(|(_, q, _)| q).sum();
    let lands_in_deck: i32 = all_cards.iter()
        .filter(|(name, _, b)| b == "main"
            && catalog_map.get(name.as_str()).map(|d| d.card_type == "land").unwrap_or(false))
        .map(|(_, q, _)| q).sum();
    let land_density = if total_main > 0 { lands_in_deck as f64 / total_main as f64 } else { 0.25 };

    // Our permanents + entry effects (e.g. Bowmasters → Orc Army token)
    let mut my_permanents = gen_permanents(all_cards, &config.cards, &mut rng);
    let perm_names: Vec<String> = my_permanents.iter().map(|p| p.name.clone()).collect();
    let perm_name_refs: Vec<&str> = perm_names.iter().map(|s| s.as_str()).collect();
    apply_entry_tokens_to_permanents(&perm_name_refs, &catalog_map, &mut my_permanents, &mut rng);

    // Clue tokens: tied to whether the deck runs a clue-generating card
    let clue_tokens = if all_cards.iter().filter(|(_, _, b)| b == "main")
        .any(|(name, _, _)| catalog_map.get(name.as_str()).map(|d| d.generates_clues).unwrap_or(false))
    {
        gen_clue_tokens(&mut rng)
    } else {
        0
    };

    // Our lands — retry until at least one black-producing land present
    let mut lands_in_play = gen_lands(all_cards, &config.cards, stage, turn, our_cards_seen, land_density, &mut rng);
    loop {
        let black_indices: Vec<usize> = lands_in_play.iter().enumerate()
            .filter(|(_, (name, _))| {
                let base = name.find(" (").map(|i| &name[..i]).unwrap_or(name.as_str());
                catalog_map.get(base).map(|d| d.produces_black).unwrap_or(false)
            })
            .map(|(i, _)| i)
            .collect();

        if black_indices.is_empty() {
            lands_in_play = gen_lands(all_cards, &config.cards, stage, turn, our_cards_seen, land_density, &mut rng);
            continue;
        }

        let has_tapped_black = black_indices.iter().any(|&i| lands_in_play[i].1);
        if !has_tapped_black {
            let idx = black_indices[rng.gen_range(0..black_indices.len())];
            lands_in_play[idx].1 = true;
        }
        break;
    }

    let hand_pool = build_hand_pool(all_cards, &config.cards, &my_permanents, &lands_in_play);
    // Hand = total resources - lands - permanents - Doomsday - other non-cantrip setup spells.
    // Cantrips (Brainstorm/Ponder/Consider) net 0 cards so are excluded from the count.
    let our_noncantrip_spells: i32 = weighted_choice(&[(1i32, 2), (2, 5), (3, 3)], &mut rng);
    let our_hand_count = (our_cards_seen - lands_in_play.len() as i32
        - my_permanents.len() as i32 - 1 - our_noncantrip_spells).clamp(0, 7) as usize;
    let hand = sample_by_quantity(&hand_pool, our_hand_count, &mut rng);

    let exile = gen_exile(all_cards, &config.cards, &mut rng);

    let opp_cfg = config.archetypes.get(opponent);
    let lands_delta = opp_cfg.map(|c| c.lands_delta).unwrap_or(0);

    // opp_mulls and opp_cards_seen already computed above (play/draw complement)

    // Opponent land count: anchored to our land count (same turn = same draw count),
    // adjusted for archetype land density via lands_delta, with small jitter.
    // Opponent max lands = turn if we're on the draw (they've completed T turns),
    // or turn-1 if we're on the play (their Tth turn hasn't happened yet).
    let opp_max_lands = turn as i32 - if on_play { 1 } else { 0 };
    let land_jitter: i32 = weighted_choice(&[(-1i32, 2), (0, 6), (1, 2)], &mut rng);
    let opp_land_count = (lands_in_play.len() as i32 + lands_delta + land_jitter)
        .max(0).min(opp_max_lands) as usize;
    let cracked_prob = cracked_land_keep_prob(stage);
    let opponent_lands: Vec<(String, bool)> = opp_cfg
        .map(|cfg| {
            let pool: Vec<String> = cfg.lands.iter()
                .filter(|name| {
                    if catalog_map.get(name.as_str()).map(|d| d.cracked_land).unwrap_or(false) {
                        rng.gen_bool(cracked_prob)
                    } else {
                        true
                    }
                })
                .cloned()
                .collect();
            uniform_sample_n(&pool, opp_land_count, &mut rng)
                .into_iter()
                .map(|name| {
                    let is_fetch = catalog_map.get(name.as_str()).map(|d| d.is_fetch).unwrap_or(false);
                    let tapped = !is_fetch && rng.gen_bool(0.5);
                    (name, tapped)
                })
                .collect()
        })
        .unwrap_or_default();

    // Opponent permanents: sample by category, then apply catalog rules (annotation, entry tokens)
    let mut opponent_permanents = Vec::new();
    if let Some(cfg) = opp_cfg {
        let n = count_from_weights(&cfg.pressure_weights, &stage_pressure_defaults(stage), &mut rng);
        opponent_permanents.extend(uniform_sample_n(&cfg.pressure, n, &mut rng));
        let n = count_from_weights(&cfg.disruption_weights, &stage_disruption_defaults(stage), &mut rng);
        opponent_permanents.extend(uniform_sample_n(&cfg.disruption, n, &mut rng));
        let n = count_from_weights(&cfg.other_weights, &stage_other_defaults(stage), &mut rng);
        opponent_permanents.extend(uniform_sample_n(&cfg.other, n, &mut rng));
    }
    opponent_permanents = apply_catalog_rules_to_names(opponent_permanents, &catalog_map, &mut rng);

    // Opponent clue tokens — tied to catalog generates_clues flag
    let opp_all_perms = opp_cfg.map(|c| {
        c.pressure.iter().chain(c.disruption.iter()).chain(c.other.iter()).cloned().collect::<Vec<_>>()
    }).unwrap_or_default();
    let opponent_clue_tokens = if opp_all_perms.iter()
        .any(|name| catalog_map.get(name.as_str()).map(|d| d.generates_clues).unwrap_or(false))
    {
        gen_clue_tokens(&mut rng)
    } else {
        0
    };

    let opponent_used = opp_cfg.map(|cfg| {
        let n = count_from_weights(&cfg.used_weights, &stage_used_defaults(stage), &mut rng);
        uniform_sample_n(&cfg.used, n, &mut rng)
    }).unwrap_or_default();

    // Opponent hand = their total resources - lands - permanents - spells used.
    // `used` captures their non-cantrip spells; permanents count cards spent from hand.
    let total_opp_hand = (opp_cards_seen
        - opp_land_count as i32
        - opponent_permanents.len() as i32
        - opponent_used.len() as i32).clamp(0, 7);

    let known_hand_prob = opp_cfg.and_then(|c| c.known_hand_prob).unwrap_or(15);
    let opponent_known_hand = if rng.gen_ratio(known_hand_prob.min(100), 100) {
        opp_cfg.map(|c| uniform_sample_n(&c.hand, 1, &mut rng)).unwrap_or_default()
    } else {
        Vec::new()
    };
    let opponent_hand_unknown = (total_opp_hand - opponent_known_hand.len() as i32).max(0);

    GameState {
        my_deck_name: deck_name.to_string(),
        opponent_archetype: opponent.to_string(),
        turn,
        on_play,
        our_cards_seen,
        our_mulls,
        our_noncantrip_spells,
        opp_cards_seen,
        opp_mulls,
        life_total,
        lands_in_play,
        hand,
        my_permanents,
        clue_tokens,
        exile,
        opponent_hand_unknown,
        opponent_known_hand,
        opponent_lands,
        opponent_permanents,
        opponent_clue_tokens,
        opponent_used,
    }
}

// ── Card rule helpers ─────────────────────────────────────────────────────────

/// Stage-based probability that a cracked_land is still in play.
fn cracked_land_keep_prob(stage: GameStage) -> f64 {
    match stage {
        GameStage::Early => 0.10,
        GameStage::Mid   => 0.35,
        GameStage::Late  => 0.75,
    }
}

/// Apply entry_token effects to our permanent list (Bowmasters → Orc Army token etc.)
fn apply_entry_tokens_to_permanents(
    names: &[&str],
    catalog_map: &HashMap<&str, &CardDef>,
    permanents: &mut Vec<Permanent>,
    rng: &mut impl Rng,
) {
    let tokens: Vec<Permanent> = names.iter()
        .filter_map(|name| {
            let base = name.find(" (").map(|i| &name[..i]).unwrap_or(name);
            let def = catalog_map.get(base)?;
            let token_name = def.entry_token.as_ref()?;
            let prob = def.entry_token_prob.unwrap_or(62);
            if rng.gen_ratio(prob.min(100), 100) {
                Some(Permanent { name: token_name.clone(), kind: PermanentKind::Creature { tapped: false } })
            } else {
                None
            }
        })
        .collect();
    permanents.extend(tokens);
}

/// Apply catalog rules to opponent permanent names: annotation_options and entry_token.
fn apply_catalog_rules_to_names(
    names: Vec<String>,
    catalog_map: &HashMap<&str, &CardDef>,
    rng: &mut impl Rng,
) -> Vec<String> {
    let mut result = Vec::new();
    for name in names {
        let def = catalog_map.get(name.as_str());
        // Annotation (e.g. Murktide size)
        let display = match def {
            Some(d) if !d.annotation_options.is_empty() => {
                let ann = &d.annotation_options[rng.gen_range(0..d.annotation_options.len())];
                format!("{} ({})", name, ann)
            }
            _ => name,
        };
        result.push(display);
        // Entry token
        if let Some(d) = def {
            if let Some(token) = &d.entry_token {
                let prob = d.entry_token_prob.unwrap_or(62);
                if rng.gen_ratio(prob.min(100), 100) {
                    result.push(token.clone());
                }
            }
        }
    }
    result
}

// ── Distribution generators (spec-defined biases only) ───────────────────────

///// Pre-Doomsday life total. Post-DD life is floor(pre/2) — shown in display as "pre -> post".
/// Biased toward 14–20: usually near full, sometimes hurt by fetchlands/pain/opponent.
fn gen_life_total(rng: &mut impl Rng) -> i32 {
    let options: Vec<(i32, u32)> = (6..=20)
        .map(|life| {
            let w = match life {
                14..=20 => 5,
                10..=13 => 3,
                _ => 1,
            };
            (life, w)
        })
        .collect();
    weighted_choice(&options, rng)
}

/// Turn number when Doomsday is cast. Legacy Doomsday typically fires on turns 3–5.
fn gen_turn(rng: &mut impl Rng) -> u8 {
    weighted_choice(&[(2u8, 10), (3, 25), (4, 30), (5, 20), (6, 10), (7, 5)], rng)
}

/// Mulligans taken (affects starting hand size).
fn gen_mulligans(rng: &mut impl Rng) -> u8 {
    weighted_choice(&[(0u8, 55), (1, 35), (2, 10)], rng)
}

/// Land count from total cards seen and land density.
/// Not all drawn lands are immediately played (drop_rate ≈ 0.90).
fn gen_land_count_from_cards_seen(cards_seen: i32, land_density: f64, rng: &mut impl Rng) -> usize {
    let expected = (cards_seen as f64 * land_density * 0.90).round() as i32;
    let jitter = weighted_choice(&[(-1i32, 2), (0, 6), (1, 2)], rng);
    (expected + jitter).max(0) as usize
}

/// Stage-based default weights for opponent permanent categories (index = count, value = weight).
fn stage_pressure_defaults(stage: GameStage) -> Vec<(usize, u32)> {
    match stage {
        GameStage::Early => vec![(0, 90), (1, 10)],
        GameStage::Mid   => vec![(0, 80), (1, 15), (2, 5)],
        GameStage::Late  => vec![(0, 60), (1, 30), (2, 10)],
    }
}
fn stage_disruption_defaults(stage: GameStage) -> Vec<(usize, u32)> {
    match stage {
        GameStage::Early => vec![(0, 85), (1, 15)],
        GameStage::Mid   => vec![(0, 60), (1, 30), (2, 10)],
        GameStage::Late  => vec![(0, 45), (1, 40), (2, 15)],
    }
}
fn stage_other_defaults(stage: GameStage) -> Vec<(usize, u32)> {
    match stage {
        GameStage::Early => vec![(0, 90), (1, 10)],
        GameStage::Mid   => vec![(0, 80), (1, 20)],
        GameStage::Late  => vec![(0, 70), (1, 25), (2, 5)],
    }
}
fn stage_used_defaults(stage: GameStage) -> Vec<(usize, u32)> {
    match stage {
        GameStage::Early => vec![(0, 60), (1, 35), (2, 5)],
        GameStage::Mid   => vec![(0, 40), (1, 40), (2, 20)],
        GameStage::Late  => vec![(0, 20), (1, 35), (2, 30), (3, 15)],
    }
}

/// Non-land permanent count: 0–2.
fn gen_permanent_count(rng: &mut impl Rng) -> usize {
    weighted_choice(&[(0usize, 15), (1, 4), (2, 1)], rng)
}

// ── Weighted choice helpers ───────────────────────────────────────────────────

/// Pick a count from a `Vec<u32>` weight list (index = count, value = weight),
/// falling back to `defaults` if the list is empty.
fn count_from_weights(weights: &[u32], defaults: &[(usize, u32)], rng: &mut impl Rng) -> usize {
    if weights.is_empty() {
        weighted_choice(defaults, rng)
    } else {
        let pairs: Vec<(usize, u32)> = weights.iter().copied().enumerate().collect();
        weighted_choice(&pairs, rng)
    }
}

fn weighted_choice<T: Clone>(options: &[(T, u32)], rng: &mut impl Rng) -> T {
    let total: u32 = options.iter().map(|(_, w)| w).sum();
    assert!(total > 0);
    let mut pick = rng.gen_range(0..total);
    for (val, weight) in options {
        if pick < *weight {
            return val.clone();
        }
        pick -= weight;
    }
    options.last().unwrap().0.clone()
}

// ── Uniform sampling helpers ──────────────────────────────────────────────────

/// Shuffle `pool` and return the first `n` items (or fewer if pool is smaller).
fn uniform_sample_n<T: Clone>(pool: &[T], n: usize, rng: &mut impl Rng) -> Vec<T> {
    if pool.is_empty() || n == 0 {
        return Vec::new();
    }
    let mut keyed: Vec<(u32, usize)> = (0..pool.len()).map(|i| (rng.gen(), i)).collect();
    keyed.sort_unstable_by_key(|(k, _)| *k);
    keyed[..n.min(pool.len())].iter().map(|(_, i)| pool[*i].clone()).collect()
}


/// Sample up to `n` unique card names from a `(name, quantity)` pool,
/// where quantity determines relative likelihood (cards appear `qty` times).
fn sample_by_quantity(pool: &[(String, u32)], n: usize, rng: &mut impl Rng) -> Vec<String> {
    if pool.is_empty() || n == 0 {
        return Vec::new();
    }
    // Expand by quantity, shuffle, take first n unique names
    let mut expanded: Vec<&str> = pool
        .iter()
        .flat_map(|(name, qty)| std::iter::repeat(name.as_str()).take(*qty as usize))
        .collect();
    let len = expanded.len();
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for i in 0..len {
        if result.len() >= n {
            break;
        }
        let j = rng.gen_range(i..len);
        expanded.swap(i, j);
        let name = expanded[i];
        if seen.insert(name) {
            result.push(name.to_string());
        }
    }
    result
}

// ── Per-component generators ──────────────────────────────────────────────────

fn gen_permanents(
    all_cards: &[(String, i32, String)],
    catalog: &[CardDef],
    rng: &mut impl Rng,
) -> Vec<Permanent> {
    let mainboard: HashSet<&str> = all_cards
        .iter()
        .filter(|(_, _, b)| b == "main")
        .map(|(n, _, _)| n.as_str())
        .collect();

    // Permanent candidates: catalog entries that are non-land permanents and are in the deck,
    // filtered by play_weight (default 100; weight=1 means ~1% chance of being in the pool).
    let available: Vec<&CardDef> = catalog
        .iter()
        .filter(|c| matches!(c.card_type.as_str(), "creature" | "planeswalker" | "artifact"))
        .filter(|c| mainboard.contains(c.name.as_str()))
        .filter(|c| {
            let w = c.play_weight.unwrap_or(100);
            w >= 100 || rng.gen_ratio(w, 100)
        })
        .collect();

    let count = gen_permanent_count(rng).min(available.len());
    if count == 0 {
        return Vec::new();
    }

    // Shuffle uniformly; avoid two faces of the same physical card
    let mut keyed: Vec<(u32, usize)> = (0..available.len()).map(|i| (rng.gen(), i)).collect();
    keyed.sort_unstable_by_key(|(k, _)| *k);

    let mut result = Vec::new();
    let mut used: HashSet<&str> = HashSet::new();

    for (_, idx) in keyed {
        if result.len() >= count { break; }
        let c = available[idx];
        if !used.insert(c.name.as_str()) { continue; }

        let (base_name, kind) = if c.flipped_name.is_some() {
            // DFC: decide flip state, then choose face
            if rng.gen_bool(0.5) {
                let ft = c.flipped_card_type.as_deref().unwrap_or("creature");
                let kind = make_kind(ft, c.flipped_loyalty.or(c.loyalty), rng);
                (c.flipped_name.as_ref().unwrap().clone(), kind)
            } else {
                (c.name.clone(), make_kind(&c.card_type, c.loyalty, rng))
            }
        } else {
            (c.name.clone(), make_kind(&c.card_type, c.loyalty, rng))
        };

        // Apply annotation (e.g. size for Murktide Regent)
        let display_name = if !c.annotation_options.is_empty() {
            let annotation = &c.annotation_options[rng.gen_range(0..c.annotation_options.len())];
            format!("{} ({})", base_name, annotation)
        } else {
            base_name
        };

        result.push(Permanent { name: display_name, kind });
    }
    result
}

fn make_kind(card_type: &str, loyalty: Option<i32>, rng: &mut impl Rng) -> PermanentKind {
    match card_type {
        "planeswalker" => PermanentKind::Planeswalker {
            loyalty: loyalty.unwrap_or(4),
            activated: rng.gen_bool(0.5),
        },
        "artifact" => PermanentKind::Artifact,
        _ => PermanentKind::Creature { tapped: rng.gen_bool(0.5) },
    }
}

fn gen_clue_tokens(rng: &mut impl Rng) -> i32 {
    weighted_choice(&[(0i32, 90), (1, 9), (2, 1)], rng)
}

fn gen_lands(
    all_cards: &[(String, i32, String)],
    catalog: &[CardDef],
    stage: GameStage,
    turn: u8,
    cards_seen: i32,
    land_density: f64,
    rng: &mut impl Rng,
) -> Vec<(String, bool)> {
    // Build lookup: land name → CardDef
    let land_defs: HashMap<&str, &CardDef> = catalog
        .iter()
        .filter(|c| c.card_type == "land")
        .map(|c| (c.name.as_str(), c))
        .collect();

    let cracked_keep_prob = cracked_land_keep_prob(stage);

    // Pool of land names from deck, expanded by quantity.
    // Cards with low play_weight are probabilistically excluded from the pool.
    // Cracked lands (Wasteland etc.) are stage-filtered via catalog flag.
    let pool: Vec<&str> = all_cards
        .iter()
        .filter(|(_, _, b)| b == "main")
        .filter(|(name, _, _)| {
            if let Some(def) = land_defs.get(name.as_str()) {
                if def.cracked_land {
                    return rng.gen_bool(cracked_keep_prob);
                }
                let w = def.play_weight.unwrap_or(100);
                w >= 100 || rng.gen_ratio(w, 100)
            } else {
                false
            }
        })
        .flat_map(|(name, qty, _)| std::iter::repeat(name.as_str()).take(*qty as usize))
        .collect();

    if pool.is_empty() { return Vec::new(); }

    // Can't have more lands than turns played (one land drop per turn max)
    let count = gen_land_count_from_cards_seen(cards_seen, land_density, rng)
        .min(turn as usize)
        .min(pool.len());

    let mut keyed: Vec<(u32, usize)> = (0..pool.len()).map(|i| (rng.gen(), i)).collect();
    keyed.sort_unstable_by_key(|(k, _)| *k);

    keyed[..count]
        .iter()
        .map(|(_, i)| {
            let base_name = pool[*i];
            let def = land_defs[base_name];

            let display_name = if !def.annotation_options.is_empty() {
                let annotation = &def.annotation_options[rng.gen_range(0..def.annotation_options.len())];
                format!("{} ({})", base_name, annotation)
            } else {
                base_name.to_string()
            };

            // Fetchlands are always untapped (cracking removes them from play)
            let tapped = !def.is_fetch && rng.gen_bool(0.5);

            (display_name, tapped)
        })
        .collect()
}

fn build_hand_pool(
    all_cards: &[(String, i32, String)],
    catalog: &[CardDef],
    in_play: &[Permanent],
    lands_in_play: &[(String, bool)],
) -> Vec<(String, u32)> {
    // Reverse map: flipped display name → front-face deck name (for DFCs like Tamiyo)
    let flipped_to_front: HashMap<&str, &str> = catalog
        .iter()
        .filter_map(|c| c.flipped_name.as_ref().map(|f| (f.as_str(), c.name.as_str())))
        .collect();

    // Count copies already accounted for per deck-card name
    let mut deducted: HashMap<String, i32> = HashMap::new();

    // One Doomsday was cast to create the pile
    *deducted.entry("Doomsday".to_string()).or_insert(0) += 1;

    // Non-land permanents in play — resolve DFC display names back to front-face name
    for p in in_play {
        let canonical = flipped_to_front.get(p.name.as_str()).copied().unwrap_or(&p.name);
        *deducted.entry(canonical.to_string()).or_insert(0) += 1;
    }

    // Lands in play — strip annotation e.g. "Cavern of Souls (Wizard)" → "Cavern of Souls"
    for (name, _) in lands_in_play {
        let base = name.find(" (").map(|i| &name[..i]).unwrap_or(name.as_str());
        *deducted.entry(base.to_string()).or_insert(0) += 1;
    }

    all_cards
        .iter()
        .filter(|(_, _, board)| board == "main")
        .filter_map(|(name, qty, _)| {
            let used = deducted.get(name.as_str()).copied().unwrap_or(0);
            let remaining = (*qty - used).max(0) as u32;
            if remaining > 0 { Some((name.clone(), remaining)) } else { None }
        })
        .collect()
}

fn gen_exile(
    all_cards: &[(String, i32, String)],
    catalog: &[CardDef],
    rng: &mut impl Rng,
) -> Vec<String> {
    let count = weighted_choice(&[(0usize, 6), (1, 3), (2, 1)], rng);
    if count == 0 { return Vec::new(); }

    let mainboard: HashMap<&str, i32> = all_cards
        .iter()
        .filter(|(_, _, b)| b == "main")
        .map(|(n, q, _)| (n.as_str(), *q))
        .collect();

    let has_jace = mainboard.contains_key("Jace, Wielder of Mysteries");

    let candidates: Vec<String> = catalog
        .iter()
        .filter(|c| c.exileable && mainboard.contains_key(c.name.as_str()))
        .filter(|c| {
            // Never exile the only Oracle when there's no Jace to replace it
            if c.name == "Thassa's Oracle" && !has_jace {
                return mainboard.get("Thassa's Oracle").copied().unwrap_or(0) > 1;
            }
            true
        })
        .map(|c| c.name.clone())
        .collect();

    uniform_sample_n(&candidates, count, rng)
}

// ── Display ───────────────────────────────────────────────────────────────────

fn sec(label: &str) -> String {
    let total = 50usize;
    let label_with_spaces = format!(" {} ", label);
    let label_char_len = label_with_spaces.chars().count();
    let padding = total.saturating_sub(label_char_len + 2);
    format!("  ──{}{}", label_with_spaces, "─".repeat(padding))
}

fn display_scenario(state: &GameState) {
    let dbar = "═".repeat(50);

    println!();
    println!("  ╔{}╗", dbar);
    println!("  ║{:^50}║", " DOOMSDAY PILE SCENARIO ");
    println!("  ╚{}╝", dbar);
    println!();
    println!("  Deck    : {}", state.my_deck_name);
    println!("  Opponent: {}", state.opponent_archetype);
    println!("  Turn    : {} ({}, {})", state.turn, stage_label(state.turn),
        if state.on_play { "on the play" } else { "on the draw" });
    println!();

    // ── Resource accounting ──
    let our_lands = state.lands_in_play.len() as i32;
    // Tokens are not cards from hand, exclude them from accounting
    let our_perms = state.my_permanents.iter()
        .filter(|p| !p.name.contains("token") && !p.name.contains("Token"))
        .count() as i32;
    let our_hand = state.hand.len() as i32;
    let our_accounted = our_lands + our_perms + 1 + state.our_noncantrip_spells + our_hand;
    println!("  [debug] Us  : {} seen ({} mulls) = {} lands + {} perms + 1 DD + {} spells + {} hand = {}{}",
        state.our_cards_seen, state.our_mulls,
        our_lands, our_perms, state.our_noncantrip_spells, our_hand,
        our_accounted,
        if our_accounted == state.our_cards_seen { "" } else { " ⚠ MISMATCH" });
    let opp_lands = state.opponent_lands.len() as i32;
    let opp_perms = state.opponent_permanents.iter()
        .filter(|p| !p.contains("token") && !p.contains("Token"))
        .count() as i32;
    let opp_used = state.opponent_used.len() as i32;
    let opp_hand_total = state.opponent_hand_unknown + state.opponent_known_hand.len() as i32;
    let opp_accounted = opp_lands + opp_perms + opp_used + opp_hand_total;
    println!("  [debug] Opp : {} seen ({} mulls) = {} lands + {} perms + {} used + {} hand = {}{}",
        state.opp_cards_seen, state.opp_mulls,
        opp_lands, opp_perms, opp_used, opp_hand_total,
        opp_accounted,
        if opp_accounted == state.opp_cards_seen { "" } else { " ⚠ MISMATCH" });
    println!();

    println!("{}", sec("MY BOARD"));
    println!();
    println!("  Life       : {} -> {}", state.life_total, state.life_total / 2);

    if !state.lands_in_play.is_empty() {
        println!("  Lands      :");
        for (land, tapped) in &state.lands_in_play {
            println!("    * {}{}", land, if *tapped { " (tapped)" } else { "" });
        }
    }

    if !state.my_permanents.is_empty() {
        println!("  Permanents :");
        for p in &state.my_permanents {
            let annotation = match &p.kind {
                PermanentKind::Creature { tapped } => {
                    if *tapped { " (tapped)".to_string() } else { " (untapped)".to_string() }
                }
                PermanentKind::Planeswalker { loyalty, activated } => {
                    if *activated {
                        format!(" ({} loyalty, activated this turn)", loyalty)
                    } else {
                        format!(" ({} loyalty)", loyalty)
                    }
                }
                PermanentKind::Artifact => String::new(),
            };
            println!("    * {}{}", p.name, annotation);
        }
    }

    if state.clue_tokens > 0 {
        println!(
            "  Clue token{}: {}",
            if state.clue_tokens == 1 { " " } else { "s" },
            state.clue_tokens
        );
    }

    println!();

    println!(
        "{}",
        sec(&format!(
            "HAND ({} card{})",
            state.hand.len(),
            if state.hand.len() == 1 { "" } else { "s" }
        ))
    );
    println!();
    if state.hand.is_empty() {
        println!("  (empty)");
    } else {
        for card in &state.hand {
            println!("    * {}", card);
        }
    }

    if !state.exile.is_empty() {
        println!();
        println!("{}", sec("EXILE"));
        println!();
        for card in &state.exile {
            println!("    * {}", card);
        }
    }

    println!();

    println!("{}", sec(&format!("OPPONENT: {}", state.opponent_archetype)));
    println!();

    if state.opponent_hand_unknown > 0 {
        println!(
            "  Hand: {} unknown card{}",
            state.opponent_hand_unknown,
            if state.opponent_hand_unknown == 1 { "" } else { "s" },
        );
    }

    if !state.opponent_known_hand.is_empty() {
        println!("  Known in hand:");
        for card in &state.opponent_known_hand {
            println!("    * {}", card);
        }
    }

    if state.opponent_hand_unknown == 0 && state.opponent_known_hand.is_empty() {
        println!("  Hand: empty");
    }

    if !state.opponent_lands.is_empty() {
        println!("  Lands:");
        for (land, tapped) in &state.opponent_lands {
            println!("    * {}{}", land, if *tapped { " (tapped)" } else { "" });
        }
    }

    if !state.opponent_permanents.is_empty() {
        println!("  Board:");
        for card in &state.opponent_permanents {
            println!("    * {}", card);
        }
    }

    if state.opponent_clue_tokens > 0 {
        println!(
            "  Clue token{}: {}",
            if state.opponent_clue_tokens == 1 { " " } else { "s" },
            state.opponent_clue_tokens
        );
    }

    if !state.opponent_used.is_empty() {
        println!("  Previously used:");
        for card in &state.opponent_used {
            println!("    * {}", card);
        }
    }

    println!();
    println!("  {}", dbar);
}
