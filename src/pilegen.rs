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
}

#[derive(Deserialize, Default)]
struct ArchetypeConfig {
    #[serde(default)]
    hand_threats: Vec<String>,
    #[serde(default)]
    perm_threats: Vec<String>,
    #[serde(default)]
    used_threats: Vec<String>,
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
    life_total: i32,
    lands_in_play: Vec<(String, bool)>, // (display_name, is_tapped)
    hand: Vec<String>,
    my_permanents: Vec<Permanent>,
    clue_tokens: i32,
    exile: Vec<String>,
    opponent_hand_unknown: i32,
    opponent_known_hand: Vec<String>,
    opponent_permanents: Vec<String>,
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

    let opponent = match select_opponent_archetype() {
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

fn select_opponent_archetype() -> Option<String> {
    let conn = &mut establish_connection();

    let all_types: Vec<DeckType> = deck_types::table
        .order((deck_types::archetype.asc(), deck_types::subtype.asc()))
        .load(conn)
        .unwrap_or_default();

    if all_types.is_empty() {
        println!("No opponent deck types found in database.");
        return None;
    }

    let displays: Vec<String> = all_types.iter().map(|dt| dt.display()).collect();
    fuzzy_select("Select opponent archetype", &displays)
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

    let life_total = gen_life_total(&mut rng);
    let my_permanents = gen_permanents(all_cards, &config.cards, &mut rng);
    let clue_tokens = gen_clue_tokens(&mut rng);

    // Generate lands, retrying until at least one black-producing land is present
    // (Doomsday costs BBB — the state must be consistent with having cast it).
    let land_catalog: HashMap<&str, &CardDef> = config.cards.iter()
        .filter(|c| c.card_type == "land")
        .map(|c| (c.name.as_str(), c))
        .collect();

    let mut lands_in_play = gen_lands(all_cards, &config.cards, &mut rng);
    loop {
        let black_indices: Vec<usize> = lands_in_play.iter().enumerate()
            .filter(|(_, (name, _))| {
                let base = name.find(" (").map(|i| &name[..i]).unwrap_or(name.as_str());
                land_catalog.get(base).map(|d| d.produces_black).unwrap_or(false)
            })
            .map(|(i, _)| i)
            .collect();

        if black_indices.is_empty() {
            // No black mana sources — retry
            lands_in_play = gen_lands(all_cards, &config.cards, &mut rng);
            continue;
        }

        // Ensure at least one black land is tapped (mana was spent on Doomsday)
        let has_tapped_black = black_indices.iter().any(|&i| lands_in_play[i].1);
        if !has_tapped_black {
            let idx = black_indices[rng.gen_range(0..black_indices.len())];
            lands_in_play[idx].1 = true;
        }

        break;
    }

    let hand_pool = build_hand_pool(all_cards, &config.cards, &my_permanents, &lands_in_play);
    let hand_count = gen_hand_count(&mut rng);
    let hand = sample_by_quantity(&hand_pool, hand_count, &mut rng);

    let exile = gen_exile(all_cards, &config.cards, &mut rng);

    let total_opp_hand = gen_opponent_hand_count(hand.len() as i32, &mut rng);
    let opp_cfg = config.archetypes.get(opponent);

    let opponent_known_hand = opp_cfg
        .filter(|_| rng.gen_bool(0.25))
        .map(|c| uniform_sample_n(&c.hand_threats, 1, &mut rng))
        .unwrap_or_default();
    let opponent_hand_unknown =
        (total_opp_hand - opponent_known_hand.len() as i32).max(0);

    let opponent_permanents = opp_cfg
        .map(|c| uniform_sample_up_to(&c.perm_threats, 2, &mut rng))
        .unwrap_or_default();
    let opponent_used = opp_cfg
        .map(|c| uniform_sample_up_to(&c.used_threats, 2, &mut rng))
        .unwrap_or_default();

    GameState {
        my_deck_name: deck_name.to_string(),
        opponent_archetype: opponent.to_string(),
        life_total,
        lands_in_play,
        hand,
        my_permanents,
        clue_tokens,
        exile,
        opponent_hand_unknown,
        opponent_known_hand,
        opponent_permanents,
        opponent_used,
    }
}

// ── Distribution generators (spec-defined biases only) ───────────────────────

/// Life total: spec says bias towards 10-18, min 2.
fn gen_life_total(rng: &mut impl Rng) -> i32 {
    let options: Vec<(i32, u32)> = (2..=20)
        .map(|life| {
            let w = match life {
                10..=18 => 4,
                7..=9 | 19..=20 => 2,
                _ => 1,
            };
            (life, w)
        })
        .collect();
    weighted_choice(&options, rng)
}

/// Hand count: spec says bias towards 1-4.
fn gen_hand_count(rng: &mut impl Rng) -> usize {
    weighted_choice(&[(0, 1), (1, 3), (2, 4), (3, 4), (4, 3), (5, 2), (6, 1)], rng)
}

/// Opponent hand count: spec says opponent tends to have ~1 more card than me.
fn gen_opponent_hand_count(my_hand: i32, rng: &mut impl Rng) -> i32 {
    let base = my_hand + 1;
    let delta: i32 = weighted_choice(&[(-1, 2), (0, 4), (1, 3), (2, 1)], rng);
    (base + delta).clamp(0, 7)
}

/// Land count: 0–5, usually 1–3.
fn gen_land_count(rng: &mut impl Rng) -> usize {
    weighted_choice(&[(0usize, 1), (1, 3), (2, 4), (3, 4), (4, 2), (5, 1)], rng)
}

/// Non-land permanent count: 0–2.
fn gen_permanent_count(rng: &mut impl Rng) -> usize {
    weighted_choice(&[(0usize, 15), (1, 4), (2, 1)], rng)
}

// ── Weighted choice helper ────────────────────────────────────────────────────

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

/// Return 0..=max items chosen uniformly at random from pool.
fn uniform_sample_up_to<T: Clone>(pool: &[T], max: usize, rng: &mut impl Rng) -> Vec<T> {
    if pool.is_empty() || max == 0 {
        return Vec::new();
    }
    let n = rng.gen_range(0..=max.min(pool.len()));
    uniform_sample_n(pool, n, rng)
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

        let (display_name, kind) = if c.flipped_name.is_some() {
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
    rng: &mut impl Rng,
) -> Vec<(String, bool)> {
    // Build lookup: land name → CardDef
    let land_defs: HashMap<&str, &CardDef> = catalog
        .iter()
        .filter(|c| c.card_type == "land")
        .map(|c| (c.name.as_str(), c))
        .collect();

    // Pool of land names from deck, expanded by quantity.
    // Cards with low play_weight are probabilistically excluded from the pool.
    let pool: Vec<&str> = all_cards
        .iter()
        .filter(|(_, _, b)| b == "main")
        .filter(|(name, _, _)| {
            if let Some(def) = land_defs.get(name.as_str()) {
                let w = def.play_weight.unwrap_or(100);
                w >= 100 || rng.gen_ratio(w, 100)
            } else {
                false
            }
        })
        .flat_map(|(name, qty, _)| std::iter::repeat(name.as_str()).take(*qty as usize))
        .collect();

    if pool.is_empty() { return Vec::new(); }

    let count = gen_land_count(rng).min(pool.len());

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
    println!();

    println!("{}", sec("MY BOARD"));
    println!();
    println!("  Life total : {}", state.life_total);

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

    if !state.opponent_permanents.is_empty() {
        println!("  Board:");
        for card in &state.opponent_permanents {
            println!("    * {}", card);
        }
    }

    if !state.opponent_used.is_empty() {
        println!("  Previously used:");
        for card in &state.opponent_used {
            println!("    * {}", card);
        }
    }

    println!();
    println!("  {}", dbar);
    println!("  Your pile is in your library (5 cards).");
    println!("  {}", dbar);
}
