//! P-7 of `docs/Macrame Update Plan v0.22.0 - Amendment 2.md` (§8): is there
//! any write-path check that refuses an **edge** whose endpoint has no belief
//! visible on the writing lineage?
//!
//! Why it gates D-315 ([§6](Macrame%20Update%20Plan%20v0.22.0 - Amendment 2.md)):
//! if an endpoint-visibility check exists, a batch must write an adoption
//! before its edges name the adopted id. If none exists, the endpoint law is
//! `concepts(id)`'s foreign key alone (existence anywhere, mint lineage
//! irrelevant), and D-308's concepts-before-edges ordering already covers
//! in-batch adoptions — no new batch-order machinery.
//!
//! Four measurements, all on the v25 write path with no schema change:
//!
//! 1. **Ghost endpoint** — edge to an id that exists nowhere: the FK refuses.
//!    This is the endpoint law, spelled out by a refusal.
//! 2. **Invisible endpoint on a fork** — `vista` forks `main` at T0; `main`
//!    mints `late` after the fork; an edge naming `late` written ON `vista`:
//!    accepted or refused?
//! 3. **An endpoint minted on a sibling** — `sib` mints `sibborn` after the
//!    fork; the trunk writes an edge naming `sibborn`, which it cannot see:
//!    accepted or refused?
//! 4. **The D-225 baseline holds** — a same-lineage overlapping duplicate is
//!    still refused (the probe did not accidentally soften edge machinery),
//!    and an in-batch edge names a concept the same batch writes first
//!    (D-308's atomicity, the precedent adoption rides).
//!
//! Run with:  cargo run --release --example m6_p7_adoption_edge_probe

use macrame::prelude::*;
use macrame::WriteBatch;

const T0: &str = "2026-01-01T00:00:00.000000Z";
const T1: &str = "2026-01-02T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";

/// The refusal, spelled however the crate spells it, or the acceptance.
async fn attempt(label: &str, r: Result<()>) -> bool {
    match r {
        Err(e) => {
            println!("  {label:<62} REFUSED  {e:?}");
            true
        }
        Ok(()) => {
            println!("  {label:<62} ACCEPTED");
            false
        }
    }
}

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join(format!("macrame_m6_p7_probe_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    println!(
        "m6_p7_adoption_edge_probe — crate {}",
        env!("CARGO_PKG_VERSION"),
    );

    let db = Database::open(dir.join("p.db")).await.unwrap();

    let vista = BranchId::new("vista").unwrap();
    let sib = BranchId::new("sib").unwrap();

    // The trunk holds alpha and beta before anything forks, so both forks
    // inherit them and every edge below is about the ENDPOINT's visibility,
    // not the source's.
    for (id, title) in [("alpha", "alpha"), ("beta", "beta")] {
        db.upsert_concept(
            ConceptUpsert::new(id, title)
                .valid_from(T0)
                .valid_to(FOREVER),
        )
        .await
        .unwrap();
    }

    // An edge on the trunk before forks exist, because the D-225 baseline arm
    // needs an anchor of its own.
    db.assert_edge(EdgeAssertion::new("alpha", "beta", "linked").valid_from(T0))
        .await
        .unwrap();

    db.fork(vista.clone(), BranchId::main()).await.unwrap();
    db.fork(sib.clone(), BranchId::main()).await.unwrap();

    // main mints AFTER the forks: invisible to both.
    db.upsert_concept(ConceptUpsert::new("late", "late").valid_from(T1))
        .await
        .unwrap();

    // sib mints sibborn on itself; the trunk cannot see it.
    db.write_batch(
        WriteBatch::new()
            .concept(ConceptUpsert::new("sibborn", "sibborn").valid_from(T1))
            .edge(EdgeAssertion::new("sibborn", "alpha", "linked").valid_from(T1))
            .on_branch(sib.clone()),
    )
    .await
    .unwrap();

    println!("\n-- the endpoint law, spelled by refusals and acceptances --");

    // 1. A ghost id: the FK alone.
    attempt(
        "trunk edge to an id that exists nowhere (ghost)",
        db.assert_edge(EdgeAssertion::new("alpha", "ghost", "linked").valid_from(T1))
            .await,
    )
    .await;

    // 2. vista names a main-minted id recorded after vista's fork.
    attempt(
        "vista edge to 'late' (minted on main after vista forked)",
        db.write_batch(
            WriteBatch::new()
                .on_branch(vista.clone())
                .edge(EdgeAssertion::new("alpha", "late", "linked").valid_from(T1)),
        )
        .await
        .map(|_| ()),
    )
    .await;

    // 3. the trunk names an id minted on sib.
    attempt(
        "trunk edge to 'sibborn' (minted on sib; trunk cannot see it)",
        db.write_batch(
            WriteBatch::new().edge(EdgeAssertion::new("alpha", "sibborn", "linked").valid_from(T1)),
        )
        .await
        .map(|_| ()),
    )
    .await;

    // 4a. D-225 refuses an overlapping-with-different-start assertion: the
    // identical re-assert above was accepted, which is its own finding.
    attempt(
        "trunk overlapping-different-start duplicate of alpha->beta",
        db.assert_edge(EdgeAssertion::new("alpha", "beta", "linked").valid_from(T1))
            .await,
    )
    .await;

    // 4b. an edge naming a concept the same batch just wrote (D-308 atomicity).
    attempt(
        "batch: concept 'inbatch' then edge naming it, one transaction",
        db.write_batch(
            WriteBatch::new()
                .concept(ConceptUpsert::new("inbatch", "inbatch").valid_from(T1))
                .edge(EdgeAssertion::new("alpha", "inbatch", "linked").valid_from(T1)),
        )
        .await
        .map(|_| ()),
    )
    .await;

    // And the reading side confirms what the ledger recorded: vista's fold of
    // the trunk edge to 'late' shows vista's own edge, while vista's view of
    // 'late' the concept stays whatever the mint tells it (nothing new here --
    // this is the status quo the overlay generalizes).
    println!(
        "\ndirect raw INSERT of a vista edge to 'late': {:?}",
        db.raw()
            .connect()
            .unwrap()
            .execute(
                "INSERT INTO links (source_id, target_id, edge_type, valid_from, valid_to, \
                 weight, properties, branch_id, recorded_at) \
                 VALUES ('alpha','late','raw-linked','2026-01-05T00:00:00.000000Z', \
                 '9999-12-31T23:59:59.999999Z', 1, '{}','vista', \
                 '2026-01-05T00:00:00.000000Z')",
                (),
            )
            .await
            .err()
            .map(|e| e.to_string())
    );

    println!("\nP-7: the law above is what adoption's batch-order answer assumes.");
}
