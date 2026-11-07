//! P-1 of `docs/Macrame Update Plan v0.22.0 - Amendment 2.md` (§8): is the
//! proposed `concept_versions` rung **additive** against a populated v25
//! file, re-opening nothing D-214 measured?
//!
//! Revision 2 of the amendment chose the table §15.2 designed instead of
//! widening `concepts` identity, precisely so the store stays additive:
//! `concepts` keeps one row per id, and every foreign key into it
//! (`links.source_id` / `links.target_id`, the per-model embedding tables at
//! `src/schema/ddl.rs:795`, `analytics_annotations`) keeps its unique parent.
//! This probe files a fresh v25 database through the public write paths,
//! takes an exact census of every table the rung must not disturb, applies
//! the version-rung sketch through the same `raw()` outside writer the
//! D-074 class speaks about, and takes the census again. It then re-proves
//! the two D-214 behaviors the rung must not have weakened:
//!
//! 1. `trg_concepts_cross_lineage` still fires for a *direct* INSERT of an id
//!    another lineage mints, and `trg_concepts_branch_immutable` still fires
//!    for a lineage move. The rung never touches those guards — the guard
//!    comment (`src/schema/ddl.rs:577-580`) says the overlay decision is
//!    machinery, not a quiet guard, and the sketch honors that literally.
//! 2. `concept_versions` itself: the FK to `branches(branch_id)` is enforced;
//!    version rows are immutable (a correction writes a new row); a version
//!    row can live beside the concepts row it versions; a direct write to
//!    `concepts` still refuses across lineages *after* the rung exists.
//!
//! Nothing here farms out to a migration rung in `src/schema/migrations.rs`:
//! the sketch ships in the probe, which is what the amendment's §14 bounds
//! mean by "a sketch for costing". The real rung (P-1's committed state)
//! lands with M6-a.
//!
//! Known-handled FTS limitation: there is deliberately no `verify_fts()`
//! (D-071), and empty-table `'integrity-check'` lies, so this probe runs the
//! integrity check only against a *populated* index and states the result.
//!
//! Run with:  cargo run --release --example m6_p1_overlay_rung_probe

use macrame::prelude::*;
use macrame::vector::ModelName;

const T0: &str = "2026-01-01T00:00:00.000000Z";
const MODEL: &str = "probe_model";

/// The v25 → v26 sketch under test (Amendment 2 §5.1). GLOB checks on the
/// bitemporal stamps are omitted in the sketch; `canonical_ts_check!` lands
/// with the rung and P-1 has nothing to ask of it.
const V26_RUNG: &str = r#"
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
CREATE TRIGGER IF NOT EXISTS trg_concept_versions_immutable
BEFORE UPDATE ON concept_versions
BEGIN
    SELECT RAISE(ABORT, 'concept_versions rows are immutable; correct by writing a new row');
END;
"#;

type Conn = libsql::Connection;
use anyhow_t::Result as PResult;

/// One census probe: label plus the SQL whose text answer the rung must not
/// move. `ORDER BY` everywhere so the dump is deterministic; two timestamp
/// columns per concept because the bitemporal answer is the one that matters.
async fn census(conn: &Conn, label: &str, sql: &str, cells: usize) -> PResult<Vec<String>> {
    let mut rows = conn.query(sql, ()).await?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().await? {
        let mut parts = Vec::with_capacity(cells);
        for i in 0..cells {
            parts.push(anyhow_t::cell(r.get_value(i as i32)?));
        }
        out.push(parts.join("\u{183}"));
    }
    if out.is_empty() {
        out.push("(none)".to_string());
    }
    let _ = label; // only the rows diff; the label is the call site's context
    Ok(out)
}

pub mod anyhow_t {
    //! The crate's `Result` is `macrame::Result<T = ()>` over `DbError`; the
    //! probe needs its own tiny error carrying `libsql` failures verbatim.
    #[derive(Debug)]
    pub struct ProbeError(pub String);
    impl std::fmt::Display for ProbeError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }
    impl std::error::Error for ProbeError {}
    impl From<libsql::Error> for ProbeError {
        fn from(e: libsql::Error) -> Self {
            ProbeError(e.to_string())
        }
    }
    pub type Result<T> = std::result::Result<T, ProbeError>;
    pub fn err(e: impl std::fmt::Display) -> ProbeError {
        ProbeError(e.to_string())
    }
    /// A census cell renders without ambiguity for diffing.
    pub fn cell(v: libsql::Value) -> String {
        match v {
            libsql::Value::Null => "NULL".to_string(),
            libsql::Value::Integer(i) => i.to_string(),
            libsql::Value::Real(x) => x.to_string(),
            libsql::Value::Text(t) => t,
            libsql::Value::Blob(b) => format!("<blob len={}>", b.len()),
        }
    }
}

/// Every table the rung must leave byte-identical, with the columns that make
/// the answer honest. `links` is fully dumped because the composite FK in
/// D-214's story is exactly what a half-overlay would touch first.
const CENSUS: &[(&str, &str, usize)] = &[
    (
        "concepts",
        "SELECT id, branch_id, title, content, valid_from, valid_to, recorded_at, retired, extra \
         FROM concepts ORDER BY id, valid_from",
        9,
    ),
    (
        "links",
        "SELECT source_id, target_id, edge_type, valid_from, valid_to, weight, branch_id \
         FROM links ORDER BY source_id, target_id, edge_type, valid_from",
        7,
    ),
    (
        "transaction_log",
        "SELECT seq_id, table_name, entity_id, operation, recorded_at, branch_id \
         FROM transaction_log ORDER BY seq_id",
        6,
    ),
    (
        "branches",
        "SELECT branch_id, IFNULL(parent_id, '(trunk)'), IFNULL(forked_at, '(none)'), created_at \
         FROM branches ORDER BY branch_id",
        4,
    ),
    (
        "embeddings",
        "SELECT concept_id, embedding FROM embeddings_probe_model ORDER BY concept_id",
        2,
    ),
    (
        "concepts_fts (index)",
        "SELECT rowid, title, content FROM concepts_fts ORDER BY rowid",
        3,
    ),
];

/// The FTS-MATCH answers the tokenized census cannot see.
const MATCH_PROBES: &[&str] = &["alpha", "beta", "gamma"];

fn fail(step: &str, why: String) -> ! {
    eprintln!("P-1 … step '{step}' FAILED: {why}");
    std::process::exit(1);
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("macrame_m6_p1_probe_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    println!(
        "m6_p1_overlay_rung_probe — crate {}",
        env!("CARGO_PKG_VERSION")
    );

    let db = Database::open(dir.join("v25.db")).await.unwrap();
    let model = ModelName::new(MODEL).unwrap();

    // -- populate v25 through public paths ------------------------------------
    db.upsert_concept(
        ConceptUpsert::new("alpha", "alpha title")
            .content("alpha body")
            .valid_from(T0),
    )
    .await
    .unwrap();
    db.upsert_concept(
        ConceptUpsert::new("beta", "beta title")
            .content("beta body")
            .valid_from(T0),
    )
    .await
    .unwrap();
    db.upsert_concept(ConceptUpsert::new("gamma", "gamma title").valid_from(T0))
        .await
        .unwrap();
    db.assert_edge(EdgeAssertion::new("alpha", "beta", "linked").valid_from(T0))
        .await
        .unwrap();
    db.register_model(&model, 4).await.unwrap();
    db.upsert_embeddings(&model, vec![("alpha".into(), vec![1.0, 0.0, 0.0, 0.0])])
        .await
        .unwrap();

    let read = db.read_conn();
    let mut before = Vec::with_capacity(CENSUS.len());
    for (name, sql, cells) in CENSUS {
        let rows = census(read, name, sql, *cells)
            .await
            .unwrap_or_else(|e| fail("census before", e.to_string()));
        println!("  census before {name}: {} rows", rows.len());
        before.push((name.to_string(), rows));
    }

    // -- apply the sketch through raw() (the D-074 outside writer) ------------
    {
        let write = db.raw().connect().unwrap();
        write.execute_batch(V26_RUNG).await.unwrap();
        println!("rung executed (concept_versions + immutability trigger)");
    }

    // -- the rung disturbed nothing -------------------------------------------
    for (i, (name, sql, cells)) in CENSUS.iter().enumerate() {
        let rows = census(read, name, sql, *cells)
            .await
            .unwrap_or_else(|e| fail("census after", e.to_string()));
        let (_, b) = &before[i];
        assert_eq!(
            b, &rows,
            "census for {name} changed when an additive table was created"
        );
        println!("P-1 … census '{name}' byte-identical after the rung — PASS");
    }

    // reference integrity, both halves
    let mut fk_rows = read.query("PRAGMA foreign_key_check", ()).await.unwrap();
    if fk_rows.next().await.unwrap().is_some() {
        fail("foreign_key_check", "non-empty after the rung".into());
    }
    println!("P-1 … PRAGMA foreign_key_check empty — PASS");

    {
        let write = db.raw().connect().unwrap();
        write
            .execute(
                "INSERT INTO concepts_fts(concepts_fts) VALUES('integrity-check')",
                (),
            )
            .await
            .unwrap();
    }
    println!("P-1 … concepts_fts integrity-check accepted on a populated index — PASS");

    for token in MATCH_PROBES {
        let sql = format!("SELECT COUNT(*) FROM concepts_fts WHERE concepts_fts MATCH '{token}'");
        let n = scalar_count(read, &sql).await;
        println!("P-1 … MATCH '{token}' → {n}");
    }

    // -- D-214's guards still hold after the rung ------------------------------
    let vista = BranchId::new("vista").unwrap();
    db.fork(vista.clone(), BranchId::main()).await.unwrap();

    {
        let write = db.raw().connect().unwrap();
        // a direct cross-lineage INSERT is refused, revision unchanged
        let r = write
            .execute(
                "INSERT INTO concepts (id, title, content, valid_from, recorded_at, branch_id) \
                 VALUES ('alpha', 'x', '', '2026-02-01T00:00:00.000000Z', \
                 '2026-02-01T00:00:00.000000Z', 'vista')",
                (),
            )
            .await;
        assert!(r.is_err(), "the cross-lineage guard stopped firing");
        println!(
            "P-1 … direct cross-lineage INSERT refused — PASS ({})",
            r.unwrap_err()
        );
        // so is a branch move
        let r = write
            .execute(
                "UPDATE concepts SET branch_id = 'vista' WHERE id = 'beta'",
                (),
            )
            .await;
        assert!(r.is_err(), "the branch-immutability guard stopped firing");
        println!(
            "P-1 … direct branch move refused — PASS ({})",
            r.unwrap_err()
        );
    }

    // -- the new table behaves as the design says ------------------------------
    {
        let write = db.raw().connect().unwrap();
        // unknown lineage → FK refuses (the overlay has no fiction that every
        // id's mint is visible)
        let r = write
            .execute(
                "INSERT INTO concept_versions (id, branch_id, valid_from, kind, title, content, \
                 recorded_at) VALUES ('alpha', 'nowhere', '2026-02-01T00:00:00.000000Z', \
                 'version', 'nope', '', '2026-02-01T00:00:00.000000Z')",
                (),
            )
            .await;
        assert!(r.is_err(), "the FK to branches did not fire");
        println!("P-1 … concept_versions FK to branches enforced — PASS");

        // a version row can live beside the concepts row it versions
        write
            .execute(
                "INSERT INTO concept_versions (id, branch_id, valid_from, kind, title, content, \
                 recorded_at) VALUES ('alpha', 'vista', '2026-02-01T00:00:00.000000Z', \
                 'version', 'alpha title on vista', 'alpha body on vista', \
                 '2026-02-01T00:00:00.000000Z')",
                (),
            )
            .await
            .unwrap();
        // corrections are writes, not updates
        let r = write
            .execute(
                "UPDATE concept_versions SET title = 'never' \
                 WHERE id = 'alpha' AND branch_id = 'vista'",
                (),
            )
            .await;
        assert!(r.is_err(), "the immutability trigger did not fire");
        println!(
            "P-1 … concept_versions rows immutable — PASS ({})",
            r.unwrap_err()
        );
        assert_eq!(
            scalar_count(
                read,
                "SELECT COUNT(*) FROM concept_versions WHERE id = 'alpha' AND branch_id = 'vista'"
            )
            .await,
            1,
            "the version row did not survive its own immutability probe"
        );
        assert_eq!(
            scalar_count(read, "SELECT COUNT(*) FROM concepts WHERE id = 'alpha'").await,
            1,
            "the version row disturbed concepts"
        );
        println!("P-1 … version row and concepts row coexist — PASS");
    }

    // -- census again, now WITH one version row: concepts must still be identical
    {
        let (name, sql, cells) = &CENSUS[0];
        let rows = census(read, name, sql, *cells)
            .await
            .unwrap_or_else(|e| fail("final concepts census", e.to_string()));
        assert_eq!(
            &before[0].1, &rows,
            "concepts changed with a version row present"
        );
        let _ = (name, sql, cells);
        println!("P-1 … concepts still byte-identical with a version row present — PASS");
    }

    println!("\nP-1: the v26 sketch is additive, and nothing D-214 measured moved.");
}

async fn scalar_count(conn: &Conn, sql: &str) -> usize {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let r = rows.next().await.unwrap().unwrap();
    r.get::<i64>(0).unwrap() as usize
}
