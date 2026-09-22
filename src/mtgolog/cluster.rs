//! Unsupervised deck-archetype clustering from card signatures.
//!
//! Every (match, side) is a document: the cards that side cast/played or otherwise exposed,
//! weighted by the number of copies observed. Documents are TF-IDF weighted, L2-normalised,
//! and clustered by cosine distance with average-linkage agglomerative clustering
//! (nearest-neighbour-chain, O(n²)); the dendrogram is cut at `--threshold`.
//! Both my decks and opponents' decks live in one space, so cluster ids are comparable.

use super::db::{mtgo_clusters, mtgo_game_cards, mtgo_matches, mtgo_variants};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Table};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone)]
pub struct Doc {
    pub match_id: i32,
    pub side: String,
    /// card name → weight (copies observed, capped at 4)
    pub cards: BTreeMap<String, f32>,
}

/// Tokens are derived objects (Clue/Monk/Construct…), not deck cards.
fn is_token(name: &str) -> bool {
    name.ends_with(" Token") || name.ends_with(" Tokens") || name == "Marit Lage" || name.contains("Token ")
}

/// Back face → front face for double-faced cards, learned from "X transforms into Y" log lines.
pub fn dfc_map(conn: &mut SqliteConnection) -> QueryResult<HashMap<String, String>> {
    use diesel::sql_types::Text;
    #[derive(QueryableByName)]
    struct T {
        #[diesel(sql_type = Text)]
        text: String,
    }
    let rows: Vec<T> = diesel::sql_query("SELECT DISTINCT text FROM mtgo_events WHERE kind = 'Other' AND text LIKE '% transforms into %'").load(conn)?;
    let mut m = HashMap::new();
    for r in rows {
        let refs = super::event::card_refs(&r.text);
        if refs.len() >= 2 && refs[0].name != refs[1].name {
            m.insert(refs[1].name.clone(), refs[0].name.clone());
        }
    }
    // resolve chains (A→B→C) to the front face
    let keys: Vec<String> = m.keys().cloned().collect();
    for k in keys {
        let mut v = m[&k].clone();
        let mut hops = 0;
        while let Some(next) = m.get(&v) {
            if hops > 5 || *next == k { break; }
            v = next.clone();
            hops += 1;
        }
        m.insert(k, v);
    }
    Ok(m)
}

/// One document per (match, side): the cards that side played or exposed, weighted by copies
/// observed (capped at 4). Tokens are dropped; DFC back faces fold into their front face.
pub fn load_docs(conn: &mut SqliteConnection, played_only: bool) -> QueryResult<(Vec<Doc>, HashMap<i32, String>)> {
    let dfc = dfc_map(conn)?;
    let rows: Vec<(i32, String, String, i32, bool, Option<String>)> = mtgo_game_cards::table
        .inner_join(mtgo_matches::table.on(mtgo_matches::id.eq(mtgo_game_cards::match_id)))
        .filter(mtgo_matches::replay.eq(false))
        .select((mtgo_game_cards::match_id, mtgo_game_cards::side, mtgo_game_cards::name, mtgo_game_cards::instances, mtgo_game_cards::played, mtgo_matches::start_ts))
        .load(conn)?;
    let mut docs: BTreeMap<(i32, String), BTreeMap<String, f32>> = BTreeMap::new();
    let mut match_day: HashMap<i32, String> = HashMap::new();
    for (mid, side, name, inst, played, start) in rows {
        if played_only && !played {
            continue;
        }
        if is_token(&name) {
            continue;
        }
        let name = dfc.get(&name).cloned().unwrap_or(name);
        match_day.entry(mid).or_insert_with(|| start.as_deref().map(|s| s[..10.min(s.len())].to_string()).unwrap_or_default());
        let w = (inst.max(1) as f32).min(4.0);
        let e = docs.entry((mid, side)).or_default().entry(name).or_insert(0.0);
        *e = e.max(w);
    }
    Ok((docs.into_iter().map(|((match_id, side), cards)| Doc { match_id, side, cards }).collect(), match_day))
}

/// Sparse TF-IDF vectors (sorted by term id) with unit L2 norm.
pub struct Vectors {
    pub terms: Vec<String>,
    pub term_id: HashMap<String, u32>,
    pub vecs: Vec<Vec<(u32, f32)>>,
    pub idf: Vec<f32>,
}

impl Vectors {
    /// Unit vector with each card weighted by `weight × idf` — the presence scale used by the
    /// labeller (a card present in a signature weighs exactly its idf).
    pub fn vector_for_weights(&self, cards: &BTreeMap<String, f32>) -> Vec<(u32, f32)> {
        let mut v: Vec<(u32, f32)> = cards.iter().filter_map(|(name, w)| self.term_id.get(name).map(|&id| (id, w * self.idf[id as usize]))).collect();
        v.sort_by_key(|(id, _)| *id);
        let norm = v.iter().map(|(_, x)| x * x).sum::<f32>().sqrt().max(1e-9);
        for (_, x) in v.iter_mut() {
            *x /= norm;
        }
        v
    }

    /// TF-IDF unit vector for an arbitrary card map, using this corpus' vocabulary/idf
    /// (cards outside the vocabulary are ignored).
    pub fn vector_for(&self, cards: &BTreeMap<String, f32>) -> Vec<(u32, f32)> {
        let mut v: Vec<(u32, f32)> = cards
            .iter()
            .filter_map(|(name, w)| self.term_id.get(name).map(|&id| (id, (1.0 + w).ln() * self.idf[id as usize])))
            .collect();
        v.sort_by_key(|(id, _)| *id);
        let norm = v.iter().map(|(_, x)| x * x).sum::<f32>().sqrt().max(1e-9);
        for (_, x) in v.iter_mut() {
            *x /= norm;
        }
        v
    }
}

pub fn vectorize(docs: &[Doc]) -> Vectors {
    vectorize_with(docs, false)
}

/// `presence`: every observed card weighs 1 (times idf) instead of ln(1+copies) — copies seen are
/// noise for identification, and presence scored better in the labeller's leave-one-out check.
pub fn vectorize_with(docs: &[Doc], presence: bool) -> Vectors {
    let mut term_id: HashMap<String, u32> = HashMap::new();
    let mut terms: Vec<String> = Vec::new();
    let mut df: Vec<u32> = Vec::new();
    for d in docs {
        for name in d.cards.keys() {
            let id = *term_id.entry(name.clone()).or_insert_with(|| {
                terms.push(name.clone());
                df.push(0);
                (terms.len() - 1) as u32
            });
            df[id as usize] += 1;
        }
    }
    let n = docs.len() as f32;
    // Cap rarity: a card seen in fewer than IDF_CAP_DF signatures weighs no more than one seen in
    // IDF_CAP_DF — otherwise one rare shared one-of (e.g. Deafening Silence, df=2) dominates the
    // similarity of small observed-card signatures and merges unrelated decks.
    const IDF_CAP_DF: f32 = 5.0;
    let idf: Vec<f32> = df.iter().map(|&d| (n / (d as f32).max(IDF_CAP_DF)).ln() + 1.0).collect();
    let vecs = docs
        .iter()
        .map(|d| {
            let mut v: Vec<(u32, f32)> = d.cards.iter().map(|(name, w)| {
                let id = term_id[name];
                (id, if presence { idf[id as usize] } else { (1.0 + w).ln() * idf[id as usize] })
            }).collect();
            v.sort_by_key(|(id, _)| *id);
            let norm = v.iter().map(|(_, x)| x * x).sum::<f32>().sqrt().max(1e-9);
            for (_, x) in v.iter_mut() {
                *x /= norm;
            }
            v
        })
        .collect();
    Vectors { terms, term_id, vecs, idf }
}

pub(crate) fn dot(a: &[(u32, f32)], b: &[(u32, f32)]) -> f32 {
    let (mut i, mut j, mut s) = (0, 0, 0.0);
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                s += a[i].1 * b[j].1;
                i += 1;
                j += 1;
            }
        }
    }
    s
}

/// Average-linkage agglomerative clustering (UPGMA) via the nearest-neighbour chain algorithm.
/// Returns merges (a, b, distance) — unordered w.r.t. distance; sort before cutting.
pub fn upgma(vecs: &[Vec<(u32, f32)>]) -> Vec<(usize, usize, f32)> {
    let n = vecs.len();
    if n < 2 {
        return Vec::new();
    }
    // Full distance matrix (n² f32).
    let mut d = vec![0f32; n * n];
    for i in 0..n {
        for j in (i + 1)..n {
            let dist = (1.0 - dot(&vecs[i], &vecs[j])).clamp(0.0, 1.0);
            d[i * n + j] = dist;
            d[j * n + i] = dist;
        }
    }
    let mut active = vec![true; n];
    let mut size = vec![1usize; n];
    let mut merges = Vec::with_capacity(n - 1);
    let mut chain: Vec<usize> = Vec::new();
    let mut remaining = n;
    while remaining > 1 {
        if chain.is_empty() {
            chain.push((0..n).find(|&i| active[i]).unwrap());
        }
        loop {
            let a = *chain.last().unwrap();
            // nearest active neighbour of a (ties → prefer the previous chain element)
            let prev = if chain.len() >= 2 { Some(chain[chain.len() - 2]) } else { None };
            let mut best = usize::MAX;
            let mut best_d = f32::INFINITY;
            for c in 0..n {
                if c == a || !active[c] {
                    continue;
                }
                let dc = d[a * n + c];
                if dc < best_d || (dc == best_d && Some(c) == prev) {
                    best_d = dc;
                    best = c;
                }
            }
            if Some(best) == prev {
                // reciprocal nearest neighbours → merge a and best
                let b = best;
                chain.pop();
                chain.pop();
                let (sa, sb) = (size[a] as f32, size[b] as f32);
                for k in 0..n {
                    if k == a || k == b || !active[k] {
                        continue;
                    }
                    let nd = (sa * d[a * n + k] + sb * d[b * n + k]) / (sa + sb);
                    d[a * n + k] = nd;
                    d[k * n + a] = nd;
                }
                size[a] += size[b];
                active[b] = false;
                remaining -= 1;
                merges.push((a, b, best_d));
                break;
            } else {
                chain.push(best);
            }
        }
    }
    merges
}

/// Cut the dendrogram: union all merges with distance <= threshold. Returns label per doc.
pub fn cut(n: usize, merges: &[(usize, usize, f32)], threshold: f32) -> Vec<usize> {
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut Vec<usize>, x: usize) -> usize {
        let mut r = x;
        while p[r] != r {
            r = p[r];
        }
        let mut c = x;
        while p[c] != r {
            let nx = p[c];
            p[c] = r;
            c = nx;
        }
        r
    }
    let mut sorted: Vec<_> = merges.to_vec();
    sorted.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());
    for (a, b, dist) in sorted {
        if dist > threshold {
            break;
        }
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[rb] = ra;
        }
    }
    (0..n).map(|i| find(&mut parent, i)).collect()
}

pub struct ClusterSummary {
    pub members: Vec<usize>,
    /// (card, fraction of members containing it, characteristic score = presence × idf)
    pub top_cards: Vec<(String, f32, f32)>,
}

/// Lands as observed in the logs (cards that were ever played as a land drop).
pub fn land_set(conn: &mut SqliteConnection) -> QueryResult<std::collections::HashSet<String>> {
    use diesel::sql_types::Text;
    #[derive(QueryableByName)]
    struct C {
        #[diesel(sql_type = Text)]
        card: String,
    }
    let rows: Vec<C> = diesel::sql_query("SELECT DISTINCT card FROM mtgo_events WHERE kind = 'PlaysLand' AND card IS NOT NULL").load(conn)?;
    Ok(rows.into_iter().map(|c| c.card).collect())
}

/// Cluster name from its characteristic cards.
/// `top2`: the two highest-scoring cards. `cutoff`: every card scoring ≥ 55% of the best (2–4 cards),
/// plus the best-scoring land (presence ≥ 50%) if none made it — lands tell colours apart.
pub fn name_cluster(top: &[(String, f32, f32)], lands: &std::collections::HashSet<String>, mode: &str) -> String {
    if top.is_empty() {
        return String::new();
    }
    if mode == "top2" {
        return top.iter().take(2).map(|(c, _, _)| c.as_str()).collect::<Vec<_>>().join(" + ");
    }
    let best = top[0].2;
    let mut picked: Vec<&str> = top.iter().enumerate().filter(|(i, (_, _, sc))| *i < 2 || (*sc >= 0.55 * best && *i < 4)).map(|(_, (c, _, _))| c.as_str()).collect();
    if !picked.iter().any(|c| lands.contains(*c)) {
        if let Some((l, _, _)) = top.iter().find(|(c, fv, _)| lands.contains(c) && *fv >= 0.5) {
            if picked.len() >= 4 {
                picked.pop();
            }
            picked.push(l.as_str());
        }
    }
    picked.join(" + ")
}

pub fn summarize(docs: &[Doc], v: &Vectors, labels: &[usize]) -> Vec<ClusterSummary> {
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, &l) in labels.iter().enumerate() {
        if l == usize::MAX {
            continue; // unclustered (sparse signature with no close established cluster)
        }
        groups.entry(l).or_default().push(i);
    }
    let mut out: Vec<ClusterSummary> = groups
        .into_values()
        .map(|members| {
            let mut presence: HashMap<u32, f32> = HashMap::new();
            for &m in &members {
                for (id, _) in &v.vecs[m] {
                    *presence.entry(*id).or_default() += 1.0;
                }
            }
            let n = members.len() as f32;
            // characteristic = presence × idf (rare-but-consistent cards name the deck)
            let mut scored: Vec<(u32, f32, f32)> = presence.into_iter().map(|(id, c)| (id, c / n, (c / n) * v.idf[id as usize])).collect();
            scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
            let top_cards = scored.into_iter().take(10).map(|(id, frac, sc)| (v.terms[id as usize].clone(), frac, sc)).collect();
            ClusterSummary { members, top_cards }
        })
        .collect();
    out.sort_by_key(|c| std::cmp::Reverse(c.members.len()));
    let _ = docs;
    out
}

#[derive(Insertable)]
#[diesel(table_name = mtgo_clusters)]
struct NewCluster {
    size: i32,
    name: String,
    top_cards: String,
    threshold: f64,
    created_at: String,
}

#[derive(Insertable)]
#[diesel(table_name = mtgo_variants)]
struct NewVariant {
    cluster_id: i32,
    size: i32,
    name: String,
    top_cards: String,
    threshold: f64,
}

pub fn run_cluster(conn: &mut SqliteConnection, threshold: f32, variant_threshold: f32, played_only: bool, me_per_match: bool, sweep: bool, min_show: usize, naming: &str, compare_names: bool) -> QueryResult<()> {
    super::db::ensure_schema(conn)?;
    let lands = land_set(conn)?;
    let (docs, match_day) = load_docs(conn, played_only)?;
    if docs.is_empty() {
        eprintln!("no imported matches; run `mtgctl mtgo import` first");
        return Ok(());
    }
    let v = vectorize(&docs);
    eprintln!("{} deck signatures, {} distinct cards; clustering…", docs.len(), v.terms.len());
    let merges = upgma(&v.vecs);

    if sweep {
        println!("threshold  clusters  ≥5 members  largest");
        for t in [0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8] {
            let labels = cut(docs.len(), &merges, t);
            let s = summarize(&docs, &v, &labels);
            let big = s.iter().filter(|c| c.members.len() >= 5).count();
            println!("{t:9.2}  {:8}  {big:10}  {:7}", s.len(), s[0].members.len());
        }
        return Ok(());
    }

    let mut labels = cut(docs.len(), &merges, threshold);

    // Sparse signatures (few cards observed — typically short games) cannot discriminate archetypes
    // and would form or join tiny junk clusters. Cluster on rich signatures only, then attach each
    // sparse one to the nearest established cluster (≥ MIN_CLUSTER rich members) by centroid
    // cosine, or leave it unclustered (label = usize::MAX) if nothing is within SPARSE_MAX_DIST.
    const SPARSE_CARDS: usize = 10;
    const MIN_CLUSTER: usize = 5;
    const SPARSE_MAX_DIST: f32 = 0.75;
    let sparse: Vec<bool> = docs.iter().map(|d| d.cards.len() < SPARSE_CARDS).collect();
    // rich-member counts per label
    let mut rich_count: HashMap<usize, usize> = HashMap::new();
    for (i, &l) in labels.iter().enumerate() {
        if !sparse[i] {
            *rich_count.entry(l).or_default() += 1;
        }
    }
    // centroids of established clusters (rich members only), unit-normalised
    let mut centroids: HashMap<usize, Vec<(u32, f32)>> = HashMap::new();
    for (&l, &c) in &rich_count {
        if c < MIN_CLUSTER {
            continue;
        }
        let mut acc: BTreeMap<u32, f32> = BTreeMap::new();
        for (i, &li) in labels.iter().enumerate() {
            if li == l && !sparse[i] {
                for (id, w) in &v.vecs[i] {
                    *acc.entry(*id).or_default() += *w;
                }
            }
        }
        let norm = acc.values().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        centroids.insert(l, acc.into_iter().map(|(id, w)| (id, w / norm)).collect());
    }
    let mut reattached = 0;
    let mut unclustered = 0;
    for i in 0..docs.len() {
        if !sparse[i] {
            continue;
        }
        let mut best: Option<(usize, f32)> = None;
        for (&l, cen) in &centroids {
            let d = 1.0 - dot(&v.vecs[i], cen);
            if best.map_or(true, |(_, bd)| d < bd) {
                best = Some((l, d));
            }
        }
        match best {
            Some((l, d)) if d <= SPARSE_MAX_DIST => {
                labels[i] = l;
                reattached += 1;
            }
            _ => {
                labels[i] = usize::MAX;
                unclustered += 1;
            }
        }
    }
    // Clusters that were only ever sparse signatures dissolve into "unclustered" too.
    for l in labels.iter_mut() {
        if *l != usize::MAX && rich_count.get(l).copied().unwrap_or(0) == 0 {
            *l = usize::MAX;
            unclustered += 1;
        }
    }
    eprintln!("{} sparse signatures (<{SPARSE_CARDS} cards): {reattached} attached to the nearest established cluster, {unclustered} left unclustered", sparse.iter().filter(|s| **s).count());
    let summaries = summarize(&docs, &v, &labels);

    // Match results for win-rate columns.
    let results: HashMap<i32, String> = mtgo_matches::table.select((mtgo_matches::id, mtgo_matches::result)).load::<(i32, String)>(conn)?.into_iter().collect();

    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut chimera_names = 0usize;
    conn.transaction::<_, diesel::result::Error, _>(|conn| {
        super::db::truncate(conn, "mtgo_clusters")?;
        super::db::truncate(conn, "mtgo_variants")?;
        diesel::update(mtgo_matches::table)
            .set((
                mtgo_matches::my_cluster.eq::<Option<i32>>(None),
                mtgo_matches::opp_cluster.eq::<Option<i32>>(None),
                mtgo_matches::my_variant.eq::<Option<i32>>(None),
                mtgo_matches::opp_variant.eq::<Option<i32>>(None),
            ))
            .execute(conn)?;
        let mut t = Table::new();
        t.load_preset(UTF8_FULL_CONDENSED);
        t.set_header(["id", "size", "mine", "my WR", "vs", "WR vs", "characteristic cards (presence)"]);
        for s in &summaries {
            let top: Vec<String> = s.top_cards.iter().take(8).map(|(c, f, _)| format!("{c} {:.0}%", f * 100.0)).collect();
            // Auto-name from the most characteristic cards. Override with `mtgctl mtgo name <id> <name>`.
            // Naming invariant: the named cards must co-occur in at least one member's signature;
            // otherwise drop the weakest named card until they do (never below 2).
            let mut name = name_cluster(&s.top_cards, &lands, naming);
            let cooccur = |name: &str| -> bool {
                let parts: Vec<&str> = name.split(" + ").collect();
                s.members.iter().any(|&m| parts.iter().all(|c| docs[m].cards.contains_key(*c)))
            };
            while !cooccur(&name) && name.matches(" + ").count() >= 2 {
                let mut parts: Vec<&str> = name.split(" + ").collect();
                // drop the lowest-scoring card among the named ones
                let score = |c: &str| s.top_cards.iter().find(|(n, _, _)| n == c).map(|(_, _, sc)| *sc).unwrap_or(0.0);
                let (idx, _) = parts.iter().enumerate().min_by(|a, b| score(a.1).partial_cmp(&score(b.1)).unwrap()).unwrap();
                parts.remove(idx);
                name = parts.join(" + ");
            }
            if !cooccur(&name) {
                chimera_names += 1;
            }
            if compare_names && s.members.len() >= min_show {
                println!("{:4}  top2: {:<55} cutoff: {}", s.members.len(), name_cluster(&s.top_cards, &lands, "top2"), name_cluster(&s.top_cards, &lands, "cutoff"));
            }
            diesel::insert_into(mtgo_clusters::table)
                .values(NewCluster { size: s.members.len() as i32, name, top_cards: top.join(", "), threshold: threshold as f64, created_at: now.clone() })
                .execute(conn)?;
            let cid: i32 = mtgo_clusters::table.order(mtgo_clusters::id.desc()).select(mtgo_clusters::id).first(conn)?;
            let (mut mine, mut my_w, mut my_n, mut vs, mut vs_w, mut vs_n) = (0, 0, 0, 0, 0, 0);
            for &m in &s.members {
                let d = &docs[m];
                let r = results.get(&d.match_id).map(String::as_str).unwrap_or("?");
                if d.side == "me" {
                    diesel::update(mtgo_matches::table.filter(mtgo_matches::id.eq(d.match_id))).set(mtgo_matches::my_cluster.eq(cid)).execute(conn)?;
                    mine += 1;
                    if r == "W" || r == "L" {
                        my_n += 1;
                        my_w += (r == "W") as i32;
                    }
                } else {
                    diesel::update(mtgo_matches::table.filter(mtgo_matches::id.eq(d.match_id))).set(mtgo_matches::opp_cluster.eq(cid)).execute(conn)?;
                    vs += 1;
                    if r == "W" || r == "L" {
                        vs_n += 1;
                        vs_w += (r == "W") as i32;
                    }
                }
            }
            if s.members.len() >= min_show {
                let pct = |w: i32, n: i32| if n > 0 { format!("{:.0}%", 100.0 * w as f32 / n as f32) } else { "".into() };
                t.add_row(vec![cid.to_string(), s.members.len().to_string(), mine.to_string(), pct(my_w, my_n), vs.to_string(), pct(vs_w, vs_n), top.join(", ")]);
            }

            // Variants: sub-cluster this cluster's members at the tighter threshold, name each
            // variant by the cards that set it apart from the rest of the cluster.
            // My side is merged per (day, cluster): one registered list per session, so the
            // union of that day's matches approximates the list instead of what I drew.
            if s.members.len() >= 2 {
                // units: Vec<(vector, member indices)>
                let mut units: Vec<(Vec<(u32, f32)>, Vec<usize>)> = Vec::new();
                let mut my_days: BTreeMap<String, (BTreeMap<String, f32>, Vec<usize>)> = BTreeMap::new();
                for &m in &s.members {
                    let d = &docs[m];
                    if d.side == "me" && !me_per_match {
                        let day = match_day.get(&d.match_id).cloned().unwrap_or_default();
                        let e = my_days.entry(day).or_default();
                        for (c, w) in &d.cards {
                            let x = e.0.entry(c.clone()).or_insert(0.0);
                            *x = x.max(*w);
                        }
                        e.1.push(m);
                    } else {
                        units.push((v.vecs[m].clone(), vec![m]));
                    }
                }
                for (_, (cards, members)) in my_days {
                    units.push((v.vector_for(&cards), members));
                }
                let unit_vecs: Vec<Vec<(u32, f32)>> = units.iter().map(|(vec, _)| vec.clone()).collect();
                let sub = cut(unit_vecs.len(), &upgma(&unit_vecs), variant_threshold);
                let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
                for (i, &l) in sub.iter().enumerate() {
                    groups.entry(l).or_default().extend(units[i].1.iter().copied());
                }
                let mut gs: Vec<Vec<usize>> = groups.into_values().collect();
                gs.sort_by_key(|g| std::cmp::Reverse(g.len()));
                let single = gs.len() == 1;
                for g in gs {
                    let d = distinguishing_cards(&v, &s.members, &g, 6);
                    let name = if single {
                        "(no variants)".to_string()
                    } else {
                        d.iter().take(2).map(|(c, _, _)| c.as_str()).collect::<Vec<_>>().join(" + ")
                    };
                    let top: Vec<String> = d.iter().map(|(c, fv, dd)| format!("{c} {:.0}% (+{:.0})", fv * 100.0, dd * 100.0)).collect();
                    diesel::insert_into(mtgo_variants::table)
                        .values(NewVariant { cluster_id: cid, size: g.len() as i32, name, top_cards: top.join(", "), threshold: variant_threshold as f64 })
                        .execute(conn)?;
                    let vid: i32 = mtgo_variants::table.order(mtgo_variants::id.desc()).select(mtgo_variants::id).first(conn)?;
                    for &m in &g {
                        let d = &docs[m];
                        if d.side == "me" {
                            diesel::update(mtgo_matches::table.filter(mtgo_matches::id.eq(d.match_id))).set(mtgo_matches::my_variant.eq(vid)).execute(conn)?;
                        } else {
                            diesel::update(mtgo_matches::table.filter(mtgo_matches::id.eq(d.match_id))).set(mtgo_matches::opp_variant.eq(vid)).execute(conn)?;
                        }
                    }
                }
            }
        }
        println!("{t}");
        Ok(())
    })?;
    println!("naming lint: {chimera_names} cluster names whose cards never co-occur in a member (should be 0)");
    println!("{} clusters at cosine-distance threshold {threshold}; variants at {variant_threshold} (mtgo_clusters / mtgo_variants; my_cards/opp_cards + my_cards_variant/opp_cards_variant in mtgo_game_view)", summaries.len());
    println!("{}", super::stats::SIGNATURE_NOTE);
    Ok(())
}

/// Sub-cluster the members of one cluster at a tighter threshold. Returns labels (local ids).
pub fn subcluster(v: &Vectors, members: &[usize], threshold: f32) -> Vec<usize> {
    let vecs: Vec<Vec<(u32, f32)>> = members.iter().map(|&m| v.vecs[m].clone()).collect();
    let merges = upgma(&vecs);
    cut(vecs.len(), &merges, threshold)
}

/// Cards that distinguish a variant from the rest of its parent cluster:
/// score = presence_in_variant − presence_in_parent (both as fractions), top `n`.
pub fn distinguishing_cards(v: &Vectors, parent: &[usize], variant: &[usize], n: usize) -> Vec<(String, f32, f32)> {
    let presence = |members: &[usize]| -> HashMap<u32, f32> {
        let mut p: HashMap<u32, f32> = HashMap::new();
        for &m in members {
            for (id, _) in &v.vecs[m] {
                *p.entry(*id).or_default() += 1.0 / members.len() as f32;
            }
        }
        p
    };
    let (pp, pv) = (presence(parent), presence(variant));
    let mut scored: Vec<(u32, f32, f32)> = pv.iter().map(|(id, &fv)| (*id, fv, fv - pp.get(id).copied().unwrap_or(0.0))).collect();
    scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
    scored.into_iter().take(n).map(|(id, fv, d)| (v.terms[id as usize].clone(), fv, d)).collect()
}

