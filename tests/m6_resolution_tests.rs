//! The last M6-P probe of [Amendment 2](../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md):
//! the §5.3 resolution rule, checked by generation before any reader converts
//! (M6-b). P-8 in that amendment's §8 table: random fork trees, random
//! versions, cuts and ancestor backfills; §5.3 brute force vs the SQL
//! resolution; monotonic under later ancestor writes.
//!
//! # What the rule under test says
//!
//! For a reader on lineage `L` at recorded cutoff `r` and valid instant `v`:
//! each lineage on `path(L)` contributes rows recorded at or before its cut
//! (`min` of the fork cutoffs crossed on the path, further capped by `r`);
//! the belief at `v` is the **nearest** lineage's row covering `v`; a
//! `'cut'` row resolves to *absent*. Two consequences the generator is built
//! to catch:
//!
//! * **D-315's emptiness claim** — the reader's *own* rows are capped by `r`
//!   too, so the trunk at an `r` before an adoption's stamp finds nothing. A
//!   candidate that gets this wrong answers the merge's future into its past.
//! * **Monotonicity** — writes to an ancestor recorded after the reader's
//!   cutoff change nothing the reader resolves. The nearness is structural
//!   (a viewer's own rows sit at distance 0), but the test verifies the
//!   *implementation shape* that makes it hold, not the hope: the same cell
//!   set is resolved before and after the late rows are added.
//!
//! # Scope, deliberately narrower than the engine
//!
//! This file tests the **rule**, not M6-b's fold: the generator keeps every
//! `(lineage, id)` interval list disjoint and ascending, so one lineage has at
//! most one row covering any instant and D-303's within-lineage resolution is
//! vacuous here (that contract has its own machinery and suite, which M6-a
//! inherits). The mint lineage holds exactly one era — the row the
//! projection carries today; same-lineage supersede of a mint is D-312's
//! built contract, not this file's. No `transaction_log`, FTS or vector
//! tables: those are P-3/P-4's.
//!
//! # The honest shape of the candidate
//!
//! The SQL under test restates the fork-cutoff walk that
//! `src/graph/lineage.rs`'s `ancestry_cte` spells (the D-259 oracle) — the
//! delta under test is the *nearness* layer D-314 adds: versions and cuts
//! compete with the mint row by path distance. A restatement rather than a
//! call into the crate: nothing public binds that CTE, and the D-259 lesson
//! is that oracle and candidate must be wrong *independently*
//! (`the_rust_walk_agrees_with_the_cte`) for a differential to be worth
//! keeping.
//!
//! # Harness
//!
//! One runtime for the whole binary, serial execution, 32 cases — the R15
//! mitigations `doctrine_property_tests.rs` documents, unchanged. Runs under
//! `cargo test --features property-tests --test m6_resolution_tests`; every
//! libSQL STATUS_ACCESS_VIOLATION here is the fault R15 records the retry
//! budget for, not a regression. A failing case writes the bundle as text to
//! `tests/m6_resolution_cases/` (the D-268 pattern), so a genuine rule
//! finding can graduate into an unconditional replay test beside it.

use std::collections::BTreeMap;
use std::future::Future;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Version,
    Cut,
}

impl Kind {
    fn sql(self) -> &'static str {
        match self {
            Kind::Version => "version",
            Kind::Cut => "cut",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Row {
    start: usize,
    end: usize,
    kind: Kind,
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
    /// Rows per `(lineage, id)`, the mint's included at its own key (exactly
    /// one row; the pour routes it to `concepts`, everyone else's to
    /// `concept_versions`).
    sheets: BTreeMap<(&'static str, &'static str), Vec<Row>>,
    viewer: &'static str,
}

/// Node names the generator may mint on. The viewer is chosen from these
/// plus `main` plus the grandchild, so the cells cover viewer==mint,
/// viewer==between, viewer==leaf and viewer==root.
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

    for id in C {
        let mint = nodes[next(nodes.len())].name;
        mint_of.insert(id, mint);

        // The mint lineage holds exactly one era: the concepts row's window.
        let start = next(GRID_MINUS_TAIL);
        let len = 1 + next(3);
        sheets.insert(
            (mint, id),
            vec![Row {
                start,
                end: start + len,
                kind: Kind::Version,
                recorded: next(GRID),
            }],
        );

        // every other lineage holds 0..=3 disjoint rows of its own
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
                        kind: if next(3) == 0 {
                            Kind::Cut
                        } else {
                            Kind::Version
                        },
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
                    kind: if next(3) == 0 {
                        Kind::Cut
                    } else {
                        Kind::Version
                    },
                    recorded: next(GRID),
                });
            }
            if !rows.is_empty() {
                sheets.insert((node.name, id), rows);
            }
        }
    }

    Bundle {
        nodes,
        mint_of,
        sheets,
        viewer,
    }
}

// ---------------------------------------------------------------------------
// The brute-force oracle (no SQL)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Belief {
    /// A row resolved, and which one it is.
    Resolved {
        bid: String,
        kind: String,
        start: usize,
    },
    /// Nothing covers on the visible path at this instant — either because
    /// nothing is there, or because the row that is there is a cut.
    Absent,
}

impl Belief {
    fn from_row(bid: String, kind: String, start: usize) -> Belief {
        if kind == "cut" {
            Belief::Absent
        } else {
            Belief::Resolved { bid, kind, start }
        }
    }
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

fn brute(bundle: &Bundle, viewer: &str, id: &str, v: usize, r: usize) -> Belief {
    for (node, cut) in path(bundle, viewer) {
        let rows = match bundle.sheets.get(&(node, id)) {
            Some(rows) => rows,
            None => continue,
        };
        for row in rows {
            let within = v >= row.start && v < row.end;
            let cut_bound = cut.unwrap_or(usize::MAX).min(r);
            let visible = row.recorded <= cut_bound;
            if within && visible {
                return Belief::from_row(node.to_string(), row.kind.sql().to_string(), row.start);
            }
        }
    }
    Belief::Absent
}

// ---------------------------------------------------------------------------
// The candidate SQL (the shape D-314's fold rests on)
// ---------------------------------------------------------------------------

const RESOLVE_SQL: &str = r#"
WITH RECURSIVE anc(bid, dist, cut) AS (
    SELECT ?1, 0, NULL
    UNION ALL
    SELECT b.parent_id, a.dist + 1,
           CASE WHEN a.cut IS NULL OR b.forked_at < a.cut
                THEN b.forked_at ELSE a.cut END
    FROM branches b JOIN anc a ON b.branch_id = a.bid
    WHERE b.parent_id IS NOT NULL
)
SELECT bid, kind, valid_from FROM (
    SELECT a.bid, a.dist, c.kind AS kind, c.valid_from, c.recorded_at
      FROM anc a JOIN concept_versions c ON c.branch_id = a.bid AND c.id = ?2
     WHERE ?4 >= c.valid_from AND ?4 < c.valid_to
       AND c.recorded_at <= min(COALESCE(a.cut, '9999-12-31T23:59:59.999999Z'), ?3)
    UNION ALL
    SELECT a.bid, a.dist, 'version' AS kind, x.valid_from, x.recorded_at
      FROM anc a JOIN concepts x ON x.branch_id = a.bid AND x.id = ?2
     WHERE ?4 >= x.valid_from AND ?4 < x.valid_to
       AND x.recorded_at <= min(COALESCE(a.cut, '9999-12-31T23:59:59.999999Z'), ?3)
)
ORDER BY dist LIMIT 1
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
                libsql::Value::Text(viewer.into()),
                libsql::Value::Text(id.into()),
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
            let start: String = row.get(2).unwrap();
            Belief::from_row(
                bid,
                kind,
                stamp_index(&start).expect("valid_from is grid-rendered"),
            )
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
// Fixture SQL (branches/concepts v25 shapes; concept_versions is §5.1's sketch)
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
    kind            TEXT NOT NULL CHECK (kind IN ('version', 'cut')),
    title           TEXT,
    content         TEXT NOT NULL DEFAULT '',
    extra           TEXT NOT NULL DEFAULT '{}',
    embedding_model TEXT,
    recorded_at     TEXT NOT NULL,
    PRIMARY KEY (id, branch_id, valid_from)
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
                         (id, branch_id, valid_from, valid_to, kind, recorded_at) \
                         VALUES ('{id}', '{node}', '{}', '{}', '{}', '{}')",
                        stamp(row.start),
                        stamp(row.end),
                        row.kind.sql(),
                        stamp(row.recorded),
                    ),
                    (),
                )
                .await
                .expect("version row");
            }
        }

        conn
    })
}

/// The monotonicity arm's material: one later write per non-viewer lineage per
/// id, recorded after every possible cutoff and `r`, in a window nothing
/// pre-generated reaches (every generated window ends at or before GRID).
fn pour_later(conn: &libsql::Connection, bundle: &Bundle) {
    block_on(async {
        for node in &bundle.nodes {
            if node.name == bundle.viewer {
                continue;
            }
            for id in C {
                let valid_from = stamp(GRID);
                let valid_to = FOREVER_STAMP;
                conn.execute(
                    &format!(
                        "INSERT OR IGNORE INTO concept_versions                          (id, branch_id, valid_from, valid_to, kind, recorded_at)                          VALUES ('{id}', '{}', '{valid_from}', '{valid_to}', 'version', '{LATE_STAMP}')",
                        node.name,
                    ),
                    (),
                )
                .await
                .expect("later write");
            }
        }
    })
}

const CASES: u32 = 32;
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
            "({node}, {id}){mint}: {:?}\n",
            rows.iter()
                .map(|r| (r.start, r.end, r.kind.sql(), r.recorded))
                .collect::<Vec<_>>()
        ));
    }
    out
}

use std::path::PathBuf;

/// Fails the case with both answers, and drops the bundle into
/// `tests/m6_resolution_cases/` so the finding replayable (D-268).
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
    let file = dir.join(format!("case_{}{}_v{v}_r{:?}.txt", viewer, id, r));
    let _ = std::fs::write(
        &file,
        format!(
            "viewers {viewer}, id {id}, v {v}, r {r:?}\nbrute={bf:?}\nsql={sql:?}\n\n{}",
            dump(bundle)
        ),
    );
    panic!(
        "the resolution rule disagrees with itself at viewer={viewer} id={id} v={v} r={r:?}: \
         brute={bf:?} sql={sql:?}; bundle written to {}",
        file.display()
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// §5.3 by brute force vs the SQL nearness resolution, cell by cell, then
    /// the same census again once later ancestor writes have landed.
    #[test]
    fn the_resolution_rule_and_the_sql_see_the_same_cell(seeds in prop::collection::vec(any::<u32>(), 96)) {
        let bundle = draw(&seeds);
        let dir = tempfile::TempDir::new().unwrap();
        let conn = pour(&bundle, &dir);

        // split the sql's chronology in two: the cells under the base data,
        // then the same cells again once the later writes are in
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
                // claim directly: a row the reader's r cannot see resolves like
                // a row that does not exist
                let late = sql_belief(&conn, viewer, id, *slot, 0);
                let bf_late = brute(&bundle, viewer, id, *slot, 0);
                if late != bf_late {
                    disagree(&bundle, viewer, id, *slot, 0, &bf_late, &late);
                }
            }
        }

        // base == after late (checked inside the loop above through bf) — and
        // the brute force itself is snapshot-stable, which is worth one line:
        for (viewer, id, slot, r, bf) in &base {
            if brute(&bundle, viewer, id, *slot, *r) != *bf {
                disagree(&bundle, viewer, id, *slot, *r, bf, bf);
            }
        }
    }
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
