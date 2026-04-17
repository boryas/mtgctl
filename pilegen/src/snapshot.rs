//! Compact binary board-state snapshot for URL-shareable pile scenarios.
//!
//! Cards are identified as `(set_index: u16, collector_number: u16)` — 4 bytes
//! per card, mapping to real Magic set + collector number pairs via a
//! [`CardRegistry`].
//!
//! # Wire format (version 1)
//!
//! ```text
//! HEADER (10 bytes):
//!   [0]     version (u8) = 1
//!   [1]     turn (u8)
//!   [2]     stage (u8: 0=Early, 1=Mid, 2=Late)
//!   [3]     flags: bit0=on_play  bit1=us_land_drop  bit2=opp_land_drop
//!   [4‑5]   us_life   (i16 LE)
//!   [6‑7]   opp_life  (i16 LE)
//!   [8‑9]   life_before_dd (i16 LE; i16::MIN = None)
//!
//! STACK (shared zone):
//!   [1]     count (u8)
//!   Per entry — CARD (5 bytes):
//!       [2] set_index  (u16 LE)
//!       [2] collector  (u16 LE)
//!       [1] flags: bit0=pile_selected
//!
//! PER PLAYER (us first, then opp):
//!   [1]     deck_name_len (u8)
//!   [N]     deck_name     (UTF-8)
//!
//!   2× PERMANENT ZONE  (lands, permanents) — 7 bytes each:
//!       [2] set_index  (u16 LE)
//!       [2] collector  (u16 LE)
//!       [1] flags: bit0=tapped  bit1=flipped  bit2=pile_selected
//!       [1] counters   (u8)
//!       [1] loyalty    (u8)
//!
//!   4× CARD ZONE  (hand, library, graveyard, exile) — 5 bytes each:
//!       [2] set_index  (u16 LE)
//!       [2] collector  (u16 LE)
//!       [1] flags: bit0=pile_selected  bit1=known
//!
//!   [1]     hand_hidden (u8)
//! ```

use std::collections::HashMap;
use std::fmt;

// ── Card identity ────────────────────────────────────────────────────────────

/// A card identified by set + collector number.  Wire: two LE u16s (4 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CardId {
    pub set_index: u16,
    pub collector_number: u16,
}

impl CardId {
    pub const fn new(set_index: u16, collector_number: u16) -> Self {
        Self { set_index, collector_number }
    }
}

// ── Snapshot types ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct BoardSnapshot {
    pub turn: u8,
    pub stage: Stage,
    pub on_play: bool,
    pub life_before_dd: Option<i16>,
    pub stack: Vec<CardEntry>,
    pub us: PlayerSnapshot,
    pub opp: PlayerSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Stage {
    Early = 0,
    Mid = 1,
    Late = 2,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerSnapshot {
    pub deck_name: String,
    pub life: i16,
    pub land_drop_available: bool,
    pub lands: Vec<PermanentEntry>,
    pub permanents: Vec<PermanentEntry>,
    pub hand: Vec<CardEntry>,
    pub library: Vec<CardEntry>,
    pub graveyard: Vec<CardEntry>,
    pub exile: Vec<CardEntry>,
    pub hand_hidden: u8,
}

/// A card on the battlefield — carries tap / counter / loyalty state.
#[derive(Clone, Debug, PartialEq)]
pub struct PermanentEntry {
    pub id: CardId,
    pub tapped: bool,
    pub flipped: bool,
    pub pile_selected: bool,
    pub counters: u8,
    pub loyalty: u8,
}

/// A card in any non-battlefield zone.
#[derive(Clone, Debug, PartialEq)]
pub struct CardEntry {
    pub id: CardId,
    pub pile_selected: bool,
    /// True if this card is known/revealed (relevant for opponent's hand).
    pub known: bool,
}

// ── Card registry ────────────────────────────────────────────────────────────

/// Two-way lookup between card names and `CardId` (set + collector number).
///
/// Built from `(name, set_code, collector_number)` entries.  Set codes are
/// sorted alphabetically to produce stable `set_index` values.
pub struct CardRegistry {
    set_to_index: HashMap<String, u16>,
    index_to_set: Vec<String>,
    name_to_id: HashMap<String, CardId>,
    id_to_name: HashMap<CardId, String>,
}

impl CardRegistry {
    /// Build from `(card_name, set_code, collector_number)` triples.
    ///
    /// Set codes are sorted alphabetically; their position becomes the u16
    /// `set_index` used in the wire format.
    pub fn from_entries(entries: &[(&str, &str, u16)]) -> Self {
        // Stable set-code ordering: sorted alphabetically.
        let mut set_codes: Vec<String> = entries
            .iter()
            .map(|(_, set, _)| set.to_string())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        set_codes.sort();

        let set_to_index: HashMap<String, u16> = set_codes
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), i as u16))
            .collect();

        let mut name_to_id = HashMap::new();
        let mut id_to_name = HashMap::new();

        for &(name, set_code, collector_num) in entries {
            let set_idx = set_to_index[set_code];
            let cid = CardId::new(set_idx, collector_num);
            name_to_id.insert(name.to_string(), cid);
            id_to_name.insert(cid, name.to_string());
        }

        Self { set_to_index, index_to_set: set_codes, name_to_id, id_to_name }
    }

    pub fn name_to_id(&self, name: &str) -> Option<CardId> {
        self.name_to_id.get(name).copied()
    }

    pub fn id_to_name(&self, id: CardId) -> Option<&str> {
        self.id_to_name.get(&id).map(|s| s.as_str())
    }

    pub fn set_code(&self, set_index: u16) -> Option<&str> {
        self.index_to_set.get(set_index as usize).map(|s| s.as_str())
    }
}

// ── Conversion from ScenarioResult ───────────────────────────────────────────

use super::{ScenarioResult, PlayerResult, PermanentResult};

impl BoardSnapshot {
    /// Convert a `ScenarioResult` (name-based) into a compact `BoardSnapshot`
    /// (id-based).  All card names are resolved through the registry.
    pub fn from_result(
        r: &ScenarioResult,
        registry: &CardRegistry,
    ) -> Result<Self, SnapshotError> {
        let stage = match r.stage.as_str() {
            "Early" => Stage::Early,
            "Mid" => Stage::Mid,
            _ => Stage::Late,
        };

        let stack: Vec<CardEntry> = r.stack.iter()
            .map(|name| Ok(CardEntry {
                id: lookup(registry, name)?,
                pile_selected: false,
                known: true,
            }))
            .collect::<Result<_, _>>()?;

        Ok(Self {
            turn: r.turn,
            stage,
            on_play: r.on_play,
            life_before_dd: r.life_before_dd.map(|l| l as i16),
            stack,
            us: PlayerSnapshot::from_player_result(&r.us, registry)?,
            opp: PlayerSnapshot::from_player_result(&r.opp, registry)?,
        })
    }

    /// Convert back to a `ScenarioResult`.  Logs are empty (they aren't part
    /// of the snapshot).
    pub fn to_result(&self, registry: &CardRegistry) -> ScenarioResult {
        let stage = match self.stage {
            Stage::Early => "Early",
            Stage::Mid => "Mid",
            Stage::Late => "Late",
        };

        ScenarioResult {
            turn: self.turn,
            stage: stage.to_string(),
            on_play: self.on_play,
            us: self.us.to_player_result(registry),
            opp: self.opp.to_player_result(registry),
            log: vec![],
            stack: self.stack.iter()
                .filter_map(|e| registry.id_to_name(e.id).map(|s| s.to_string()))
                .collect(),
            life_before_dd: self.life_before_dd.map(|l| l as i32),
            decision_log: vec![],
            text_summary: String::new(),
        }
    }
}

impl PlayerSnapshot {
    fn from_player_result(
        p: &PlayerResult,
        reg: &CardRegistry,
    ) -> Result<Self, SnapshotError> {
        let lands = p.lands.iter()
            .map(|pr| perm_entry(reg, pr))
            .collect::<Result<_, _>>()?;
        let permanents = p.permanents.iter()
            .map(|pr| perm_entry(reg, pr))
            .collect::<Result<_, _>>()?;
        let hand = p.hand.iter()
            .map(|cr| Ok(CardEntry {
                id: lookup(reg, &cr.name)?,
                pile_selected: false,
                known: true,
            }))
            .collect::<Result<_, _>>()?;
        let library = p.library.iter()
            .map(|name| Ok(CardEntry {
                id: lookup(reg, name)?,
                pile_selected: false,
                known: true,
            }))
            .collect::<Result<_, _>>()?;
        let graveyard = p.graveyard.iter()
            .map(|name| Ok(CardEntry {
                id: lookup(reg, name)?,
                pile_selected: false,
                known: false,
            }))
            .collect::<Result<_, _>>()?;
        let exile = p.exile.iter()
            .map(|name| Ok(CardEntry {
                id: lookup(reg, name)?,
                pile_selected: false,
                known: false,
            }))
            .collect::<Result<_, _>>()?;

        Ok(Self {
            deck_name: p.deck_name.clone(),
            life: p.life as i16,
            land_drop_available: p.land_drop_available,
            lands,
            permanents,
            hand,
            library,
            graveyard,
            exile,
            hand_hidden: p.hand_hidden as u8,
        })
    }

    fn to_player_result(&self, reg: &CardRegistry) -> PlayerResult {
        let resolve = |id: CardId| -> String {
            reg.id_to_name(id).unwrap_or("???").to_string()
        };

        PlayerResult {
            deck_name: self.deck_name.clone(),
            life: self.life as i32,
            lands: self.lands.iter().map(|e| PermanentResult {
                name: resolve(e.id),
                tapped: e.tapped,
                counters: e.counters as i32,
                loyalty: e.loyalty as i32,
                flipped: e.flipped,
            }).collect(),
            permanents: self.permanents.iter().map(|e| PermanentResult {
                name: resolve(e.id),
                tapped: e.tapped,
                counters: e.counters as i32,
                loyalty: e.loyalty as i32,
                flipped: e.flipped,
            }).collect(),
            hand: self.hand.iter()
                .map(|e| super::CardResult { name: resolve(e.id) })
                .collect(),
            hand_hidden: self.hand_hidden as usize,
            land_drop_available: self.land_drop_available,
            library: self.library.iter().map(|e| resolve(e.id)).collect(),
            graveyard: self.graveyard.iter().map(|e| resolve(e.id)).collect(),
            exile: self.exile.iter().map(|e| resolve(e.id)).collect(),
        }
    }
}

fn lookup(reg: &CardRegistry, name: &str) -> Result<CardId, SnapshotError> {
    reg.name_to_id(name).ok_or_else(|| SnapshotError::UnknownCard(name.to_string()))
}

fn perm_entry(reg: &CardRegistry, pr: &PermanentResult) -> Result<PermanentEntry, SnapshotError> {
    Ok(PermanentEntry {
        id: lookup(reg, &pr.name)?,
        tapped: pr.tapped,
        flipped: pr.flipped,
        pile_selected: false,
        counters: pr.counters as u8,
        loyalty: pr.loyalty as u8,
    })
}

// ── Binary encoding ──────────────────────────────────────────────────────────

const VERSION: u8 = 1;
const NONE_LIFE: i16 = i16::MIN;

pub fn encode(snap: &BoardSnapshot) -> Vec<u8> {
    let mut b = Vec::with_capacity(300);

    // Header (10 bytes).
    b.push(VERSION);
    b.push(snap.turn);
    b.push(snap.stage as u8);
    let mut flags: u8 = 0;
    if snap.on_play                  { flags |= 1; }
    if snap.us.land_drop_available   { flags |= 2; }
    if snap.opp.land_drop_available  { flags |= 4; }
    b.push(flags);
    b.extend_from_slice(&snap.us.life.to_le_bytes());
    b.extend_from_slice(&snap.opp.life.to_le_bytes());
    b.extend_from_slice(&snap.life_before_dd.unwrap_or(NONE_LIFE).to_le_bytes());

    // Stack.
    write_cards(&mut b, &snap.stack);

    // Players.
    write_player(&mut b, &snap.us);
    write_player(&mut b, &snap.opp);
    b
}

pub fn decode(data: &[u8]) -> Result<BoardSnapshot, SnapshotError> {
    let mut c = Cursor::new(data);

    let ver = c.u8()?;
    if ver != VERSION { return Err(SnapshotError::BadVersion(ver)); }

    let turn = c.u8()?;
    let stage = match c.u8()? {
        0 => Stage::Early,
        1 => Stage::Mid,
        2 => Stage::Late,
        x => return Err(SnapshotError::BadStage(x)),
    };
    let flags = c.u8()?;
    let on_play           = flags & 1 != 0;
    let us_land_drop      = flags & 2 != 0;
    let opp_land_drop     = flags & 4 != 0;

    let us_life  = c.i16()?;
    let opp_life = c.i16()?;
    let lbdd     = c.i16()?;
    let life_before_dd = if lbdd == NONE_LIFE { None } else { Some(lbdd) };

    let stack = read_cards(&mut c)?;

    let mut us = read_player(&mut c)?;
    us.life = us_life;
    us.land_drop_available = us_land_drop;

    let mut opp = read_player(&mut c)?;
    opp.life = opp_life;
    opp.land_drop_available = opp_land_drop;

    Ok(BoardSnapshot { turn, stage, on_play, life_before_dd, stack, us, opp })
}

fn write_player(b: &mut Vec<u8>, p: &PlayerSnapshot) {
    let name = p.deck_name.as_bytes();
    b.push(name.len() as u8);
    b.extend_from_slice(name);

    // Permanent zones.
    for zone in [&p.lands, &p.permanents] {
        b.push(zone.len() as u8);
        for e in zone {
            b.extend_from_slice(&e.id.set_index.to_le_bytes());
            b.extend_from_slice(&e.id.collector_number.to_le_bytes());
            let mut f: u8 = 0;
            if e.tapped        { f |= 1; }
            if e.flipped       { f |= 2; }
            if e.pile_selected { f |= 4; }
            b.push(f);
            b.push(e.counters);
            b.push(e.loyalty);
        }
    }

    // Card zones.
    for zone in [&p.hand, &p.library, &p.graveyard, &p.exile] {
        write_cards(b, zone);
    }

    b.push(p.hand_hidden);
}

fn write_cards(b: &mut Vec<u8>, zone: &[CardEntry]) {
    b.push(zone.len() as u8);
    for e in zone {
        b.extend_from_slice(&e.id.set_index.to_le_bytes());
        b.extend_from_slice(&e.id.collector_number.to_le_bytes());
        let mut f: u8 = 0;
        if e.pile_selected { f |= 1; }
        if e.known         { f |= 2; }
        b.push(f);
    }
}

fn read_player(c: &mut Cursor<'_>) -> Result<PlayerSnapshot, SnapshotError> {
    let name_len = c.u8()? as usize;
    let deck_name = String::from_utf8(c.bytes(name_len)?.to_vec())
        .map_err(|_| SnapshotError::BadUtf8)?;

    let lands      = read_permanents(c)?;
    let permanents = read_permanents(c)?;
    let hand       = read_cards(c)?;
    let library    = read_cards(c)?;
    let graveyard  = read_cards(c)?;
    let exile      = read_cards(c)?;
    let hand_hidden = c.u8()?;

    Ok(PlayerSnapshot {
        deck_name,
        life: 0,                    // filled in by caller from header
        land_drop_available: false,  // filled in by caller from header
        lands, permanents, hand, library, graveyard, exile, hand_hidden,
    })
}

fn read_permanents(c: &mut Cursor<'_>) -> Result<Vec<PermanentEntry>, SnapshotError> {
    let n = c.u8()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let set_index = c.u16()?;
        let coll = c.u16()?;
        let f = c.u8()?;
        v.push(PermanentEntry {
            id: CardId::new(set_index, coll),
            tapped:        f & 1 != 0,
            flipped:       f & 2 != 0,
            pile_selected: f & 4 != 0,
            counters: c.u8()?,
            loyalty:  c.u8()?,
        });
    }
    Ok(v)
}

fn read_cards(c: &mut Cursor<'_>) -> Result<Vec<CardEntry>, SnapshotError> {
    let n = c.u8()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let set_index = c.u16()?;
        let coll = c.u16()?;
        let f = c.u8()?;
        v.push(CardEntry {
            id: CardId::new(set_index, coll),
            pile_selected: f & 1 != 0,
            known:         f & 2 != 0,
        });
    }
    Ok(v)
}

// ── Byte cursor ──────────────────────────────────────────────────────────────

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self { Self { data, pos: 0 } }

    fn u8(&mut self) -> Result<u8, SnapshotError> {
        if self.pos >= self.data.len() { return Err(SnapshotError::TooShort); }
        let v = self.data[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn u16(&mut self) -> Result<u16, SnapshotError> {
        if self.pos + 2 > self.data.len() { return Err(SnapshotError::TooShort); }
        let v = u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn i16(&mut self) -> Result<i16, SnapshotError> {
        if self.pos + 2 > self.data.len() { return Err(SnapshotError::TooShort); }
        let v = i16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], SnapshotError> {
        if self.pos + n > self.data.len() { return Err(SnapshotError::TooShort); }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
}

// ── Base64url (RFC 4648 §5, no padding) ──────────────────────────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn to_base64url(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[((triple >> 18) & 0x3F) as usize] as char);
        out.push(B64[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 { out.push(B64[((triple >> 6) & 0x3F) as usize] as char); }
        if chunk.len() > 2 { out.push(B64[(triple & 0x3F) as usize] as char); }
    }
    out
}

pub fn from_base64url(s: &str) -> Result<Vec<u8>, SnapshotError> {
    let mut rev = [0xFFu8; 128];
    for (i, &ch) in B64.iter().enumerate() { rev[ch as usize] = i as u8; }

    let src = s.as_bytes();
    let mut out = Vec::with_capacity(src.len() * 3 / 4 + 1);
    let mut i = 0;

    while i < src.len() {
        let remaining = src.len() - i;
        let a = b64val(src[i], &rev)?;
        let b = b64val(src[i + 1], &rev)?;

        if remaining == 2 {
            out.push((a << 2) | (b >> 4));
            break;
        }
        let cc = b64val(src[i + 2], &rev)?;
        if remaining == 3 {
            out.push((a << 2) | (b >> 4));
            out.push((b << 4) | (cc >> 2));
            break;
        }
        let d = b64val(src[i + 3], &rev)?;
        out.push((a << 2) | (b >> 4));
        out.push((b << 4) | (cc >> 2));
        out.push((cc << 6) | d);
        i += 4;
    }
    Ok(out)
}

fn b64val(ch: u8, rev: &[u8; 128]) -> Result<u8, SnapshotError> {
    if ch >= 128 { return Err(SnapshotError::BadBase64); }
    let v = rev[ch as usize];
    if v == 0xFF { return Err(SnapshotError::BadBase64); }
    Ok(v)
}

// ── Convenience ──────────────────────────────────────────────────────────────

/// Encode a snapshot as a URL-safe base64 token.
pub fn to_url_token(snap: &BoardSnapshot) -> String {
    to_base64url(&encode(snap))
}

/// Decode a URL-safe base64 token back into a snapshot.
pub fn from_url_token(token: &str) -> Result<BoardSnapshot, SnapshotError> {
    decode(&from_base64url(token)?)
}

// ── Errors ───────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum SnapshotError {
    TooShort,
    BadVersion(u8),
    BadStage(u8),
    BadUtf8,
    BadBase64,
    UnknownCard(String),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort            => write!(f, "unexpected end of data"),
            Self::BadVersion(v)       => write!(f, "unsupported version {v}"),
            Self::BadStage(s)         => write!(f, "invalid stage {s}"),
            Self::BadUtf8             => write!(f, "invalid UTF-8 in deck name"),
            Self::BadBase64           => write!(f, "invalid base64url character"),
            Self::UnknownCard(name)   => write!(f, "card not in registry: {name}"),
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot() -> BoardSnapshot {
        let dd_id = CardId::new(0, 42);
        let sea_id = CardId::new(1, 100);
        let ritual_id = CardId::new(2, 55);
        let oracle_id = CardId::new(0, 10);
        let petal_id = CardId::new(3, 200);
        let delta_id = CardId::new(1, 83);
        let delver_id = CardId::new(4, 11);
        let volc_id = CardId::new(1, 110);

        BoardSnapshot {
            turn: 2,
            stage: Stage::Early,
            on_play: true,
            life_before_dd: Some(20),
            stack: vec![CardEntry { id: dd_id, pile_selected: false, known: true }],
            us: PlayerSnapshot {
                deck_name: "Doomsday".into(),
                life: 10,
                land_drop_available: false,
                lands: vec![
                    PermanentEntry {
                        id: sea_id, tapped: true, flipped: false,
                        pile_selected: false, counters: 0, loyalty: 0,
                    },
                    PermanentEntry {
                        id: delta_id, tapped: true, flipped: false,
                        pile_selected: false, counters: 0, loyalty: 0,
                    },
                ],
                permanents: vec![
                    PermanentEntry {
                        id: petal_id, tapped: false, flipped: false,
                        pile_selected: false, counters: 0, loyalty: 0,
                    },
                ],
                hand: vec![],
                library: vec![
                    CardEntry { id: oracle_id, pile_selected: true, known: true },
                    CardEntry { id: ritual_id, pile_selected: true, known: true },
                    CardEntry { id: petal_id, pile_selected: true, known: true },
                    CardEntry { id: sea_id, pile_selected: true, known: true },
                    CardEntry { id: sea_id, pile_selected: true, known: true },
                ],
                graveyard: vec![
                    CardEntry { id: ritual_id, pile_selected: false, known: false },
                ],
                exile: vec![],
                hand_hidden: 0,
            },
            opp: PlayerSnapshot {
                deck_name: "Izzet Delver".into(),
                life: 20,
                land_drop_available: true,
                lands: vec![
                    PermanentEntry {
                        id: volc_id, tapped: false, flipped: false,
                        pile_selected: false, counters: 0, loyalty: 0,
                    },
                ],
                permanents: vec![
                    PermanentEntry {
                        id: delver_id, tapped: false, flipped: true,
                        pile_selected: false, counters: 0, loyalty: 0,
                    },
                ],
                hand: vec![
                    CardEntry { id: ritual_id, pile_selected: false, known: true },
                ],
                library: vec![],
                graveyard: vec![],
                exile: vec![],
                hand_hidden: 3,
            },
        }
    }

    #[test]
    fn roundtrip_binary() {
        let snap = sample_snapshot();
        let bytes = encode(&snap);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(snap, decoded);
    }

    #[test]
    fn roundtrip_base64url() {
        let snap = sample_snapshot();
        let token = to_url_token(&snap);
        let decoded = from_url_token(&token).unwrap();
        assert_eq!(snap, decoded);

        // Verify the token is URL-safe (no +, /, or =).
        assert!(!token.contains('+'));
        assert!(!token.contains('/'));
        assert!(!token.contains('='));
    }

    #[test]
    fn snapshot_size() {
        let snap = sample_snapshot();
        let bytes = encode(&snap);
        let token = to_url_token(&snap);
        // Rough sanity: binary should be under 300 bytes, token under 400 chars.
        assert!(bytes.len() < 300, "binary too large: {} bytes", bytes.len());
        assert!(token.len() < 400, "token too large: {} chars", token.len());
        eprintln!("binary: {} bytes, base64url: {} chars", bytes.len(), token.len());
    }

    #[test]
    fn base64url_roundtrip_various_lengths() {
        for len in 0..20 {
            let data: Vec<u8> = (0..len).collect();
            let encoded = to_base64url(&data);
            let decoded = from_base64url(&encoded).unwrap();
            assert_eq!(data, decoded, "failed at len={len}");
        }
    }

    #[test]
    fn pile_selected_survives_roundtrip() {
        let snap = sample_snapshot();
        let bytes = encode(&snap);
        let decoded = decode(&bytes).unwrap();
        // Library cards should have pile_selected=true.
        assert!(decoded.us.library.iter().all(|c| c.pile_selected));
        // Graveyard card should have pile_selected=false.
        assert!(decoded.us.graveyard.iter().all(|c| !c.pile_selected));
    }

    #[test]
    fn decode_bad_version() {
        let snap = sample_snapshot();
        let mut bytes = encode(&snap);
        bytes[0] = 99;
        assert!(matches!(decode(&bytes), Err(SnapshotError::BadVersion(99))));
    }

    #[test]
    fn decode_truncated() {
        assert!(matches!(decode(&[1, 2]), Err(SnapshotError::TooShort)));
    }

    fn sample_registry() -> CardRegistry {
        CardRegistry::from_entries(&[
            ("Doomsday",            "WTH", 42),
            ("Underground Sea",     "3ED", 100),
            ("Dark Ritual",         "LEA", 55),
            ("Thassa's Oracle",     "THB", 10),
            ("Lotus Petal",         "TMP", 200),
            ("Polluted Delta",      "ONS", 83),
            ("Delver of Secrets",   "ISD", 11),
            ("Volcanic Island",     "3ED", 110),
        ])
    }

    #[test]
    fn registry_roundtrip() {
        let reg = sample_registry();
        let id = reg.name_to_id("Dark Ritual").unwrap();
        let name = reg.id_to_name(id).unwrap();
        assert_eq!(name, "Dark Ritual");
    }
}
