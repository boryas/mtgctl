## Pilegen Rules → Card Index

Maps CR rules and engine mechanics to the cards that use them.
Given a rule or mechanic, find which cards exercise it and how.
Identifies non-standard patterns.

**Maintain this file**: when adding or modifying a card, update the relevant
sections below. When adding a new engine primitive, add a section for it.

### How to Read

- **Standard** — uses existing engine primitives, no special logic.
- **Notable** — correct but uses an unusual pattern worth knowing about.
- **Abnormal** — workarounds, hardcoded shortcuts, or missing features.

---

### Mana Production (CR 106)

#### Tap for mana (basic pattern)

All basic lands, ABU duals, Great Furnace use `tap_produces(mana_string)`.

| Card | Produces | Notes |
|------|----------|-------|
| Basic lands (6) | Single color | `basic_land()` helper |
| Snow-Covered basics (6) | Single color | `snow_basic()` helper |
| ABU duals (10) | Two colors | Two `tap_produces` calls |
| Great Furnace | {R} | Also typed as Artifact |

#### Sacrifice for mana

| Card | Cost | Produces | Notes |
|------|------|----------|-------|
| Lotus Petal | Sac self | Any color | `SacSelf`, `produces_colors("WUBRG")` |
| Lion's Eye Diamond | Discard hand + sac | 3 of any one color | `DiscardHand` cost; `timing: Instant` (excluded from mana sub-loop) |
| Dark Ritual | Cast (instant) | BBB | Spell, not mana ability |

#### Conditional mana abilities

| Card | Condition | Notes |
|------|-----------|-------|
| Mox Opal | Metalcraft (3+ artifacts) | `condition: Some(metalcraft)` predicate |

#### Colorless mana producers

| Card | Produces | Notes |
|------|----------|-------|
| Ancient Tomb | CC | Also `eff_life_loss(who, 2)` |
| City of Traitors | CC | Has self-sacrifice trigger on another land played |
| Cavern of Souls | C or any color | Two mana abilities; color-producing one lacks type restriction |
| Wastes / Snow-Covered Wastes | C | `tap_produces("")` |

#### Hand-zone mana ability (CR 605.3)

| Card | Cost | Produces |
|------|------|----------|
| Simian Spirit Guide | Exile self from hand | {R} |

---

### Enters Tapped (CR 614.1 replacement)

| Card | Condition | Mechanism |
|------|-----------|-----------|
| MKM surveil duals (10) | Always | `replacement_enters_tapped()` via `surveil_dual()` |
| Mistrise Village | Unless you control Mountain/Forest | `etb_self_replacement` checks materialized land types |

---

### ETB Triggers (CR 603)

#### Search/tutor on ETB

| Card | Finds | Destination |
|------|-------|-------------|
| Recruiter of the Guard | Creature toughness ≤ 2 | Hand |

#### Damage on ETB

| Card | Damage | Target |
|------|--------|--------|
| Orcish Bowmasters | 1 | Any target (also triggers on opponent draws) |
| Fury | 4 | Creature/planeswalker |

#### Choice on ETB (via `etb_self_replacement`)

| Card | Choice type | Stored in | Drives |
|------|-------------|-----------|--------|
| Cavern of Souls | CreatureType | `bf.etb_choice` | Uncounterable (TODO: not enforced) |
| Painter's Servant | Color | `bf.etb_choice` | L5 CE adding chosen color globally |
| Disruptor Flute | CardName | `bf.etb_choice` | L3 CE: +3 cost & suppress abilities |
| Engineered Explosives | (sunburst) | `obj.counters[Charge]` | Reads `resolving_costs_ctx.chosen_x` |

#### Placeholder/incomplete ETB

| Card | Issue |
|------|-------|
| Atraxa, Grand Unifier | **Abnormal**: silently adds 4 cards; no strategy callback |
| Thassa's Oracle | **Abnormal**: strategy checks directly; no actual trigger |

---

### ETB Replacements (CR 614.1 — "As ~ enters")

Modify the ZoneChange event itself rather than triggering after it.

| Card | What it replaces | Notes |
|------|------------------|-------|
| Murktide Regent | Sets counters from delve exile count | Reads `resolving_costs_ctx.objects_moved` |
| Dauthi Voidwalker | Opponent GY → Exile + Void counter | `dv_replacement_check` |
| Painter's Servant | Stores color, registers L5 CE | `etb_self_replacement` |
| Cavern of Souls | Stores creature type | `etb_self_replacement` |
| Disruptor Flute | Stores card name, registers L3 CE | `etb_self_replacement` |
| Engineered Explosives | Sets Charge counters from X | `etb_self_check` |
| MKM surveil duals (10) | Sets `bf.tapped = true` | `replacement_enters_tapped()` |
| Mistrise Village | Conditionally sets `bf.tapped` | Checks for Mountain/Forest |
| Karn / Tamiyo back | Planeswalker loyalty setup | `replacement_planeswalker_etb(N)` |

---

### Triggered Abilities (CR 603)

#### Draw triggers

| Card | Condition | Effect |
|------|-----------|--------|
| Orcish Bowmasters | Opponent non-natural draw | 1 damage + Amass Orc 1 |
| Tamiyo, Inquisitive Student | Controller's natural draw | Surveil; tracks draw count for transform |

#### Spell-cast triggers

| Card | Condition | Effect |
|------|-----------|--------|
| Lavinia | Opponent spell, no mana spent | Counter it |
| Dragon's Rage Channeler | Controller noncreature spell | Surveil 1 |
| Cori-Steel Cutter (flurry) | Controller's second spell this turn | Create Monk Token, may attach |
| Monk Token (prowess) | Controller noncreature spell | +1/+1 until EOT (L7 CE) |
| Flusterstorm (Storm) | Self cast | Copy N times |

#### Land-played trigger

| Card | Condition | Effect |
|------|-----------|--------|
| City of Traitors | Controller plays another land | Sacrifice self |

#### Upkeep triggers

| Card | Condition | Effect |
|------|-----------|--------|
| Delver of Secrets | Controller upkeep, instant/sorcery on top | Transform → Insectile Aberration |

#### Delayed triggers

| Card | When | Effect |
|------|------|--------|
| Sneak Attack | Next end step | Sacrifice creature; `OneShot` `TriggerInstance` |
| Fury (evoke) | Self ETB with `alt_cost_index` set | Sacrifice self |
| Mishra's Bauble | Next upkeep | Draw 1; `OneShot` delayed `TriggerInstance` |

#### Ward (CR 702.21)

| Card | Cost | Notes |
|------|------|-------|
| Hexing Squelcher | 2 life | `ward_pay_or_counter`; also grants Ward to other creatures via L6 CE |

---

### Replacement Effects (CR 614.1)

| Card | Replaces | With |
|------|----------|------|
| Leyline of the Void | Any card → GY | → Exile |
| Dauthi Voidwalker | Opponent's cards → GY | → Exile + Void counter |
| Murktide Regent | Self → BF | Self → BF with counters |
| Containment Priest | Nontoken creature → BF (non-cast) | → Exile; `active_when: on_battlefield` |

---

### Prohibition Effects (CR 614.17)

#### "Can't be countered"

| Card | Scope | Mechanism |
|------|-------|-----------|
| Emrakul | Self only | `ProhibitionDef` on `SpellBeingCountered` |
| Long Goodbye | Self only | Same |
| Hexing Squelcher | Self (stack) + all controller's spells (BF) | Two `ProhibitionDef` entries |
| Mistrise Village | Next spell this turn | `LatentSpellMod` grants `ProhibitionDef` |

#### "Can't enter the battlefield"

| Card | What | Mechanism |
|------|------|-----------|
| Grafdigger's Cage | Creatures from GY/library → BF | `ProhibitionDef` on `ZoneChange` |

#### "Can't cast"

| Card | What | Mechanism |
|------|------|-----------|
| Grafdigger's Cage | Spells from GY/library | L3 CE sets `castable = false` |
| Lavinia | Opponent noncreature MV > lands | L6 CE sets `castable = false` |

---

### Continuous Effects / Layer System (CR 613)

#### L3 — Text Effects

| Card | Effect |
|------|--------|
| Omniscience | Adds free-cast `AlternateCost` to all non-lands |
| Disruptor Flute | +3 casting cost + suppress abilities for named card |
| Dauthi Voidwalker (activated) | `castable = true` + free-cast on exiled card (EndOfTurn) |
| Grafdigger's Cage | `castable = false` for GY/library cards |

#### L4 — Type Effects

| Card | Effect |
|------|--------|
| Blood Moon / Magus of the Moon | Nonbasic lands become Mountains | `nonbasic_lands_are_mountains()` shared |
| Urborg, Tomb of Yawgmoth | All lands gain Swamp + {T}: Add {B} | `each_land_gains_subtype()` shared |
| Yavimaya, Cradle of Growth | All lands gain Forest + {T}: Add {G} | Same |

#### L5 — Color Effects

| Card | Effect |
|------|--------|
| Painter's Servant | All objects gain chosen color (global filter) |

#### L6 — Ability Effects

| Card | Effect |
|------|--------|
| Karn, the Great Creator | Suppress all abilities on opponent artifacts |
| Null Rod | Suppress all activated abilities on all artifacts (both players) |
| Lavinia | `castable = false` on opponent noncreature spells |
| Hexing Squelcher | Grants Ward trigger to other creatures via `granted_trigger_defs` |
| Sneak Attack | Grants Haste to sneaked creature |
| Cori-Steel Cutter | Grants Trample + Haste to equipped creature via `bf.attached_to` filter |
| Mistrise Village | `LatentSpellMod` applies CE granting "can't be countered" on next spell |
| Dragon's Rage Channeler | Delirium: grants Flying when ≥4 card types in GY |

#### L7 — Power/Toughness

| Card | Effect |
|------|--------|
| Toxic Deluge | All creatures get -X/-X (EndOfTurn); SBA handles death at 0 toughness |
| Dragon's Rage Channeler | Delirium: +2/+2 when ≥4 card types in GY |
| Cori-Steel Cutter | +1/+1 to equipped creature via `bf.attached_to` filter |
| Monk Token (prowess) | +1/+1 until EOT per noncreature spell (stacking CIs) |

---

### Alternate Costs (CR 118.9b)

#### Pitch (exile from hand)

| Card | Exile | Extra cost | Condition |
|------|-------|------------|-----------|
| Force of Will | Blue card | 1 life | — |
| Force of Negation | Blue card | — | Not your turn |
| Fury | Red card | — | `hand_min: 2` |

#### Life payment

| Card | Life |
|------|------|
| Snuff Out | 4 |
| Surgical Extraction | 2 (Phyrexian mana) |

#### Return permanent

| Card | What |
|------|------|
| Daze | Island to hand |

#### Free cast (conditional)

| Card | Condition |
|------|-----------|
| Mindbreak Trap | Opponent cast 3+ spells |
| Omniscience (granted) | L3 CE adds `AlternateCost::default()` |
| Dauthi Voidwalker (granted) | Activated ability grants free-cast |

---

### Activation Timing (CR 602.5b, 605.3a)

`ActivationTiming` enum on `ManaAbility` and `AbilityDef`:
- `Default` — natural speed (mana abilities: mana sub-loop; abilities: instant speed)
- `Instant` — priority only, any stack state (excluded from mana sub-loop)
- `Sorcery` — priority only, empty stack + main phase

| Card | Ability type | Timing | Notes |
|------|-------------|--------|-------|
| Lion's Eye Diamond | ManaAbility | Instant | Can't activate during CR 601.2g mana sub-loop |
| Tamiyo +2 (back) | AbilityDef | Sorcery | Loyalty ability (generalizes `is_loyalty_ability()` check) |

---

### Additional Costs (CR 118.9d)

| Card | Cost | Notes |
|------|------|-------|
| Bitter Triumph | Discard OR 3 life | `CostOr` |
| Toxic Deluge | X life | `XLife` |
| Engineered Explosives | X mana (sunburst) | `XMana` |
| Consign to Memory | Replicate {1} | `Replicate` |

---

### Modal Spells (CR 700.2)

| Card | Modes | Notes |
|------|-------|-------|
| Sheoldred's Edict | 3: sac nontoken / sac token / sac PW | `SpellModes::modal` |
| Brotherhood's End | 2: 3 damage to creatures+PWs / destroy artifacts MV≤3 | `SpellModes::modal` |
| Abrade | 2: 3 damage to creature / destroy artifact | `SpellModes::modal` |

---

### Keyword Abilities (CR 702)

#### Evasion / Combat

| Keyword | Cards |
|---------|-------|
| Flying | Emrakul, Griselbrand, Atraxa, Dragon's Rage Channeler (delirium), Insectile Aberration |
| Shadow | Dauthi Voidwalker |
| Double Strike | Fury |
| Trample | Granted by Cori-Steel Cutter via L6 CE; stored as keyword, not functionally modeled |
| Haste | Granted by Sneak Attack via L6 CE; also by Cori-Steel Cutter |

#### Other

| Keyword | Cards | Notes |
|---------|-------|-------|
| Delve | Murktide Regent | `data.delve = true` |
| Storm | Flusterstorm | Self-cast trigger creates `StackAbility` copies |
| Replicate | Consign to Memory | `CostComponent::Replicate` |
| Ward | Hexing Squelcher | `ward_pay_or_counter` helper |
| Protection | Emrakul | `protection_from: vec![obj_pred_colored_spell()]` |
| Annihilator 6 | Emrakul | Stored as keyword; **not functionally modeled** |
| Cycling | Street Wraith, Edge of Autumn | Street Wraith: hand `AbilityDef`; Edge: **strategy-only** |
| Prowess | Monk Token (from Cori-Steel Cutter) | Triggered ability: +1/+1 EOT per noncreature spell |
| Ninjutsu | Ingenious Infiltrator, Kaito | `ninjutsu_ability()` helper |

---

### Keyword Actions

#### Counter (CR 701.5)

| Card | Target | Primitive |
|------|--------|-----------|
| Force of Will | Any spell | `eff_counter_target` |
| Force of Negation | Noncreature | `eff_counter_and_exile` |
| Daze | Any spell | `eff_counter_unless_pays` ({1}) |
| Spell Pierce | Noncreature | `eff_counter_unless_pays` ({2}) |
| Flusterstorm | Instant/sorcery | `eff_counter_unless_pays` ({1}) + storm copies |
| Mindbreak Trap | Any number | `eff_exile_all_targets` (exiles, doesn't counter) |
| Consign to Memory | Triggered ability or colorless | `eff_counter_target` |

#### Destroy (CR 701.7)

| Card | Target | Primitive |
|------|--------|-----------|
| Fatal Push | Creature MV≤3 | `eff_destroy_target` |
| Snuff Out | Non-black creature | `eff_destroy_target` |
| Long Goodbye | Creature/PW MV≤3 | `eff_destroy_target` (uncounterable) |
| Bitter Triumph | Creature/PW | `eff_destroy_target` |
| Wasteland | Nonbasic land | `eff_destroy_target` (ability) |
| Abrade | Artifact (mode) | `eff_destroy_target` |
| REB/Pyroblast/BEB/Hydroblast | Color-restricted | `counter_or_destroy` / `counter_or_destroy_if_color` |
| Brotherhood's End | Artifacts MV≤3 (mode) | `eff_destroy_all` |
| Engineered Explosives | Nonland MV=charges | `eff_destroy_all` |
| Meltdown | Artifacts MV≤X | `eff_destroy_all`; `XMana` additional cost |

#### Exile (CR 701.10)

| Card | Target | Notes |
|------|--------|-------|
| Swords to Plowshares | Creature | `eff_exile_target_gain_power` |
| Surgical Extraction | GY card + all copies | Custom effect iterating zones |
| Mindbreak Trap | All targeted spells | `eff_exile_all_targets` |

#### Search (CR 701.19)

| Card | Finds | Destination |
|------|-------|-------------|
| Fetch lands (10) | Land by subtype | Battlefield |
| Recruiter of the Guard | Creature toughness≤2 | Hand |
| Personal Tutor | Sorcery | Library (top) |
| Green Sun's Zenith | Green creature | Battlefield |
| Urza's Saga | Artifact, no colored pips, MV≤1 | Battlefield |

#### Sacrifice (CR 701.16)

| Card | What | Trigger |
|------|------|---------|
| Sheoldred's Edict | Opponent (modal) | `eff_sacrifice` |
| Fury (evoke) | Self | ETB when alt cost used |
| Sneak Attack | Creature | Delayed end-step trigger |
| City of Traitors | Self | Another-land-played trigger |

#### Bounce (CR 701.15)

| Card | Target |
|------|--------|
| Karakas | Legendary creature (either player's) |
| Brazen Borrower (Petty Theft) | Nonland permanent |

#### Discard (CR 701.8)

| Card | Amount | Filter |
|------|--------|--------|
| Thoughtseize | 1 | Nonland |
| Hymn to Tourach | 2 | Random |

#### Draw (CR 701.9)

| Card | Amount | Notes |
|------|--------|-------|
| Brainstorm | 3 (put back 2) | `eff_draw(3).then(eff_put_back(2))` |
| Griselbrand | 7 | Pay 7 life ability |
| Street Wraith | 1 | Cycling |
| Stock Up | 2 | Simplified from "look at 5, pick 2" |
| Preordain | 1 | Simplified from "scry 2, draw 1" |
| Ponder | 1 | Simplified from "look at 3, pick 1" |
| Consider | 1 | Simplified from "surveil 1, draw 1" |
| Clue Token | 1 | {2}, tap, sac |
| Mishra's Bauble | 1 | Delayed (next upkeep); tap + sac |

#### Damage (CR 120)

| Card | Amount | Target |
|------|--------|--------|
| Lightning Bolt | 3 | Any target |
| Orcish Bowmasters | 1 | Any (ETB + draw trigger) |
| Fury | 4 | Creature/PW (ETB) |
| Brotherhood's End | 3 | All creatures + PWs (mode) |
| Abrade | 3 | Creature (mode) |
| Unholy Heat | 2/6 | Creature/PW (delirium: 6) |
| Price of Progress | 2×nonbasics | Each player |
| Rough | 2 | All creatures without flying |
| Tumble | 6 | All creatures with flying |
| Ancient Tomb | 2 | Controller (life loss, not damage) |

#### Surveil (CR 701.43)

| Card | Amount |
|------|--------|
| MKM surveil duals (10) | 1 (ETB trigger) |
| Dragon's Rage Channeler | 1 (noncreature spell-cast trigger) |
| Tamiyo | Strategy-driven |

#### Amass (CR 701.44)

| Card | Type | Amount |
|------|------|--------|
| Orcish Bowmasters | Orc | 1 |

#### Token creation

| Card | Token | Notes |
|------|-------|-------|
| Orcish Bowmasters | Orc Army 0/0 | Amass; grown by counters |
| Tamiyo | Clue Token | Artifact with {2}, tap, sac: draw 1 |
| Cori-Steel Cutter | Monk Token 1/1 | White creature with prowess; flurry trigger |

---

### Equipment / Attach (CR 301.5)

| Card | Equip cost | Grants | Notes |
|------|-----------|--------|-------|
| Cori-Steel Cutter | {1}{R} | +1/+1, trample, haste | `BattlefieldState.attached_to`; sorcery-speed equip; flurry auto-attaches |

---

### DFC / Adventure / Split (CR 712, 715)

| Card | Layout | Notes |
|------|--------|-------|
| Tamiyo | `DoubleFaced` | Front: creature; back: planeswalker |
| Delver of Secrets | `DoubleFaced` | Front: 1/1; back: 3/2 flying (Insectile Aberration) |
| Brazen Borrower | `Split` | Front: creature; back: adventure instant |
| Rough // Tumble | `Split` | True split card; Rough: 2 to non-flyers; Tumble: 6 to flyers |

---

### Multi-typed Cards

| Card | Types | Notes |
|------|-------|-------|
| Great Furnace | Land + Artifact | `types.push(CardType::Artifact)` after construction |
| Painter's Servant | Creature + Artifact | Same pattern |
| Urza's Saga | Artifact only | **Notable**: real card is Enchantment Land Saga; saga system not implemented |

---

### Abnormal Implementations Summary

#### Missing functionality

| Card | What's missing |
|------|----------------|
| Emrakul | Extra turn on cast, annihilator 6 (functional), GY shuffle |
| Atraxa | Real ETB (reveal 10, pick per type); placeholder adds 4 cards |
| Thassa's Oracle | No ETB trigger; strategy checks directly |
| Cavern of Souls | Mana type restriction + uncounterable for named type |
| Tamiyo (back) | Only +2 loyalty ability modeled |
| Brazen Borrower | Front face missing Flying keyword |
| Edge of Autumn | No cycling AbilityDef; strategy handles manually |
| Consider/Stock Up/Ponder/Preordain | Selection simplified to draw N |

#### Non-standard patterns

| Card | Pattern | Why unusual |
|------|---------|-------------|
| Urza's Saga | Artifact not Enchantment Land Saga | Saga system absent |
| Hexing Squelcher | Ward grant via `granted_trigger_defs` in L6 modifier | Verbose but correct |
| Mistrise Village | `LatentSpellMod` with predicate + CI factory | Complex: spell-mod pattern instead of dormant CE |
| Painter's Servant | CE registered in replacement, not `static_ability_defs` | Needs `etb_choice` to know the color |

#### Intentional simplifications

| Card | Simplification | Rationale |
|------|----------------|-----------|
| Doomsday | Sets `success = true` | Win-probability sim, not full game |
| Ponder/Stock Up/Consider | Draw N instead of selection | Library order not fully tracked |
| Fetch lands | Implicit shuffle | Library order not tracked |
