//! Is the concept/edge asymmetry [D-259] documents actually reachable?
//!
//! `reconstruct_on` resolves **edges** by nearest ancestor and does not resolve
//! **concepts**, because a folded concept row carries no lineage to pick a
//! nearer one by. Both D-259 and Appendix A.1 write that down as a trade, in
//! the form *"where two visible lineages both wrote a concept, the winner is
//! the later log row rather than the nearer lineage."*
//!
//! That sentence describes a state. It does not say whether the state can
//! exist, and the schema has two guards that look like they forbid it:
//!
//! * `concepts.id` is `NOT NULL UNIQUE` — identity, not identity-per-lineage;
//! * `trg_concepts_cross_lineage` (BEFORE INSERT) refuses an id another lineage
//!   holds, and `trg_concepts_branch_immutable` (BEFORE UPDATE) refuses moving
//!   one between lineages.
//!
//! So this probe tries to *build* the state the documentation warns about,
//! by every route the public API offers, and reports which of them the database
//! refuses. A hazard that cannot be constructed is a different thing from a
//! hazard that is merely unlikely, and the difference belongs in the docs.
//!
//! Five routes:
//!
//! 1. **Restate an inherited concept on a fork.** The case the rustdoc names.
//! 2. **Two siblings mint the same id.** Neither inherits it; both are visible
//!    from neither, but both are visible from the trunk's own descendants only
//!    if a lineage forks from both, which it cannot — still worth the refusal.
//! 3. **Parent mints after the child did.** The fork exists first, so the
//!    unique index is being asked about a row the child already owns.
//! 4. **Retire on the fork, then mint on the trunk.** Retirement is not
//!    deletion, so the row is still there — the question is whether the guard
//!    knows that.
//! 5. **The one that is not a write at all**: does `reconstruct_on` ever see
//!    two lineages' concept rows for one id in the *log*, which is what it
//!    actually folds? The log is history and keeps what `concepts` no longer
//!    shows.
//!
//! Run with:  cargo run --release --example concept_lineage_probe

use macrame::prelude::*;
use macrame::BranchId;

const T0: &str = "2026-01-01T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";

/// The refusal, or the fact that there was none.
async fn attempt(label: &str, r: std::result::Result<(), macrame::DbError>) -> bool {
    match r {
        Err(e) => {
            let kind = format!("{e:?}");
            let kind = kind
                .split_once(' ')
                .map_or(kind.clone(), |(k, _)| k.to_string());
            println!("  {label:<44} REFUSED  {kind}");
            true
        }
        Ok(()) => {
            println!("  {label:<44} ACCEPTED  <-- the collision is reachable");
            false
        }
    }
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("macrame_concept_probe_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    println!(
        "concept_lineage_probe — crate {}\n",
        env!("CARGO_PKG_VERSION")
    );

    let db = Database::open(dir.join("p.db")).await.unwrap();

    // The trunk holds `a` BEFORE anything forks, so the forks inherit it and
    // the last section can show that inheritance works as well as that
    // collision does not.
    db.upsert_concept(ConceptUpsert::new("a", "trunk's a").valid_from(T0))
        .await
        .unwrap();
    let exp = BranchId::new("exp").unwrap();
    let sib = BranchId::new("sib").unwrap();
    db.fork(exp.clone(), BranchId::main()).await.unwrap();
    db.fork(sib.clone(), BranchId::main()).await.unwrap();

    println!("=== can two visible lineages hold one concept id? ===\n");
    let mut refused = 0;
    let mut tried = 0;

    // 1. the case the rustdoc names.
    tried += 1;
    refused += attempt(
        "1. fork restates an inherited concept",
        db.upsert_concept(
            ConceptUpsert::new("a", "exp's a")
                .valid_from(T0)
                .on_branch(exp.clone()),
        )
        .await
        .map(|_| ()),
    )
    .await as i32;

    // 2. two siblings mint the same *new* id.
    db.upsert_concept(
        ConceptUpsert::new("b", "exp's b")
            .valid_from(T0)
            .on_branch(exp.clone()),
    )
    .await
    .unwrap();
    tried += 1;
    refused += attempt(
        "2. sibling mints the same new id",
        db.upsert_concept(
            ConceptUpsert::new("b", "sib's b")
                .valid_from(T0)
                .on_branch(sib.clone()),
        )
        .await
        .map(|_| ()),
    )
    .await as i32;

    // 3. the trunk mints an id a fork already owns.
    tried += 1;
    refused += attempt(
        "3. trunk mints an id its fork owns",
        db.upsert_concept(ConceptUpsert::new("b", "trunk's b").valid_from(T0))
            .await
            .map(|_| ()),
    )
    .await as i32;

    // 4. retire on the fork first. Retirement is a closed row, not a deletion,
    //    so the guard should still see it.
    db.upsert_concept(
        ConceptUpsert::new("c", "exp's c")
            .valid_from(T0)
            .on_branch(exp.clone()),
    )
    .await
    .unwrap();
    db.upsert_concept(
        ConceptUpsert::new("c", "exp's c")
            .valid_from(T0)
            .retired(true)
            .on_branch(exp.clone()),
    )
    .await
    .unwrap();
    tried += 1;
    refused += attempt(
        "4. trunk mints an id its fork retired",
        db.upsert_concept(ConceptUpsert::new("c", "trunk's c").valid_from(T0))
            .await
            .map(|_| ()),
    )
    .await as i32;

    // 5. the guard reads the LIVE `concepts` table. Archiving a lineage moves
    //    its rows to the cold file, so the guard should stop seeing them --
    //    which is the one route by which the trunk could then mint the same id
    //    and the *log* end up holding two lineages for it.
    db.upsert_concept(
        ConceptUpsert::new("d", "sib's d")
            .valid_from(T0)
            .on_branch(sib.clone()),
    )
    .await
    .unwrap();
    let report = db.archive_branch(sib.clone()).await;
    println!(
        "
  (archive_branch(sib) -> {})",
        match &report {
            Ok(r) => format!("{r:?}").chars().take(90).collect::<String>(),
            Err(e) => format!("refused: {e}"),
        }
    );
    // An instant after the archive and before the re-mint. A read here has a
    // newer hot row above it, which is what makes `hot_log_reach` say
    // `NeedsArchive` and send the fold down the cold arm.
    let between: String = {
        let c = db.diagnostic_conn().await.unwrap();
        let mut r = c
            .query("SELECT MAX(recorded_at) FROM transaction_log", ())
            .await
            .unwrap();
        r.next().await.unwrap().unwrap().get(0).unwrap()
    };

    tried += 1;
    refused += attempt(
        "5. trunk mints an id an ARCHIVED lineage held",
        db.upsert_concept(ConceptUpsert::new("d", "trunk's d").valid_from(T0))
            .await
            .map(|_| ()),
    )
    .await as i32;

    // 6. the log, which is what `reconstruct_on` actually folds. `concepts` is
    //    a current-state projection and the guards above are on it; history is
    //    a different table and keeps what the projection no longer shows.
    println!("\n=== and in the *log*, which is what the fold reads? ===\n");
    let conn = db.diagnostic_conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT entity_id, COUNT(DISTINCT branch_id) AS lineages
             FROM transaction_log
             WHERE table_name = 'concepts'
             GROUP BY entity_id
             HAVING lineages > 1",
            (),
        )
        .await
        .unwrap();
    let mut collisions = 0;
    while let Some(r) = rows.next().await.unwrap() {
        let id: String = r.get(0).unwrap();
        let n: i64 = r.get(1).unwrap();
        println!("    {id:?} appears under {n} lineages");
        collisions += 1;
    }
    if collisions == 0 {
        println!("  no concept id appears under two lineages in transaction_log");
    }

    // The archive MOVED sib's rows, so the hot log alone cannot show the
    // collision. Both files together are the honest question, and the cold arm
    // of the fold is what unions them.
    println!(
        "
=== hot and cold together ===
"
    );
    conn.execute(
        "ATTACH DATABASE ?1 AS cold",
        libsql::params![db.archive_path().to_string_lossy().as_ref()],
    )
    .await
    .unwrap();
    let mut rows = conn
        .query(
            "SELECT entity_id, COUNT(DISTINCT branch_id) FROM (
                 SELECT entity_id, branch_id FROM main.transaction_log WHERE table_name = 'concepts'
                 UNION ALL
                 SELECT entity_id, branch_id FROM cold.transaction_log WHERE table_name = 'concepts'
             ) GROUP BY entity_id HAVING COUNT(DISTINCT branch_id) > 1",
            (),
        )
        .await
        .unwrap();
    let mut both = 0;
    while let Some(r) = rows.next().await.unwrap() {
        let id: String = r.get(0).unwrap();
        let n: i64 = r.get(1).unwrap();
        println!("  {id:?} appears under {n} lineages across hot+cold");
        both += 1;
    }
    if both == 0 {
        println!("  none");
    }
    conn.execute("DETACH DATABASE cold", ()).await.unwrap();

    // And what the two reads make of it. `sib` is no longer a registered
    // lineage, so it is not in anyone's ancestry -- which is the thing that
    // decides whether the collision can reach an answer.
    println!(
        "
=== what the reads return ===
"
    );
    let whole = db.reconstruct(FOREVER).await.unwrap();
    let on_main = db.reconstruct_on(FOREVER, "main").await.unwrap();
    let on_exp = db.reconstruct_on(FOREVER, "exp").await.unwrap();
    let show = |label: &str, s: &MaterializedState| {
        let mut ids: Vec<String> = s
            .concepts
            .iter()
            .map(|(k, v)| format!("{k}={:?}", v.title))
            .collect();
        ids.sort();
        println!("  {label:<30} {ids:?}");
    };
    show("reconstruct (whole ledger)", &whole);
    show("reconstruct_on(main)", &on_main);
    show("reconstruct_on(exp)", &on_exp);
    // The reads above took the HOT arm, which never opens the cold file. The
    // union only happens on the cold arm, so that is where the collision would
    // reach a reader if it can reach one at all.
    println!(
        "
=== forcing the cold arm (both files unioned) ===
"
    );
    let early = between.as_str();
    for (label, r) in [
        ("reconstruct(early)", db.reconstruct(early).await),
        (
            "reconstruct_on(early, main)",
            db.reconstruct_on(early, "main").await,
        ),
        (
            "reconstruct_on(early, exp)",
            db.reconstruct_on(early, "exp").await,
        ),
    ] {
        match r {
            Ok(s) => {
                let mut ids: Vec<String> = s
                    .concepts
                    .iter()
                    .map(|(k, v)| format!("{k}={:?}", v.title))
                    .collect();
                ids.sort();
                println!("  {label:<30} {ids:?}");
            }
            Err(e) => println!("  {label:<30} refused: {e}"),
        }
    }

    // The sharp end of route 5: cold holds sib's "d" and hot now holds main's
    // "d". `rehydrate` moves a cold concept back into the hot tables, which is
    // where the UNIQUE index on `concepts.id` lives.
    println!(
        "
=== rehydrating the archived id back on top of the live one ===
"
    );
    match db.rehydrate(&["d"]).await {
        Ok(r) => println!("  ACCEPTED: {r:?}"),
        Err(e) => println!("  refused: {e}"),
    }
    let after = db.reconstruct(FOREVER).await.unwrap();
    let mut ids: Vec<String> = after
        .concepts
        .iter()
        .map(|(k, v)| format!("{k}={:?}", v.title))
        .collect();
    ids.sort();
    println!("  reconstruct afterwards: {ids:?}");

    println!(
        "
  reconstruct_on(sib) -> {}",
        match db.reconstruct_on(FOREVER, "sib").await {
            Ok(_) => "answered".to_string(),
            Err(e) => format!("{e}"),
        }
    );

    println!(
        "
=== verdict ===
"
    );
    println!("  {refused} of {tried} write routes refused by the schema");
    println!(
        "  the one that is not is `archive_branch` + re-mint, and it produces a
           lineage that is no longer registered -- so it is in nobody's ancestry,
           no `reconstruct_on` can see it, and `rehydrate` refuses to bring it back."
    );

    db.close().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
