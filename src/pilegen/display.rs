use std::fmt;
use super::*;

pub(super) fn stage_label(turn: u8) -> &'static str {
    match turn {
        0..=3 => "Early",
        4..=5 => "Mid",
        _ => "Late",
    }
}

// ── Display ───────────────────────────────────────────────────────────────────

fn sec(label: &str) -> String {
    let total = 50usize;
    let label_with_spaces = format!(" {} ", label);
    let padding = total.saturating_sub(label_with_spaces.chars().count() + 2);
    format!("  ──{}{}", label_with_spaces, "─".repeat(padding))
}

impl std::fmt::Display for SimLand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.tapped {
            write!(f, "{} (tapped)", self.name)
        } else {
            write!(f, "{}", self.name)
        }
    }
}

impl std::fmt::Display for PlayerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.lands.is_empty() {
            writeln!(f, "  Lands      :")?;
            for land in &self.lands {
                writeln!(f, "    * {}", land)?;
            }
        }

        if !self.permanents.is_empty() {
            writeln!(f, "  Permanents :")?;
            for p in &self.permanents {
                let mut tags: Vec<String> = Vec::new();
                if let Some(ann) = &p.annotation { tags.push(ann.clone()); }
                if p.counters > 0 { tags.push(format!("+{} counters", p.counters)); }
                if p.loyalty > 0 { tags.push(format!("loyalty: {}", p.loyalty)); }
                let suffix = if tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", tags.join(", "))
                };
                writeln!(f, "    * {}{}", p.name, suffix)?;
            }
        }

        if !self.hand.is_empty() {
            writeln!(f, "  Hand       :")?;
            write!(f, "{}", self.hand)?;
        }

        if !self.graveyard.is_empty() {
            writeln!(f, "  Graveyard  :")?;
            write!(f, "{}", self.graveyard)?;
        }

        if !self.exile.is_empty() {
            writeln!(f, "  Exile      :")?;
            for card in &self.exile.visible {
                let tag = if self.on_adventure.contains(card) { " [on adventure]" } else { "" };
                writeln!(f, "    * {}{}", card, tag)?;
            }
            if self.exile.hidden > 0 {
                writeln!(f, "    * ({} hidden)", self.exile.hidden)?;
            }
        }

        Ok(())
    }
}

impl std::fmt::Display for SimState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let dbar = "═".repeat(50);
        writeln!(f)?;
        writeln!(f, "  ╔{}╗", dbar)?;
        writeln!(f, "  ║{:^50}║", " DOOMSDAY PILE SCENARIO ")?;
        writeln!(f, "  ╚{}╝", dbar)?;
        writeln!(f)?;
        writeln!(f, "  Deck    : {}", self.us.deck_name)?;
        writeln!(f, "  Opponent: {}", self.opp.deck_name)?;
        writeln!(
            f,
            "  Turn    : {} ({}, {})",
            self.turn,
            stage_label(self.turn),
            if self.on_play { "on the play" } else { "on the draw" }
        )?;

        if !self.log.is_empty() {
            writeln!(f)?;
            writeln!(f, "{}", sec("TURN LOG"))?;
            writeln!(f)?;
            for entry in &self.log {
                writeln!(f, "  {}", entry)?;
            }
        }

        writeln!(f)?;
        writeln!(f, "{}", sec("MY BOARD"))?;
        writeln!(f)?;
        writeln!(f, "  Life       : {} -> {}", self.us.life, self.us.life / 2)?;
        write!(f, "{}", self.us)?;
        writeln!(f)?;

        let opp_label = format!("OPPONENT: {}", self.opp.deck_name);
        writeln!(f, "{}", sec(&opp_label))?;
        writeln!(f)?;
        writeln!(f, "  Life       : {}", self.opp.life)?;
        write!(f, "{}", self.opp)?;

        Ok(())
    }
}
