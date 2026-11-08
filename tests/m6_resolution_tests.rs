//! Probe P-8 of [Amendment 2](../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md):
//! the §5.3 resolution rule, checked by generation before any reader converts
//! (M6-b). Revision 3 of that amendment states the rule as this file tests
//! it: its first round ran against revision 2's nearest-row-wins SQL and
//! reds on the resurrection hole (branch supersedes the trunk at `t`, later
//! retires its own version, the trunk text comes back), which is what
//! switched the design to a stored cut (§5.1's `concept_cuts`); the SQL in
//! this file is the post-amendment candidate, and every run replays the
//! regression history in the proptest file.
//!
//! # The rule under test (Amendment 2 §5.3, revision 3)
//!
//! For a reader on lineage `L` at recorded cutoff `r` and valid instant `v`:
//! walk the path from `L` to the root. Each ancestor contributes only rows
//! recorded at or before the fork cutoffs crossed so far, further capped by
//! `r` (today's D-223 rule). The first lineage on the path that *decides* at
//! `v` answers, and the walk stops:
//!
//! * a recorded-visible row of that lineage covering `v` (its own version, or
//!   the mint row at the mint lineage) resolves to that belief — and **a
//!   version wins over a cut where both cover `v`**, because a cut on its own
//!   means *nothing is here*, and the covering version is what is here;
//! * else, if a cut row of this lineage covers `v` (`cut_at ≤ v`),
//!   *absent* — the invented belief is hidden from `cut_at` on, permanently:
//!   whatever the lineage's own versions do afterwards, the walk never falls
//!   past a visible cut. Supersede writes the cut and its version together
//!   (§5.2, both at `valid_from`); the version's own later retirement then
//!   cannot revive the inherited text, because the cut is still there.
//!
//! Two consequences the generator is built to catch:
//!
//! * **No resurrection** (the new §5.5 red line): after a supersede-then-retire,
//!   the inherited belief must not come back. The generator lays
//!   supersede-then-retire shapes deliberately; against the revision-2 SQL,
//!   this file's first round reds on exactly that shape.
//! * **Monotonicity** — writes to an ancestor recorded after the reader's
//!   cutoff change nothing the reader resolves, versions and cuts both. The
//!   same census is resolved before and after the late rows are added.
//!
//! # Scope, deliberately narrower than the engine
//!
//! This file tests the **rule**, not M6-b's fold: the generator keeps every
//! `(lineage, id)` version list disjoint and ascending, so one lineage has at
//! most one version covering any instant and D-303's within-lineage
//! resolution is vacuous here (that contract has its own machinery and
//! suite, which M6-a inherits). The mint lineage holds exactly one era — the
//! row the projection carries today; same-lineage supersede of a mint is
//! D-312's built contract, not this file's. No `transaction_log`, FTS or
//! vector tables: those are P-3/P-4's.
//!
//! # What this file's SQL is, and what M6-b must not take from it
//!
//! The candidate SQL restates the fork-cutoff walk that
//! `src/graph/lineage.rs`'s `ancestry_cte` spells (the D-259 oracle) and adds
//! the nearness layer. It is a restatement on fixture tables, not production
//! code: the engine's real as-of readers rebuild from the log (D-303's
//! fold), because today's tables no longer hold rows superseded later. The
//! durable asset of this probe is the **brute-force oracle**;
//! `the_rust_walk_agrees_with_the_cte` keeps it worth the differential.
//! M6-b's conversion runs the actual readers, through the public API,
//! against this file's oracle — not against this private SQL.
//!
//! # Harness
//!
//! One runtime for the whole binary, serial execution, 256 cases. The
//! one-runtime and serial discipline is the R15 mitigation
//! `doctrine_property_tests.rs` documents; the case count came up from 32
//! because R15's crash treatment no longer reproduces on libSQL 0.10.0 and
//! the larger sweep is what finds the resurrection shapes quickly. Runs
//! under `cargo test --features property-tests --test m6_resolution_tests`:
//! every libSQL STATUS_ACCESS_VIOLATION here is the crash R15 retries for,
//! not a regression. Failing case bundles land in
//! `tests/m6_resolution_cases/`; a genuine rule finding graduates into an
//! unconditional test like `the_superseded_inherited_belief_does_not_come_
//! back` in this file, so a fixed hole never reopens silently.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;

use proptest::prelude::*;
use tokio::runtime::Runtime;

// ---------------------------------------------------------------------------
// Harness: one runtime, serial, small databases (R15 dharma)
// ---------------------------------------------------------------------------

static RT: std::sync::OnceLock<Runtime> = std::sync::OnceLock::new();

fn block_on<F: Future>(f: F) -> F::Output {
    RT.get_or_init(|| Runtime::new().expect("a runtime for the whole binary"))
        .block_on(f)
}

// ---------------------------------------------------------------------------
// The generated model
// ---------------------------------------------------------------------------

/// Valid instants live on a grid of 12 slots; stamps render so that
/// lexicographic order == slot order. Recorded stamps draw from the same
/// grid, with two sentinels outside it.
const GRID: usize = 12;

fn stamp(slot: usize) -> String {
    format!("2026-01-{:02}T00:00:00.000000Z", slot + 1)
}

/// Slot index of an open sentinel; the reader instant `r` uses it for "now".
const OPEN_R: usize = usize::MAX;
/// A recorded stamp larger than every fork cutoff but smaller than the open
/// sentinel: the "later ancestor write" monotonicity is about.
const LATE_STAMP: &str = "2999-12-31T23:59:59.999999Z";
/// The open sentinel as the fold's terminals spell it.
const FOREVER_STAMP: &str = "9999-12-31T23:59:59.999999Z";

#[derive(Debug, Clone, Copy)]
struct Row {
    start: usize,
    end: usize,
    recorded: usize,
}

/// A `concept_cuts` row. Open-ended: from `cut_at` on, the fall-through past
/// this lineage stops, permanently (§5.1, §5.3 of the amendment).
#[derive(Debug, Clone, Copy)]
struct CutRow {
    cut_at: usize,
    recorded: usize,
}

#[derive(Debug, Clone)]
struct Node {
    name: &'static str,
    parent: Option<&'static str>,
    /// The recorded cutoff at which this lineage stopped seeing its parent.
    forked_at: Option<usize>,
}

#[derive(Debug, Clone)]
struct Bundle {
    nodes: Vec<Node>,
    /// The mint lineage of each id; its window is the concepts row.
    mint_of: BTreeMap<&'static str, &'static str>,
    /// Version rows per `(lineage, id)`, the mint's included at its own key
    /// (exactly one row; the pour routes it to `concepts`, everyone else's
    /// to `concept_versions`).
    sheets: BTreeMap<(&'static str, &'static str), Vec<Row>>,
    /// Cut rows per `(lineage, id)` (the mint's included: it may retire its
    /// own era or reject an inherited one, like any lineage).
    cuts: BTreeMap<(&'static str, &'static str), Vec<CutRow>>,
    viewer: &'static str,
}

/// Node names the generator may mint on. The viewer is chosen from all of
/// the nodes, so the cells cover viewer==mint, viewer==between,
/// viewer==leaf and viewer==root.
const B: [&str; 3] = ["b0", "b1", "b2"];
const C: [&str; 3] = ["a", "b", "c"];
const GRAND: &str = "g1";

const GRID_MINUS_TAIL: usize = GRID - 3;

/// One draw from the seed vector. The builder consumes the seeds
/// deterministically so proptest's shrinking maps seed churn onto case churn.
fn draw(seeds: &[u32]) -> Bundle {
    let mut idx = 0usize;
    let mut next = move |limit: usize| -> usize {
        let v = seeds[idx % seeds.len()] as usize;
        idx += 1;
        v % limit.max(1)
    };

    let mut nodes = vec![Node {
        name: "main",
        parent: None,
        forked_at: None,
    }];

    // 1..=3 children of main, each with its own recorded cutoff; the child
    // names are fixed so the viewer can address them
    let child_count = 1 + next(3);
    for child in B.iter().take(child_count) {
        nodes.push(Node {
            name: child,
            parent: Some("main"),
            forked_at: Some(1 + next(GRID - 2)),
        });
    }
    // a grandchild under one child (when there is one worth having)
    if child_count >= 2 && next(2) == 1 {
        let parent = B[next(child_count)];
        nodes.push(Node {
            name: GRAND,
            parent: Some(parent),
            forked_at: Some(1 + next(GRID - 2)),
        });
    }

    let viewer = nodes[next(nodes.len())].name;

    let mut mint_of = BTreeMap::new();
    let mut sheets: BTreeMap<(&'static str, &'static str), Vec<Row>> = BTreeMap::new();
    let mut cuts: BTreeMap<(&'static str, &'static str), Vec<CutRow>> = BTreeMap::new();

    for id in C {
        let mint = nodes[next(nodes.len())].name;
        mint_of.insert(id, mint);

        // The mint lineage holds exactly one era: the concepts row's window.
        let start = next(GRID_MINUS_TAIL);
        let len = 1 + next(3);
        let mint_sheet = vec![Row {
            start,
            end: start + len,
            recorded: next(GRID),
        }];

        // every other lineage holds 0..=3 disjoint rows of its own
        let mut other: Vec<(&str, Vec<Row>)> = Vec::new();
        for node in &nodes {
            if node.name == mint {
                continue;
            }
            let mut rows = Vec::new();
            let mut cursor = 0usize;
            for _ in 0..next(4) {
                // advance past the previous window, then take len inside the
                // grid; disjoint and ascending by construction
                if cursor >= GRID {
                    break; // the grid is spent
                }
                let left = GRID - cursor;
                if left == 1 {
                    rows.push(Row {
                        start: GRID - 1,
                        end: GRID,
                        recorded: next(GRID),
                    });
                    break;
                }
                let start = cursor + next(left);
                let len = (1 + next(3)).min(GRID - start);
                cursor = start + len.max(1);
                rows.push(Row {
                    start,
                    end: start + len.max(1),
                    recorded: next(GRID),
                });
            }
            if !rows.is_empty() {
                other.push((node.name, rows));
            }
        }

        sheets.insert((mint, id), mint_sheet);
        for (node, rows) in other {
            sheets.insert((node, id), rows);
        }

        // cuts across every lineage that participates in this id, the mint's
        // included. Three shapes:
        //   - the supersede pair: a cut sharing the first version's start and
        //     recorded stamp (W2's one-transaction supersede, §5.2), the shape
        //     whose retirement is the resurrection hole;
        //   - the retirement: a cut at a lineage's last window's end, its own
        //     era plus nothing after;
        //   - the bare cut: anywhere on the valid axis, recorded anywhere —
        //     the reject-inheritance shape (and any rule-state in between).
        let push_cut =
            |key: (&'static str, &'static str),
             cut: CutRow,
             cuts: &mut BTreeMap<(&'static str, &'static str), Vec<CutRow>>| {
                let entry = cuts.entry(key).or_default();
                if !entry.iter().any(|c| c.cut_at == cut.cut_at) {
                    entry.push(cut);
                }
            };

        if next(2) == 1 {
            let first = sheets[&(mint, id)][0];
            push_cut(
                (mint, id),
                CutRow {
                    cut_at: first.start,
                    recorded: first.recorded,
                },
                &mut cuts,
            );
        }
        if next(3) == 0 {
            let last = sheets[&(mint, id)].last().unwrap();
            push_cut(
                (mint, id),
                CutRow {
                    cut_at: last.end,
                    recorded: next(GRID),
                },
                &mut cuts,
            );
        }

        for node in &nodes {
            let mined = node.name == mint;
            if let Some(rows) = sheets.get(&(node.name, id)) {
                if !mined && next(2) == 1 {
                    // the supersede pair: cut shares the first window's start
                    // and its recorded stamp, both written at one stamp (§5.2)
                    let first = rows[0];
                    push_cut(
                        (node.name, id),
                        CutRow {
                            cut_at: first.start,
                            recorded: first.recorded,
                        },
                        &mut cuts,
                    );
                }
                if next(2) == 1 {
                    let last = rows.last().unwrap();
                    push_cut(
                        (node.name, id),
                        CutRow {
                            cut_at: last.end,
                            recorded: next(GRID),
                        },
                        &mut cuts,
                    );
                }
            }
            if next(3) == 0 {
                push_cut(
                    (node.name, id),
                    CutRow {
                        cut_at: next(GRID),
                        recorded: next(GRID),
                    },
                    &mut cuts,
                );
            }
        }
    }

    Bundle {
        nodes,
        mint_of,
        sheets,
        cuts,
        viewer,
    }
}

// ---------------------------------------------------------------------------
// The brute-force oracle (no SQL)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Belief {
    /// A row resolved, and which one it is.
    Resolved { bid: String, start: usize },
    /// Nothing is here, and nothing falls through either: either no lineage
    /// on the path covers `v`, or a visible cut stopped the walk.
    Absent,
}

/// `(node, cumulative cut)`, nearest first. The cut is the `min` of every
/// forked_at crossed from the viewer down to this node, `None` for the
/// viewer itself (it sees its own rows regardless of fork records).
fn path<'a>(bundle: &'a Bundle, viewer: &'a str) -> Vec<(&'a str, Option<usize>)> {
    let by_name = |n: &str| {
        bundle
            .nodes
            .iter()
            .find(|x| x.name == n)
            .expect("node in bundle")
    };
    let mut out = vec![(viewer, None)];
    let mut current = by_name(viewer);
    let mut cut: Option<usize> = None;
    while let Some(parent) = current.parent {
        let hop = current.forked_at.expect("non-root nodes carry a cutoff");
        cut = Some(cut.map_or(hop, |c| c.min(hop)));
        out.push((parent, cut));
        current = by_name(parent);
    }
    out
}

/// §5.3 as revision 3 states it: version wins, cut caps the fall-through,
/// nearest decides first and stops the walk.
fn brute(bundle: &Bundle, viewer: &str, id: &str, v: usize, r: usize) -> Belief {
    for (node, hop_cap) in path(bundle, viewer) {
        let cap = hop_cap.unwrap_or(usize::MAX).min(r);
        if let Some(rows) = bundle.sheets.get(&(node, id)) {
            for row in rows {
                let within = v >= row.start && v < row.end;
                if within && row.recorded <= cap {
                    // a version wins over a cut where both cover v
                    return Belief::Resolved {
                        bid: node.to_string(),
                        start: row.start,
                    };
                }
            }
        }
        if let Some(cuts) = bundle.cuts.get(&(node, id)) {
            for cut in cuts {
                if cut.cut_at <= v && cut.recorded <= cap {
                    // a visible cut caps the fall-through: absent, and the
                    // walk stops — the inherited belief stays hidden
                    return Belief::Absent;
                }
            }
        }
    }
    Belief::Absent
}

// ---------------------------------------------------------------------------
// The candidate SQL: the nearest decisive lineage, version before cut
// ---------------------------------------------------------------------------

const RESOLVE_SQL: &str = r#"
WITH RECURSIVE anc(bid, dist, hop_cut) AS (
    SELECT ?1, 0, NULL
    UNION ALL
    SELECT b.parent_id, a.dist + 1,
           CASE WHEN a.hop_cut IS NULL OR b.forked_at < a.hop_cut
                THEN b.forked_at ELSE a.hop_cut END
    FROM branches b JOIN anc a ON b.branch_id = a.bid
    WHERE b.parent_id IS NOT NULL
),
bound(bid, dist, cap) AS (
    SELECT bid, dist,
           CASE WHEN hop_cut IS NULL OR hop_cut > ?3 THEN ?3 ELSE hop_cut END
    FROM anc
)
SELECT bid, kind, valid FROM (
    SELECT a.bid, a.dist, 'version' AS kind, v.valid_from AS valid
      FROM bound a JOIN concept_versions v ON v.branch_id = a.bid AND v.id = ?2
     WHERE ?4 >= v.valid_from AND ?4 < v.valid_to AND v.recorded_at <= a.cap
    UNION ALL
    SELECT a.bid, a.dist, 'version' AS kind, x.valid_from AS valid
      FROM bound a JOIN concepts x ON x.branch_id = a.bid AND x.id = ?2
     WHERE ?4 >= x.valid_from AND ?4 < x.valid_to AND x.recorded_at <= a.cap
    UNION ALL
    SELECT a.bid, a.dist, 'cut' AS kind, NULL AS valid
      FROM bound a
     WHERE EXISTS (SELECT 1 FROM concept_cuts k
                    WHERE k.branch_id = a.bid AND k.id = ?2
                      AND k.cut_at <= ?4 AND k.recorded_at <= a.cap)
)
ORDER BY dist ASC, CASE kind WHEN 'cut' THEN 1 ELSE 0 END ASC
LIMIT 1
"#;

fn sql_belief(stmt: &libsql::Connection, viewer: &str, id: &str, v: usize, r: usize) -> Belief {
    block_on(async { sql_belief_async(stmt, viewer, id, v, r).await })
}

async fn sql_belief_async(
    stmt: &libsql::Connection,
    viewer: &str,
    id: &str,
    v: usize,
    r: usize,
) -> Belief {
    let r_stamp = match r {
        OPEN_R => FOREVER_STAMP.to_string(),
        _ => stamp(r),
    };
    let mut rows = stmt
        .query(
            RESOLVE_SQL,
            libsql::params![
                libsql::Value::Text(viewer.to_string()),
                libsql::Value::Text(id.to_string()),
                libsql::Value::Text(r_stamp),
                libsql::Value::Text(stamp(v)),
            ],
        )
        .await
        .expect("the resolution statement parses");
    match rows.next().await.expect("rows") {
        None => Belief::Absent,
        Some(row) => {
            let bid: String = row.get(0).unwrap();
            let kind: String = row.get(1).unwrap();
            if kind == "cut" {
                Belief::Absent
            } else {
                let start: String = row.get(2).unwrap();
                Belief::Resolved {
                    bid,
                    start: stamp_index(&start).expect("valid_from is grid-rendered"),
                }
            }
        }
    }
}

fn stamp_index(ts: &str) -> Option<usize> {
    // "2026-01-NN…" → NN-1 (day 1 == slot 0)
    ts.strip_prefix("2026-01-")?
        .chars()
        .take(2)
        .collect::<String>()
        .parse::<usize>()
        .ok()
        .map(|n| n - 1)
}

// ---------------------------------------------------------------------------
// Fixture SQL (branches/concepts v25 shapes; §5.1's two new tables)
// ---------------------------------------------------------------------------

const FIXTURE_DDL: &str = r#"
CREATE TABLE branches (
    branch_id  TEXT NOT NULL PRIMARY KEY,
    parent_id  TEXT REFERENCES branches(branch_id),
    forked_at  TEXT,
    created_at TEXT NOT NULL
);
CREATE TABLE concepts (
    id          TEXT NOT NULL UNIQUE,
    title       TEXT,
    content     TEXT,
    valid_from  TEXT NOT NULL,
    valid_to    TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    branch_id   TEXT NOT NULL REFERENCES branches(branch_id)
);
CREATE TABLE concept_versions (
    id              TEXT NOT NULL,
    branch_id       TEXT NOT NULL REFERENCES branches(branch_id),
    valid_from      TEXT NOT NULL,
    valid_to        TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    title           TEXT,
    content         TEXT NOT NULL DEFAULT '',
    extra           TEXT NOT NULL DEFAULT '{}',
    embedding_model TEXT,
    recorded_at     TEXT NOT NULL,
    PRIMARY KEY (id, branch_id, valid_from)
) WITHOUT ROWID;
CREATE TABLE concept_cuts (
    id          TEXT NOT NULL,
    branch_id   TEXT NOT NULL REFERENCES branches(branch_id),
    cut_at      TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (id, branch_id, cut_at)
) WITHOUT ROWID;
"#;

fn pour(bundle: &Bundle, case_dir: &tempfile::TempDir) -> libsql::Connection {
    block_on(async {
        let db = libsql::Builder::new_local(case_dir.path().join("p.db"))
            .build()
            .await
            .expect("local libsql opens");
        let conn = db.connect().expect("connect");
        conn.execute_batch(FIXTURE_DDL)
            .await
            .expect("the fixture schema");

        for node in &bundle.nodes {
            let parent = node
                .parent
                .map(|p| format!("'{p}'"))
                .unwrap_or_else(|| "NULL".to_string());
            let forked = node
                .forked_at
                .map(|c| format!("'{}'", stamp(c)))
                .unwrap_or_else(|| "NULL".to_string());
            conn.execute(
                &format!(
                    "INSERT OR IGNORE INTO branches (branch_id, parent_id, forked_at, created_at) \
                     VALUES ('{}', {parent}, {forked}, '')",
                    node.name
                ),
                (),
            )
            .await
            .expect("branch row");
        }

        for (id, mint) in &bundle.mint_of {
            let row = bundle.sheets.get(&(*mint, *id)).unwrap()[0];
            conn.execute(
                &format!(
                    "INSERT INTO concepts (id, title, content, valid_from, valid_to, recorded_at, branch_id) \
                     VALUES ('{id}', 'mint', '', '{}', '{}', '{}', '{mint}')",
                    stamp(row.start),
                    stamp(row.end),
                    stamp(row.recorded),
                ),
                (),
            )
            .await
            .expect("mint row");
        }

        for ((node, id), rows) in &bundle.sheets {
            if *node == bundle.mint_of[*id] {
                continue; // the mint row lives in concepts, not here
            }
            for row in rows {
                conn.execute(
                    &format!(
                        "INSERT OR IGNORE INTO concept_versions \
                         (id, branch_id, valid_from, valid_to, recorded_at) \
                         VALUES ('{id}', '{node}', '{}', '{}', '{}')",
                        stamp(row.start),
                        stamp(row.end),
                        stamp(row.recorded),
                    ),
                    (),
                )
                .await
                .expect("version row");
            }
        }

        for ((node, id), cuts) in &bundle.cuts {
            for cut in cuts {
                conn.execute(
                    &format!(
                        "INSERT OR IGNORE INTO concept_cuts (id, branch_id, cut_at, recorded_at) \
                         VALUES ('{id}', '{node}', '{}', '{}')",
                        stamp(cut.cut_at),
                        stamp(cut.recorded),
                    ),
                    (),
                )
                .await
                .expect("cut row");
            }
        }

        conn
    })
}

/// The monotonicity arm's material: one later version and one later cut per
/// non-viewer lineage per id, both recorded after every possible cutoff and
/// `r`, in shapes nothing pre-generated reaches (the versions start past the
/// grid; the cuts say nothing until the reader's r admits them, which it
/// never does for a `LATE` record).
fn pour_later(conn: &libsql::Connection, bundle: &Bundle) {
    block_on(async {
        for node in &bundle.nodes {
            if node.name == bundle.viewer {
                continue;
            }
            for id in C {
                let valid_from = stamp(GRID);
                conn.execute(
                    &format!(
                        "INSERT OR IGNORE INTO concept_versions \
                         (id, branch_id, valid_from, valid_to, recorded_at) \
                         VALUES ('{id}', '{}', '{valid_from}', '{FOREVER_STAMP}', '{LATE_STAMP}')",
                        node.name,
                    ),
                    (),
                )
                .await
                .expect("later version");
                conn.execute(
                    &format!(
                        "INSERT OR IGNORE INTO concept_cuts (id, branch_id, cut_at, recorded_at) \
                         VALUES ('{id}', '{}', '{}', '{LATE_STAMP}')",
                        node.name,
                        stamp(GRID),
                    ),
                    (),
                )
                .await
                .expect("later cut");
            }
        }
    })
}

const CASES: u32 = 256;
const RS: &[usize] = &[OPEN_R, 5];

fn dump(bundle: &Bundle) -> String {
    let mut out = String::new();
    for node in &bundle.nodes {
        out.push_str(&format!(
            "{} parent={:?} forked_at={:?}\n",
            node.name, node.parent, node.forked_at
        ));
    }
    out.push_str(&format!("viewer = {}\n", bundle.viewer));
    for ((node, id), rows) in &bundle.sheets {
        let mint = if bundle.mint_of.get(id).copied() == Some(*node) {
            " [mint]"
        } else {
            ""
        };
        out.push_str(&format!(
            "({node}, {id}){mint} versions: {:?} cuts: {:?}\n",
            rows.iter()
                .map(|r| (r.start, r.end, r.recorded))
                .collect::<Vec<_>>(),
            bundle
                .cuts
                .get(&(node, id))
                .map(|cs| cs
                    .iter()
                    .map(|c| (c.cut_at, c.recorded))
                    .collect::<Vec<_>>())
                .unwrap_or_default(),
        ));
    }
    out
}

/// Fails the case with both answers, and drops the bundle into
/// `tests/m6_resolution_cases/` so the finding stays replayable (D-268).
fn disagree(
    bundle: &Bundle,
    viewer: &str,
    id: &str,
    v: usize,
    r: usize,
    bf: &Belief,
    sql: &Belief,
) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/m6_resolution_cases");
    let _ = std::fs::create_dir_all(&dir);
    let r_label = match r {
        OPEN_R => "now".to_string(),
        _ => stamp(r),
    };
    let file = dir.join(format!("case_{viewer}_{id}_v{v}_r{r_label}.txt"));
    let _ = std::fs::write(
        &file,
        format!(
            "viewer {viewer}, id {id}, v {v}, r {r_label}\nbrute={bf:?}\nsql={sql:?}\n\n{}",
            dump(bundle)
        ),
    );
    panic!(
        "the resolution rule disagrees with itself at viewer={viewer} id={id} v={v} r={r_label}: \
         brute={bf:?} sql={sql:?}; bundle written to {}",
        file.display()
    );
}

fn viewers(bundle: &Bundle) -> Vec<&'static str> {
    let mut out = vec!["main"];
    for name in B {
        out.push(name);
    }
    out.push(GRAND);
    out.retain(|n| bundle.nodes.iter().any(|x| x.name == *n));
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// §5.3 by brute force vs the SQL candidate, cell for cell, then the same
    /// census again once later ancestor versions and cuts have landed.
    #[test]
    fn the_resolution_rule_and_the_sql_see_the_same_cell(seeds in prop::collection::vec(any::<u32>(), 96)) {
        let bundle = draw(&seeds);
        let dir = tempfile::TempDir::new().unwrap();
        let conn = pour(&bundle, &dir);

        // split the candidate's chronology in two: the cells under the base
        // data, then the same cells again once the late writes are in
        let mut base = Vec::new();
        for viewer in viewers(&bundle) {
            for id in C {
                for slot in 0..GRID {
                    for &r in RS {
                        let bf = brute(&bundle, viewer, id, slot, r);
                        let sql = sql_belief(&conn, viewer, id, slot, r);
                        if bf != sql {
                            disagree(&bundle, viewer, id, slot, r, &bf, &sql);
                        }
                        base.push((viewer, id, slot, r, bf));
                    }
                }
            }
        }

        pour_later(&conn, &bundle);

        for (viewer, id, slot, r, bf) in &base {
            let sql = sql_belief(&conn, viewer, id, *slot, *r);
            if &sql != bf {
                disagree(&bundle, viewer, id, *slot, *r, bf, &sql);
            }
            if r != &OPEN_R {
                // the recorded-axis cells also assert the pre-merge emptiness
                // claim directly: a row or cut the reader's r cannot see
                // resolves like one that does not exist
                let late = sql_belief(&conn, viewer, id, *slot, 0);
                let bf_late = brute(&bundle, viewer, id, *slot, 0);
                if late != bf_late {
                    disagree(&bundle, viewer, id, *slot, 0, &bf_late, &late);
                }
            }
        }

        // base == after late (checked above through bf), and the brute force
        // itself is snapshot-stable, which is worth one line of its own
        for (viewer, id, slot, r, bf) in &base {
            if brute(&bundle, viewer, id, *slot, *r) != *bf {
                disagree(&bundle, viewer, id, *slot, *r, bf, bf);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The graduated regression: the hole P-8's first round reds on, pinned
// ---------------------------------------------------------------------------

/// The scenario from the finding that produced Amendment 2 revision 3: the
/// trunk holds `{id}` open from the root; the branch supersedes it at 5
/// (a cut and a version `[5, 8)` at one recorded stamp, §5.2) and its own
/// version retires at 8 with no successor. A branch read at 9 must answer
/// absent — revision 2's nearest-row-wins let the trunk text come back, the
/// S1-class wrong answer. Also pins the two rules the fix leans on: the
/// version wins over the cut where both cover (6), the fall-through still
/// works before the cut (3), the cut hangs down through the grandchild (9 on
/// `g1`), the retired state is decided at one stamp (6 at r=4 answers the
/// trunk, before either row was recorded), and the whole thing reproduces
/// through the SQL candidate, not just the oracle.
#[test]
fn the_superseded_inherited_belief_does_not_come_back() {
    let bundle = Bundle {
        nodes: vec![
            Node {
                name: "main",
                parent: None,
                forked_at: None,
            },
            Node {
                name: "b0",
                parent: Some("main"),
                forked_at: Some(2),
            },
            Node {
                name: GRAND,
                parent: Some("b0"),
                forked_at: Some(6),
            },
        ],
        mint_of: BTreeMap::from([("c", "main")]),
        sheets: BTreeMap::from([
            // the trunk's open belief, minted on main
            (
                ("main", "c"),
                vec![Row {
                    start: 0,
                    end: GRID,
                    recorded: 0,
                }],
            ),
            // the branch's supersede: version [5, 8) ... and its cut, both at
            // one recorded stamp; the version later retired at 8 with no
            // successor, which is just this window's shape
            (
                ("b0", "c"),
                vec![Row {
                    start: 5,
                    end: 8,
                    recorded: 5,
                }],
            ),
        ]),
        cuts: BTreeMap::from([(
            ("b0", "c"),
            vec![CutRow {
                cut_at: 5,
                recorded: 5,
            }],
        )]),
        viewer: "b0",
    };

    // the oracle's answers
    let current = brute(&bundle, "b0", "c", 9, OPEN_R);
    assert_eq!(
        current,
        Belief::Absent,
        "the trunk belief must not come back after the retire"
    );
    let asserting = brute(&bundle, "b0", "c", 6, OPEN_R);
    assert_eq!(
        asserting,
        Belief::Resolved {
            bid: "b0".to_string(),
            start: 5
        },
        "the superseding version wins over the cut where both cover"
    );
    let inheriting = brute(&bundle, "b0", "c", 3, OPEN_R);
    assert_eq!(
        inheriting,
        Belief::Resolved {
            bid: "main".to_string(),
            start: 0
        },
        "before the cut, the branch still inherits"
    );
    let grandchild = brute(&bundle, GRAND, "c", 9, OPEN_R);
    assert_eq!(
        grandchild,
        Belief::Absent,
        "the cut caps the grandchild's fall-through too"
    );
    let grandchild_inherits = brute(&bundle, GRAND, "c", 6, OPEN_R);
    assert_eq!(
        grandchild_inherits,
        Belief::Resolved {
            bid: "b0".to_string(),
            start: 5
        },
        "the grandchild inherits the branch's own version"
    );
    let trunk_reads_itself = brute(&bundle, "main", "c", 9, OPEN_R);
    assert_eq!(
        trunk_reads_itself,
        Belief::Resolved {
            bid: "main".to_string(),
            start: 0
        },
        "the cut is a branch-side decision; the trunk's own read is untouched"
    );
    let before_the_stamp = brute(&bundle, "b0", "c", 6, 4);
    assert_eq!(
        before_the_stamp,
        Belief::Resolved {
            bid: "main".to_string(),
            start: 0
        },
        "at r = 4 neither the cut nor the version was recorded yet; the trunk shows"
    );

    // the candidate SQL reproduces the same answers
    let dir = tempfile::TempDir::new().unwrap();
    let conn = pour(&bundle, &dir);
    assert_eq!(sql_belief(&conn, "b0", "c", 9, OPEN_R), Belief::Absent);
    assert_eq!(
        sql_belief(&conn, "b0", "c", 6, OPEN_R),
        Belief::Resolved {
            bid: "b0".to_string(),
            start: 5
        }
    );
    assert_eq!(
        sql_belief(&conn, "b0", "c", 3, OPEN_R),
        Belief::Resolved {
            bid: "main".to_string(),
            start: 0
        }
    );
    assert_eq!(sql_belief(&conn, GRAND, "c", 9, OPEN_R), Belief::Absent);
    assert_eq!(
        sql_belief(&conn, GRAND, "c", 6, OPEN_R),
        Belief::Resolved {
            bid: "b0".to_string(),
            start: 5
        }
    );
    assert_eq!(
        sql_belief(&conn, "main", "c", 9, OPEN_R),
        Belief::Resolved {
            bid: "main".to_string(),
            start: 0
        }
    );
    assert_eq!(
        sql_belief(&conn, "b0", "c", 6, 4),
        Belief::Resolved {
            bid: "main".to_string(),
            start: 0
        }
    );
}
