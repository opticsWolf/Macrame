//! The real readers against P-8's brute-force oracle (0.22.0 review queue, the
//! real-reader test the review asked for before M6-d's archive arm).
//!
//! The pour, the model and the oracle live in `common/m6_fixture.rs` so M6-d's
//! archive file can hold the SAME law against the ledger after an archive
//! session has trimmed it (P-4's own story).
//!
//! [P-8](m6_resolution_tests.rs) checked the §5.3 resolution law on fixture
//! tables. This file does what the review expected of that work's successor:
//! **the public readers, through the crate's own API, against the same walk's
//! law** — with the oracle's bookkeeping written here, as the acts land, and
//! never read back from the engine. A disagreement between the two sides is a
//! finding; the two sides deliberately share nothing but the file they sit in
//! (the store the pour wrote is the one thing both sides touch).
//!
//! Reader by reader, the law each is held to:
//!
//! - [`concepts_by_id`](macrame::Database::concepts_by_id) — the §5.3 walk:
//!   nearest first; a version covering the instant wins over a cut where both
//!   cover; a covering cut answers *absent* and stops the walk; and where the
//!   overlay decides nothing, the projection answers as the fold always did —
//!   one row per `(id, branch)` at the walk's caps ([D-310]), its window
//!   applied after ([D-303]'s within-pair law; [D-291]'s one-open guard keeps
//!   at most one candidate), win-or-absent with no fall-through past the
//!   mint lineage's row.
//! - [`reconstruct_on`](macrame::Database::reconstruct_on) — the same walk at
//!   the axis's far end: a version covers while open, a cut covers always,
//!   and where the walk says nothing the fold's newest row answers, the
//!   retired one removing its id ([D-312]'s close-and-retire are log rows).
//! - [`reconstruct`](macrame::Database::reconstruct) — the whole-ledger read:
//!   names no lineage, walks nothing; the newest belief per `(id, branch)`,
//!   the retired one removing its id. The branch's rows are per-lineage
//!   states no lineage-less read resolves by (D-310's cost 4: "the rule
//!   cannot be 'the most recent'"), so a whole-ledger read answers the
//!   creating lineage's rows and nothing else.
//! - [`reconstruct_as_of`](macrame::Database::reconstruct_as_of) — the
//!   valid-axis whole-ledger read: one current belief per valid start, a
//!   covering start answering it ([D-292]); the overlay tables are filtered
//!   out of the claim list, a retired covering claim removes its id, and two
//!   covering starts would be an error the write guard (D-291) keeps out.
//!
//! The scenario below carries every shape the queue named: several versions
//! and cuts per lineage (`c` runs through eras on the trunk plus version+cut
//! pairs on three forked lines), trunk edits before and after the fork (the
//! trunk superseded twice and retired twice while branches held caps across
//! and behind the edits), cutoffs between the rewrites (`r` walks every act
//! stamp plus one stamp before the ledger's first row), a standalone branch
//! cut (`b2` retires an inherited `d`), a branch-minted concept (`e` on
//! `b1`, visible on its own line and nowhere else), the windowed-supersede
//! shape whose far end leaves with the cut (§12.1's law), and supersede-
//! then-retire on the trunk (`c`: superseded at V7 and V12, retired at V13).
//!
//! The grids pin the two readers' asymmetry on purpose: at `(v=V4, b0, r ≥
//! C)` the per-lineage read answers ABSENT (the fold's one row is era two,
//! whose window does not contain `V4`) while the whole-ledger valid read
//! answers era one from its own partition start. Different laws, both
//! correct; a reader that conflates them fails here.

#[path = "common/m6_fixture.rs"]
mod fixture;
#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;

use fixture::*;
use macrame::temporal::AsOf;
// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// Every bitemporal cell of `concepts_by_id`: four viewers, three ids, one
/// instant per window day, one bound per act stamp plus pre-history.
#[tokio::test]
async fn a_concepts_by_id_matches_the_oracle_on_every_bitemporal_cell() {
    let harness = TestHarness::new();
    let (db, m) = hold(&harness).await;

    let bounds = act_bounds(&m);
    for r in &bounds {
        for k in 1..=13 {
            let at = v(k);
            for viewer in VIEWERS {
                for id in IDS {
                    by_id_cell(&db, &m, viewer, id, Some(&at), Some(r)).await;
                }
            }
        }
    }
}

/// The current-belief cells: no recorded bound, so the ancestor windows are
/// their fork points and the viewer's own cap unbounded — the live row is
/// the candidate (D-310's M4, today's law).
#[tokio::test]
async fn a_concepts_by_id_matches_the_oracle_in_current_belief() {
    let harness = TestHarness::new();
    let (db, m) = hold(&harness).await;

    for k in 1..=13 {
        let at = v(k);
        for viewer in VIEWERS {
            for id in IDS {
                by_id_cell(&db, &m, viewer, id, Some(&at), None).await;
            }
        }
    }
}

/// `reconstruct_on` at every act stamp and pre-history, per viewer: the walk
/// at the far end, then the fold's own answer.
#[tokio::test]
async fn a_reconstruction_on_matches_the_oracle_at_every_act_stamp() {
    let harness = TestHarness::new();
    let (db, m) = hold(&harness).await;

    for r in act_bounds(&m) {
        for viewer in VIEWERS {
            let state = db
                .reconstruct_on(&r, viewer)
                .await
                .unwrap_or_else(|e| panic!("reconstruct_on({r}, {viewer}): {e}"));
            for id in IDS {
                let want = m.on(viewer, id, &r);
                let got = state.concepts.get(id).map(|n| n.title.clone());
                state_cell(
                    got,
                    want.as_ref(),
                    format!("reconstruct_on({r}, {viewer}) :: {id}"),
                );
            }
        }
    }
}

/// `reconstruct` (no lineage) at every act stamp and pre-history: the
/// whole-ledger fold, D-310's creating-lineage law.
#[tokio::test]
async fn a_reconstruction_matches_the_oracle_at_every_act_stamp() {
    let harness = TestHarness::new();
    let (db, m) = hold(&harness).await;

    for r in act_bounds(&m) {
        let state = db
            .reconstruct(&r)
            .await
            .unwrap_or_else(|e| panic!("reconstruct({r}): {e}"));
        for id in IDS {
            let want = m.whole_recorded(id, &r);
            let got = state.concepts.get(id).map(|n| n.title.clone());
            state_cell(got, want.as_ref(), format!("reconstruct({r}) :: {id}"));
        }
    }
}

/// `reconstruct_as_of` on every bitemporal cell — the valid-axis fold's own
/// law (D-292's one current belief per start), plus the valid-only cells that
/// leave the belief unbounded.
#[tokio::test]
async fn a_reconstruction_as_of_matches_the_oracle_on_every_bitemporal_cell() {
    let harness = TestHarness::new();
    let (db, m) = hold(&harness).await;

    for r in act_bounds(&m) {
        for k in 1..=13 {
            let at = v(k);
            let state = db
                .reconstruct_as_of(AsOf::bitemporal(at.clone(), r.clone()))
                .await
                .unwrap_or_else(|e| panic!("reconstruct_as_of({at}, {r}): {e}"));
            for id in IDS {
                let want = m.whole_valid(id, &at, Some(&r));
                let got = state.concepts.get(id).map(|n| n.title.clone());
                state_cell(
                    got,
                    want.as_ref(),
                    format!("reconstruct_as_of({at}, {r}) :: {id}"),
                );
            }
        }
    }

    // Valid-only: the whole log's newest-per-start rows, no recorded bound.
    for k in 1..=13 {
        let at = v(k);
        let state = db
            .reconstruct_as_of(AsOf::valid_at(at.clone()))
            .await
            .unwrap_or_else(|e| panic!("reconstruct_as_of(valid {at}): {e}"));
        for id in IDS {
            let want = m.whole_valid(id, &at, None);
            let got = state.concepts.get(id).map(|n| n.title.clone());
            state_cell(
                got,
                want.as_ref(),
                format!("reconstruct_as_of(valid {at}) :: {id}"),
            );
        }
    }
}
