//! The stats API as MCP tools (JSON-RPC 2.0 over streamable HTTP, POST /mcp), for agents the
//! players run: `claude mcp add --transport http stats https://bur.io/doomsday/stats/mcp`.
//! Stateless: no session id, no server-initiated stream (GET is 405). Filters arrive as the
//! page's entries and become SQL here (`filter::where_sql`), never as SQL from the caller.

use super::*;
use mtgctl::mtgolog::filter::{self, Entry};
use serde_json::{json, Value};

const PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

const INSTRUCTIONS: &str = "mtgctl-web: MTGO Legacy results, updated as players upload logs and the Legacy Data Collection publishes: a player's record, Challenge results, matchups, win rates by deck, archetype, era or month, what an opponent played, cards cast, mulligans, play/draw, decklists. Start with `describe`. \
Datasets: `mtgo` = uploaded game logs of every team member (one row per game; `player` names whose logs, `my_*` that player's side, `opp_*` the opponent; cards played are filterable). \
`ldc` = the Legacy Data Collection: every player's matches in MTGO Challenges and qualifiers, one row per game per side (A = `my_*`, B = `opp_*`), scores only; `my_deck` is the LDC's deck name, `my_ref_deck` the finer reference deck when the list was published. \
Players are MTGO usernames; spelling and case vary, so find one with `values` on `player` (or `opponent`) with op `contains` before filtering on it. \
A filter is {field, op, not, values}: op `is` (default; values ORed, null = unknown), `contains`, `>=`, `<=`; colours take op `all` | `exact` | `none` with one WUBRG string; `date` takes [from, to]. \
On `mtgo`, card sentences: {field: my_play | opp_play, verb, values: [card names], other?: [targets], when?: any | before | after (my Doomsday resolved), scope?: game | match, n?: at least n times} and {field: my_list_has | opp_list_has, values: [cards], board?: any | main | side, n?: copies}. \
Grouping by a game-level column or filtering on one turns a query per game. `stats` returns counts, match and game wins, and averages per group; `matches` and `games` return the rows; `game` returns one game's full event log.";

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
    let ds = json!({"type": "string", "description": "dataset key: mtgo (default) or ldc"});
    let limit = |d: usize| json!({"type": "integer", "default": d});
    json!([
        {
            "name": "describe",
            "description": "Start here for any MTGO Legacy results question: the datasets, or one dataset's column catalog, default filters and notes.",
            "inputSchema": {"type": "object", "properties": {"ds": ds}}
        },
        {
            "name": "values",
            "description": "Distinct values of one column with their match and game counts, most matches first (at most 500), under optional filters. Use it to find a player's or deck's exact spelling (op contains) before filtering on it.",
            "inputSchema": {"type": "object", "properties": {"ds": ds, "column": {"type": "string"}, "filters": filter_schema()}, "required": ["column"]}
        },
        {
            "name": "stats",
            "description": "Win rates and counts for any question about Legacy results, matchups or a player's record. Results grouped by columns: per group its matches, match wins and decided matches, games, game wins and decided games, first date, and averages (turns, minutes, mulligans) where the dataset has them. `split` filters each add a yes/no grouping. `hist` columns add a value histogram per group. Sort: games (default; chronological for list/era/month/date groups), time, wr, key.",
            "inputSchema": {"type": "object", "properties": {
                "ds": ds,
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
            "description": "mtgo only: the matches the filters select (a match passes if any of its games does), newest first, with the decks, the inference behind the opponent's deck, and a compact per-game summary 'id:game:play|draw:mulligans:turns:winner:how'.",
            "inputSchema": {"type": "object", "properties": {"filters": filter_schema(), "limit": limit(200)}}
        },
        {
            "name": "games",
            "description": "mtgo only: one row per game the filters select, newest first.",
            "inputSchema": {"type": "object", "properties": {"filters": filter_schema(), "limit": limit(500)}}
        },
        {
            "name": "game",
            "description": "mtgo only: one game's cards played per side and its full event log.",
            "inputSchema": {"type": "object", "properties": {"id": {"type": "integer"}}, "required": ["id"]}
        },
        {
            "name": "lists",
            "description": "mtgo only: a player's registered decklists with the reference deck each is nearest to.",
            "inputSchema": {"type": "object", "properties": {"player": {"type": "string", "description": "MTGO username as the logs spell it"}}, "required": ["player"]}
        },
        {
            "name": "list",
            "description": "mtgo only: one registered list's cards and its delta against its reference deck.",
            "inputSchema": {"type": "object", "properties": {"player": {"type": "string"}, "id": {"type": "integer"}}, "required": ["player", "id"]}
        }
    ])
}

/// Prompt templates, exposed by Claude Code as `/mcp__<server>__<name>` slash commands.
fn prompts() -> Value {
    json!([
        {
            "name": "record",
            "description": "A player's Legacy results: overall, by event type, by deck and by opponent archetype, on both datasets",
            "arguments": [{"name": "player", "description": "MTGO username", "required": true}]
        },
        {
            "name": "compare",
            "description": "Two players' Challenge results side by side: record, decks, matchups, trend",
            "arguments": [{"name": "a", "description": "MTGO username", "required": true}, {"name": "b", "description": "MTGO username", "required": true}]
        },
        {
            "name": "matchup",
            "description": "How a deck fares against an opponent deck or archetype, across eras and on both datasets",
            "arguments": [{"name": "deck", "required": true}, {"name": "opponent", "description": "opponent deck or archetype", "required": true}]
        }
    ])
}

fn prompt_text(name: &str, args: &Value) -> Option<String> {
    let g = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("?").to_string();
    Some(match name {
        "record" => format!("Using the mtgctl-web stats tools, summarise {}'s MTGO Legacy results. Find the exact spelling with `values` on `player` first. Give the overall record, by event type, by deck, by opponent archetype, and the trend by month, on the ldc dataset and (if they have uploaded logs) the mtgo dataset. State sample sizes.", g("player")),
        "compare" => format!("Using the mtgctl-web stats tools, compare {} and {} on the ldc dataset: Challenge record, decks played, matchups by opponent archetype and deck, and trend by era and month. Find each spelling with `values` on `player` first. Note where the samples are too small to separate them.", g("a"), g("b")),
        "matchup" => format!("Using the mtgctl-web stats tools, report how {} does against {}: match and game win rates on the ldc dataset (filter `my_deck` or `my_ref_deck`, group by `opp_deck`) and on the mtgo dataset (`my_deck`, `opp_deck`/`opp_archetype`), by era, with sample sizes.", g("deck"), g("opponent")),
        _ => return None,
    })
}

fn arg_str(a: &Value, k: &str) -> Option<String> {
    a.get(k).and_then(|v| v.as_str()).map(str::to_string).filter(|s| !s.is_empty())
}
fn arg_filters(a: &Value, k: &str) -> Result<Vec<Entry>, String> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(vec![]),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("{k}: {e}")),
    }
}
fn arg_strings(a: &Value, k: &str) -> Vec<String> {
    a.get(k).and_then(|v| v.as_array()).map(|xs| xs.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn q(pairs: &[(&str, String)]) -> Query<HashMap<String, String>> {
    Query(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
}

/// A tool's result as JSON, or the message of the error the caller sees.
async fn call(st: &Shared, name: &str, a: &Value) -> Result<Value, String> {
    let api = |e: ApiError| e.1;
    let ds_key = arg_str(a, "ds");
    let ds = dataset(ds_key.as_deref()).map_err(api)?;
    let wher = |by: &[String]| filter::where_sql(ds, &arg_filters(a, "filters")?, by);
    let mtgo_only = || if ds.features.rows { Ok(()) } else { Err(format!("{} is only for the mtgo dataset", name)) };
    match name {
        "describe" => {
            if ds_key.is_none() && a.get("ds").is_none() {
                return Ok(json!(dataset::ALL.iter().map(|d| json!({"key": d.key, "title": d.title, "about": d.about})).collect::<Vec<_>>()));
            }
            let s = schema(State(st.clone()), q(&[("ds", ds.key.into())])).await.map_err(api)?.0;
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
            let mut params = vec![("ds", ds.key.to_string()), ("col", col)];
            if let Some(w) = wher(&[])? {
                params.push(("where", w));
            }
            Ok(serde_json::to_value(values(State(st.clone()), q(&params)).await.map_err(api)?.0).unwrap())
        }
        "stats" => {
            let by = arg_strings(a, "by");
            let split: Vec<String> = arg_filters(a, "split")?.iter().map(|f| filter::where_sql(ds, std::slice::from_ref(f), &[]).map(|w| w.unwrap_or_else(|| "1".into()))).collect::<Result<_, _>>()?;
            let p = StatsParams {
                by: by.join(","),
                where_sql: wher(&by)?,
                min: a.get("min_games").and_then(Value::as_i64).unwrap_or(1),
                limit: a.get("limit").and_then(Value::as_u64).unwrap_or(60) as usize,
                sort: arg_str(a, "sort").unwrap_or_else(|| "games".into()),
                full: false,
                hist: arg_strings(a, "hist").join(","),
                split: serde_json::to_string(&split).unwrap(),
                game: a.get("game").and_then(Value::as_bool),
                ds: Some(ds.key.into()),
            };
            Ok(serde_json::to_value(stats_api(State(st.clone()), Query(p)).await.map_err(api)?.0).unwrap())
        }
        "matches" | "games" => {
            mtgo_only()?;
            let p = MatchesParams { where_sql: wher(&[])?, limit: a.get("limit").and_then(Value::as_u64).unwrap_or(if name == "matches" { 200 } else { 500 }) as usize };
            Ok(if name == "matches" {
                serde_json::to_value(matches_api(State(st.clone()), Query(p)).await.map_err(api)?.0).unwrap()
            } else {
                serde_json::to_value(games_api(State(st.clone()), Query(p)).await.map_err(api)?.0).unwrap()
            })
        }
        "game" => {
            mtgo_only()?;
            let id = a.get("id").and_then(Value::as_i64).ok_or("id required")?;
            Ok(serde_json::to_value(game_detail(State(st.clone()), q(&[("id", id.to_string())])).await.map_err(api)?.0).unwrap())
        }
        "lists" | "list" => {
            mtgo_only()?;
            let player = arg_str(a, "player").ok_or("player required")?;
            let mut params = vec![("player", player)];
            if name == "list" {
                params.push(("id", a.get("id").and_then(Value::as_i64).ok_or("id required")?.to_string()));
                Ok(serde_json::to_value(list_detail(State(st.clone()), HeaderMap::new(), q(&params)).await.map_err(api)?.0).unwrap())
            } else {
                Ok(serde_json::to_value(lists(State(st.clone()), HeaderMap::new(), q(&params)).await.map_err(api)?.0).unwrap())
            }
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

fn rpc_error(id: Value, code: i64, msg: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg.into()}})
}

/// One request's response; `None` for a notification.
async fn handle(st: &Shared, msg: &Value) -> Option<Value> {
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
                "serverInfo": {"name": "mtgctl-web", "version": env!("CARGO_PKG_VERSION")},
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
            match call(st, name, &args).await {
                Ok(v) => json!({"content": [{"type": "text", "text": serde_json::to_string(&v).unwrap()}], "structuredContent": if v.is_object() { v } else { json!({"rows": v}) }}),
                Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
            }
        }
        _ => return Some(rpc_error(id, -32601, format!("method not found: {method}"))),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

pub async fn mcp(State(st): State<Shared>, body: Bytes) -> Response {
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(rpc_error(Value::Null, -32700, format!("parse error: {e}")))).into_response(),
    };
    let out = match &msg {
        Value::Array(batch) => {
            let mut out = vec![];
            for m in batch {
                out.extend(handle(&st, m).await);
            }
            if out.is_empty() { None } else { Some(Value::Array(out)) }
        }
        m => handle(&st, m).await,
    };
    match out {
        Some(v) => Json(v).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

pub async fn mcp_get() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, "MCP over streamable HTTP: POST JSON-RPC here; this server opens no stream").into_response()
}
