//! `mtgctl mtgo scan`: parse every game log on disk and report.

use super::event::{template, Event};
use super::format::{read_log, LogKind};
use super::model::{build_match, GameOutcome, MatchLog, MatchResult, Verb, WinBy};
use super::MtgoConfig;
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, Table};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

/// Recursively find `Match_GameLog_*.dat` under `root` (bounded depth).
pub fn find_game_logs(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 10 {
            return;
        }
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, depth + 1, out);
            } else if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("Match_GameLog_") && n.ends_with(".dat")) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out.sort();
    out
}

pub struct Loaded {
    pub path: PathBuf,
    pub m: MatchLog,
}

/// Load every game log under `root`. The MTGO ClickOnce layout can hold several `Data/...`
/// generations with byte-identical copies of the same logs, so files are de-duplicated by
/// name (= match guid), keeping the most recently modified copy.
pub fn load_all(root: &Path) -> (Vec<Loaded>, Vec<(PathBuf, String)>) {
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    let mut by_name: BTreeMap<String, PathBuf> = BTreeMap::new();
    for p in find_game_logs(root) {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let newer = |a: &Path, b: &Path| {
            let m = |x: &Path| fs::metadata(x).and_then(|m| m.modified()).ok();
            m(a) >= m(b)
        };
        match by_name.get(&name) {
            Some(prev) if newer(prev, &p) => {}
            _ => {
                by_name.insert(name, p);
            }
        }
    }
    for p in by_name.into_values() {
        match read_log(&p) {
            Ok(raw) if raw.kind == LogKind::GameLog => ok.push(Loaded { m: build_match(&raw), path: p }),
            Ok(raw) => bad.push((p, format!("unexpected kind {:?}", raw.kind))),
            Err(e) => bad.push((p, e.to_string())),
        }
    }
    ok.sort_by_key(|l| l.m.start);
    (ok, bad)
}

/// The player present in (nearly) every match is "me".
pub fn guess_me(loaded: &[Loaded]) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for l in loaded {
        for p in &l.m.players {
            *counts.entry(p).or_default() += 1;
        }
    }
    counts.into_iter().max_by_key(|(_, c)| *c).map(|(p, _)| p.to_string())
}

pub fn run_scan(cfg: &MtgoConfig, root: &Path, show_matches: bool, unparsed: Option<usize>, dump: Option<&str>, last: Option<usize>, include_replays: bool) {
    let (mut loaded, bad) = load_all(root);
    let n_replays = loaded.iter().filter(|l| l.m.replay).count();
    if !include_replays {
        loaded.retain(|l| !l.m.replay);
    }
    if loaded.is_empty() {
        eprintln!("No Match_GameLog_*.dat files found under {}", root.display());
        for (p, e) in bad.iter().take(5) {
            eprintln!("  {}: {e}", p.display());
        }
        return;
    }
    let me = cfg.username.clone().or_else(|| guess_me(&loaded)).unwrap_or_default();

    if let Some(prefix) = dump {
        for l in loaded.iter().filter(|l| l.m.guid.starts_with(prefix)) {
            dump_match(l, &me);
        }
        return;
    }

    if let Some(n) = last {
        let len = loaded.len();
        if len > n {
            loaded.drain(0..len - n);
        }
    }

    let n_games: usize = loaded.iter().map(|l| l.m.games.len()).sum();
    let n_records: usize = loaded.iter().map(|l| l.m.record_count).sum();
    let first = loaded.first().and_then(|l| l.m.start);
    let lastts = loaded.last().and_then(|l| l.m.end);
    let (mut mw, mut ml, mut mt, mut mu) = (0, 0, 0, 0);
    let (mut gw, mut gl, mut gu) = (0, 0, 0);
    let mut by_end: BTreeMap<String, usize> = BTreeMap::new();
    for l in &loaded {
        match &l.m.result {
            MatchResult::Won { winner, .. } if *winner == me => mw += 1,
            MatchResult::Won { .. } => ml += 1,
            MatchResult::Tied { .. } => mt += 1,
            MatchResult::Unknown => mu += 1,
        }
        for g in &l.m.games {
            match &g.outcome {
                GameOutcome::Won { winner, by } => {
                    if *winner == me { gw += 1 } else { gl += 1 }
                    let k = match by {
                        WinBy::Concession => "concession",
                        WinBy::OpponentLeft(_) => "opponent left",
                        WinBy::LossLine => "loses-the-game line",
                        WinBy::Normal => "normal",
                    };
                    *by_end.entry(k.to_string()).or_default() += 1;
                }
                GameOutcome::Unfinished => gu += 1,
            }
        }
    }

    println!("MTGO game logs under {}", root.display());
    println!("  files: {} matches parsed, {} failed, {} replays {}; records: {}", loaded.len(), bad.len(), n_replays, if include_replays { "included" } else { "skipped" }, n_records);
    if let (Some(a), Some(b)) = (first, lastts) {
        println!("  span: {} .. {}", a.format("%Y-%m-%d %H:%M"), b.format("%Y-%m-%d %H:%M"));
    }
    println!("  me: {me}{}", if cfg.username.is_none() { " (auto-detected; set [mtgo].username in config.toml)" } else { "" });
    println!("  matches: {mw}W {ml}L {mt}T {mu}? (of {})", loaded.len());
    println!("  games: {gw}W {gl}L {gu} unfinished (of {n_games}); game-end kinds: {:?}", by_end);
    for (p, e) in &bad {
        eprintln!("  FAILED {}: {e}", p.display());
    }

    if show_matches {
        let mut t = Table::new();
        t.load_preset(UTF8_FULL_CONDENSED);
        t.set_header(["date", "opponent", "result", "games", "my mulls", "opp mulls", "turns", "min", "guid", ""]);
        for l in &loaded {
            let m = &l.m;
            let opp = m.opponent_of(&me).unwrap_or("?");
            let (w, lo) = m.game_score(&me);
            let res = match &m.result {
                MatchResult::Won { winner, .. } if *winner == me => "W".to_string(),
                MatchResult::Won { .. } => "L".to_string(),
                MatchResult::Tied { .. } => "T".to_string(),
                MatchResult::Unknown => "?".to_string(),
            };
            let mulls = |who: &str| m.games.iter().map(|g| g.per_player.get(who).map_or(0, |p| p.mulligans).to_string()).collect::<Vec<_>>().join("/");
            let turns = m.games.iter().map(|g| g.turns.to_string()).collect::<Vec<_>>().join("/");
            let mins = match (m.start, m.end) {
                (Some(a), Some(b)) => ((b - a).num_seconds() / 60).to_string(),
                _ => "".into(),
            };
            t.add_row(vec![
                Cell::new(m.start.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()),
                Cell::new(opp),
                Cell::new(format!("{res} {w}-{lo}")),
                Cell::new(m.games.len()),
                Cell::new(mulls(&me)),
                Cell::new(mulls(opp)),
                Cell::new(turns),
                Cell::new(mins),
                Cell::new(m.guid.chars().take(8).collect::<String>()),
                Cell::new(if m.replay { "replay" } else { "" }),
            ]);
        }
        println!("{t}");
    }

    if let Some(n) = unparsed {
        let mut counts: HashMap<String, (usize, String)> = HashMap::new();
        for l in &loaded {
            let players = l.m.players.clone();
            for ev in l.m.games.iter().flat_map(|g| g.events.iter()).chain(l.m.extra.iter()) {
                if let Event::Other(_) | Event::System(_) = ev.event {
                    let k = template(&ev.raw, &players);
                    let e = counts.entry(k).or_insert_with(|| (0, ev.raw.clone()));
                    e.0 += 1;
                }
            }
        }
        let total: usize = counts.values().map(|v| v.0).sum();
        let mut v: Vec<_> = counts.into_iter().collect();
        v.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
        println!("\nUnparsed lines: {total} in {} templates; top {n}:", v.len());
        for (k, (c, _)) in v.into_iter().take(n) {
            let k: String = k.chars().take(150).collect();
            println!("{c:7}  {k}");
        }
    }
}

fn dump_match(l: &Loaded, me: &str) {
    let m = &l.m;
    println!("== {} ({})", m.guid, l.path.display());
    println!("players: {:?}  result: {:?}", m.players, m.result);
    for g in &m.games {
        println!(
            "-- game {} {}..{} ({}s) turns={} first={:?} outcome={:?}",
            g.number,
            g.start.format("%H:%M:%S"),
            g.end.format("%H:%M:%S"),
            g.duration_secs(),
            g.turns,
            g.first_player,
            g.outcome
        );
        for (p, pg) in &g.per_player {
            let tag = if p == me { "me " } else { "opp" };
            let played: Vec<&str> = g.card_events.iter().filter(|e| e.owner == *p && matches!(e.verb, Verb::Cast | Verb::Land)).map(|e| e.card.name.as_str()).collect();
            println!("   {tag} {p}: mull={} hand={:?} played=[{}]", pg.mulligans, pg.hand_size, played.join(", "));
        }
        for ev in &g.events {
            println!("   {} {:?}", ev.ts.format("%H:%M:%S"), ev.event);
        }
    }
    if !m.extra.is_empty() {
        println!("-- extra:");
        for ev in &m.extra {
            println!("   {} {:?}", ev.ts.format("%H:%M:%S"), ev.event);
        }
    }
}
