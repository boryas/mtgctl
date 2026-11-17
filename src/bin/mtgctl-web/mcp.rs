//! The stats API as MCP tools (JSON-RPC 2.0 over streamable HTTP, POST /mcp), for agents the
//! players run: `claude mcp add --transport http mtgstats https://bur.io/mtg/stats/mcp --header
//! "Authorization: Bearer <read key>"` (the line the teams page makes).
//! Stateless: no session id, no server-initiated stream (GET is 405). Filters arrive as the
//! page's entries and go to the page's handlers as they are.

use super::*;
use mtgctl::mtgolog::filter;
use serde_json::{json, Value};

const PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

const INSTRUCTIONS: &str = "mtgstats: MTGO Legacy results from the game logs players upload: a player's record, results by event, matchups, win rates by deck, archetype, era or month, what an opponent played, cards cast, mulligans, play/draw, decklists. Start with `describe`. \
The key this connection uses decides whose games every tool sees: the key owner's own, or those of every member of the team it was made for; `lists` and `list` show only the owner's lists. \
One row per game; `player` names whose logs, `my_*` that player's side, `opp_*` the opponent; cards played are filterable. \
Players are MTGO usernames; spelling and case vary, so find one with `values` on `player` (or `opponent`) with op `contains` before filtering on it. \
A filter is {field, op, not, values}: op `is` (default; values ORed, null = unknown), `contains`, `>=`, `<=`; colours take op `all` | `exact` | `none` with one WUBRG string; `date` takes [from, to]. \
Card sentences: {field: my_play | opp_play, verb, values: [card names], other?: [targets], when?: any | before | after (my Doomsday resolved), scope?: game | match, n?: at least n times} and {field: my_list_has | opp_list_has, values: [cards], board?: any | main | side, n?: copies}. \
Card verbs with `needs` (opening_hand, mulliganed, bottomed, drawn, in_hand, boarded_in, boarded_out: `hand`; pile: `pile`) are the player's own hidden cards, read from their client session log, and hold only in games where `my_hand_known` (`my_pile_known`) = 1; a filter on one adds that condition. \
Grouping by a game-level column or filtering on one turns a query per game. `stats` returns counts, match and game wins, and averages per group; `matches` and `games` return the rows; `cards` counts card events over a selection (e.g. Forces drawn, cards seen, copies in my deck after sideboarding); `game` returns one game's full event log and, where the session log was uploaded, my hidden cards and my hand after every change.";

const NO_KEY: &str = "this server answers only with a key: make one on https://bur.io/mtg/stats/teams.html (log in with Discord) and add it to the MCP setup with --header \"Authorization: Bearer <key>\"";

fn filter_schema() -> Value {
    json!({
        "type": "array",
        "description": "filters, ANDed; see the server instructions for the entry shape",
        "items": {
            "type": "object",
            "properties": {
                "field": {"type": "string"},
                "op": {"type": "string"},
                "not": {"type": "boolean"},
                "values": {"type": "array"},
                "verb": {"type": "string"},
                "other": {"type": "array", "items": {"type": "string"}},
                "when": {"type": "string", "enum": ["any", "before", "after"]},
                "scope": {"type": "string", "enum": ["game", "match"]},
                "n": {"type": "integer"},
                "board": {"type": "string", "enum": ["any", "main", "side"]}
            },
            "required": ["field", "values"]
        }
    })
}

fn tools() -> Value {
    let limit = |d: usize| json!({"type": "integer", "default": d});
    json!([
        {
            "name": "describe",
            "description": "Start here for any MTGO Legacy results question: the column catalog, default filters, notes and card verbs.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "values",
            "description": "Distinct values of one column with their match and game counts, most matches first (at most 500), under optional filters. Use it to find a player's or deck's exact spelling (op contains) before filtering on it.",
            "inputSchema": {"type": "object", "properties": {"column": {"type": "string"}, "filters": filter_schema()}, "required": ["column"]}
        },
        {
            "name": "stats",
            "description": "Win rates and counts for any question about Legacy results, matchups or a player's record. Results grouped by columns: per group its matches, match wins and decided matches, games, game wins and decided games, first date, and averages (turns, minutes, mulligans). `split` filters each add a yes/no grouping. `hist` columns add a value histogram per group. Sort: games (default; chronological for list/era/month/date groups), time, wr, key.",
            "inputSchema": {"type": "object", "properties": {
                "by": {"type": "array", "items": {"type": "string"}, "description": "columns to group by"},
                "filters": filter_schema(),
                "split": filter_schema(),
                "hist": {"type": "array", "items": {"type": "string"}},
                "min_games": {"type": "integer", "default": 1},
                "sort": {"type": "string", "enum": ["games", "time", "wr", "key"]},
                "limit": limit(60),
                "game": {"type": "boolean", "description": "count games rather than matches"}
            }}
        },
        {
            "name": "matches",
            "description": "The matches the filters select (a match passes if any of its games does), newest first, with the decks, the inference behind the opponent's deck, and a compact per-game summary 'id:game:play|draw:mulligans:turns:winner:how'.",
            "inputSchema": {"type": "object", "properties": {"filters": filter_schema(), "limit": limit(200)}}
        },
        {
            "name": "games",
            "description": "One row per game the filters select, newest first.",
            "inputSchema": {"type": "object", "properties": {"filters": filter_schema(), "limit": limit(500)}}
        },
        {
            "name": "cards",
            "description": "Card events counted over the games the filters select. `rows`: per side, verb and card, the count `n` and the games with at least one; `totals`: the same per side and verb over every card (e.g. opening_hand + drawn = cards I saw; `drawn` counts every draw, so a card Brainstorm put back and drew again counts twice). Verbs are `describe`'s card_verbs plus `deck`: copies in my main deck for that game after sideboarding (its total is the deck size). My hidden verbs and `deck` count only the `games_hand_known` games, `pile` only games with my pile known. `by_game` gives one row per game instead, for per-game expectations (e.g. hypergeometric: cards seen × copies in deck / deck size).",
            "inputSchema": {"type": "object", "properties": {
                "filters": filter_schema(),
                "verbs": {"type": "array", "items": {"type": "string"}, "description": "verbs to count; none = all"},
                "cards": {"type": "array", "items": {"type": "string"}, "description": "cards for `rows`; none = every card. `totals` always covers every card"},
                "side": {"type": "string", "enum": ["me", "opp"], "description": "whose cards; none = both"},
                "by_game": {"type": "boolean", "description": "one row per game rather than summed over the selection"},
                "limit": limit(500)
            }}
        },
        {
            "name": "game",
            "description": "One game's cards played per side and its full event log. Where the player's session log was uploaded (`hand_known`): `my_cards`, their hidden card events by log `seq` (opening hand, mulligans, bottomed, draws, boarding, Doomsday pile); `my_deck`, their main deck after sideboarding; `my_hand`, their hand after every change, each at the `seq` and turn of the last log line at or before it (so the hand when the opponent cast a spell is the last entry with seq <= that cast's seq).",
            "inputSchema": {"type": "object", "properties": {"id": {"type": "integer"}}, "required": ["id"]}
        },
        {
            "name": "lists",
            "description": "Your registered decklists with the reference deck each is nearest to (lists are personal: a teammate's are not shown).",
            "inputSchema": {"type": "object", "properties": {"player": {"type": "string", "description": "one of your MTGO usernames; default your first"}}}
        },
        {
            "name": "list",
            "description": "One of your registered lists: its cards and its delta against its reference deck.",
            "inputSchema": {"type": "object", "properties": {"player": {"type": "string"}, "id": {"type": "integer"}}, "required": ["id"]}
        }
    ])
}

/// Prompt templates, exposed by Claude Code as `/mcp__<server>__<name>` slash commands.
fn prompts() -> Value {
    json!([
        {
            "name": "record",
            "description": "A player's results: overall, by event type, by deck and by opponent archetype",
            "arguments": [{"name": "player", "description": "MTGO username", "required": true}]
        },
        {
            "name": "compare",
            "description": "Two players' results side by side: record, decks, matchups, trend",
            "arguments": [{"name": "a", "description": "MTGO username", "required": true}, {"name": "b", "description": "MTGO username", "required": true}]
        },
        {
            "name": "matchup",
            "description": "How a deck fares against an opponent deck or archetype, across eras",
            "arguments": [{"name": "deck", "required": true}, {"name": "opponent", "description": "opponent deck or archetype", "required": true}]
        }
    ])
}

fn prompt_text(name: &str, args: &Value) -> Option<String> {
    let g = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("?").to_string();
    Some(match name {
        "record" => format!("Using the mtgctl-web stats tools, summarise {}'s MTGO Legacy results. Find the exact spelling with `values` on `player` first. Give the overall record, by event type, by deck, by opponent archetype, and the trend by month. State sample sizes.", g("player")),
        "compare" => format!("Using the mtgctl-web stats tools, compare {} and {}: record by event type, decks played, matchups by opponent archetype and deck, and trend by era and month. Find each spelling with `values` on `player` first. Note where the samples are too small to separate them.", g("a"), g("b")),
        "matchup" => format!("Using the mtgctl-web stats tools, report how {} does against {}: match and game win rates (filter `my_deck` or `my_ref_deck`, group by `opp_deck` or `opp_archetype`), by era, with sample sizes.", g("deck"), g("opponent")),
        _ => return None,
    })
}

fn arg_str(a: &Value, k: &str) -> Option<String> {
    a.get(k).and_then(|v| v.as_str()).map(str::to_string).filter(|s| !s.is_empty())
}
fn arg_json(a: &Value, k: &str) -> String {
    a.get(k).filter(|v| !v.is_null()).map(Value::to_string).unwrap_or_default()
}
fn arg_strings(a: &Value, k: &str) -> Vec<String> {
    a.get(k).and_then(|v| v.as_array()).map(|xs| xs.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn q(pairs: &[(&str, String)]) -> Query<HashMap<String, String>> {
    Query(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
}

/// A tool's result as JSON, or the message of the error the caller sees.
async fn call(st: &Shared, headers: &HeaderMap, name: &str, a: &Value) -> Result<Value, String> {
    let api = |e: ApiError| e.1;
    let ds = dataset(None).map_err(api)?;
    if name != "describe" && auth::viewer(st, headers, false).await.map_err(api)?.account.is_none() {
        return Err(NO_KEY.into());
    }
    let h = || headers.clone();
    match name {
        "describe" => {
            let s = schema(State(st.clone()), q(&[])).await.map_err(api)?.0;
            let mut v = serde_json::to_value(&s.dataset).unwrap();
            v["present_columns"] = json!(s.present);
            v["defaults"] = s.defaults;
            if ds.features.cards {
                v["notes"] = json!(stats::COLUMNS_NOTE);
                v["card_verbs"] = json!(filter::VERBS.iter().map(|(v, other, needs)| json!({"verb": v, "takes_other": other, "needs": needs})).collect::<Vec<_>>());
            }
            Ok(v)
        }
        "values" => {
            let col = arg_str(a, "column").ok_or("column required")?;
            let params = [("col", col), ("filters", arg_json(a, "filters"))];
            Ok(serde_json::to_value(values(State(st.clone()), h(), q(&params)).await.map_err(api)?.0).unwrap())
        }
        "stats" => {
            let p = StatsParams {
                by: arg_strings(a, "by").join(","),
                filters: arg_json(a, "filters"),
                groups: arg_json(a, "split"),
                min: a.get("min_games").and_then(Value::as_i64).unwrap_or(1),
                limit: a.get("limit").and_then(Value::as_u64).unwrap_or(60) as usize,
                sort: arg_str(a, "sort").unwrap_or_else(|| "games".into()),
                full: false,
                hist: arg_strings(a, "hist").join(","),
                game: a.get("game").and_then(Value::as_bool),
                ds: None,
            };
            Ok(serde_json::to_value(stats_api(State(st.clone()), h(), Query(p)).await.map_err(api)?.0).unwrap())
        }
        "matches" | "games" => {
            let p = MatchesParams { filters: arg_json(a, "filters"), groups: String::new(), limit: a.get("limit").and_then(Value::as_u64).unwrap_or(if name == "matches" { 200 } else { 500 }) as usize };
            Ok(if name == "matches" {
                serde_json::to_value(matches_api(State(st.clone()), h(), Query(p)).await.map_err(api)?.0).unwrap()
            } else {
                serde_json::to_value(games_api(State(st.clone()), h(), Query(p)).await.map_err(api)?.0).unwrap()
            })
        }
        "game" => {
            let id = a.get("id").and_then(Value::as_i64).ok_or("id required")?;
            Ok(serde_json::to_value(game_detail(State(st.clone()), h(), q(&[("id", id.to_string()), ("hands", "1".into())])).await.map_err(api)?.0).unwrap())
        }
        "cards" => {
            let p = CardCountParams {
                filters: arg_json(a, "filters"),
                groups: String::new(),
                verbs: Some(arg_strings(a, "verbs").join(",")),
                cards: Some(serde_json::to_string(&arg_strings(a, "cards")).unwrap()),
                side: arg_str(a, "side"),
                by_game: a.get("by_game").and_then(Value::as_bool).unwrap_or(false),
                limit: a.get("limit").and_then(Value::as_u64).unwrap_or(500) as usize,
            };
            Ok(serde_json::to_value(card_counts(State(st.clone()), h(), Query(p)).await.map_err(api)?.0).unwrap())
        }
        "lists" | "list" => {
            let mut params: Vec<(&str, String)> = arg_str(a, "player").map(|p| ("player", p)).into_iter().collect();
            if name == "list" {
                params.push(("id", a.get("id").and_then(Value::as_i64).ok_or("id required")?.to_string()));
                Ok(serde_json::to_value(list_detail(State(st.clone()), h(), q(&params)).await.map_err(api)?.0).unwrap())
            } else {
                Ok(serde_json::to_value(lists(State(st.clone()), h(), q(&params)).await.map_err(api)?.0).unwrap())
            }
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

fn rpc_error(id: Value, code: i64, msg: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg.into()}})
}

/// One request's response; `None` for a notification.
async fn handle(st: &Shared, headers: &HeaderMap, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned().filter(|i| !i.is_null())?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = if PROTOCOL_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSIONS[PROTOCOL_VERSIONS.len() - 1] };
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}, "prompts": {"listChanged": false}},
                "serverInfo": {"name": "mtgstats", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "prompts/list" => json!({"prompts": prompts()}),
        "prompts/get" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match prompt_text(name, &args) {
                Some(t) => json!({"messages": [{"role": "user", "content": {"type": "text", "text": t}}]}),
                None => return Some(rpc_error(id, -32602, format!("no prompt {name}"))),
            }
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call(st, headers, name, &args).await {
                Ok(v) => json!({"content": [{"type": "text", "text": serde_json::to_string(&v).unwrap()}], "structuredContent": if v.is_object() { v } else { json!({"rows": v}) }}),
                Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
            }
        }
        _ => return Some(rpc_error(id, -32601, format!("method not found: {method}"))),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

pub async fn mcp(State(st): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(rpc_error(Value::Null, -32700, format!("parse error: {e}")))).into_response(),
    };
    let out = match &msg {
        Value::Array(batch) => {
            let mut out = vec![];
            for m in batch {
                out.extend(handle(&st, &headers, m).await);
            }
            if out.is_empty() { None } else { Some(Value::Array(out)) }
        }
        m => handle(&st, &headers, m).await,
    };
    match out {
        Some(v) => Json(v).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

pub async fn mcp_get() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, "MCP over streamable HTTP: POST JSON-RPC here; this server opens no stream").into_response()
}
