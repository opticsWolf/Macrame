//! M6-f's post-review write-law corrections on the overlay (the review's
//! confirmed finding on [`reject_overlapping_w2`]'s start-instant-only check,
//! and the route misreading a re-write on an adopted id as fresh-foreignness).
//!
//! The review ran against M6-f's HEAD (`2c4e03f`) and proved the hole with a
//! probe: vista adopts `late` as `[T4, FOREVER)` — the adoption act is a first
//! version on vista's own lineage, the exact shape [D-315]'s act produces —
//! and then writes `[T_LATE, T6)`. The write succeeded, because every guard
//! the write meets answers **at the write's start only**:
//!
//! - `route_concept`'s resolution walk answers the stance at `valid_from`
//!   (nothing is open at `T_LATE` on vista: the adopted row starts at `T4`
//!   after it), so the route fell to the vista check, which sent the id off
//!   the path to a *fresh* adoption;
//! - `reject_overlapping_w2`'s own check is the same instant read, so the
//!   interval shape (`[T4, FOREVER)` overlapping `[T_LATE, T6)` on
//!   `[T4, T6)`) passed it too;
//! - `trg_concept_versions_single_open` fires only for an open-ended `NEW`.
//!
//! The result was two version rows on `(late, vista)` covering `T4..T6` at
//! once, and a read in the middle silently picking one. The fixes carry the
//! law to both spaces it lived behind:
//!
//! - the route knows the writer's own overlay history (`own_version_row_
//!   exists`): an id the lineage has ever-versioned is the lineage's own
//!   matter from its first row on, and reaches the ordinary W2 arms with the
//!   walk's blankness where the walk answers absent — so a re-write refuses
//!   (or takes over) by the overlay's own law, and a re-retirement is
//!   retired by vista's own vocabulary rather than misnamed an adoption
//!   refusal. A truly fresh foreign id still routes to [D-315]'s act.
//! - `reject_overlapping_w2` runs [D-291]'s whole partition on the lineage's
//!   own rows, not only its point check: any overlap in the half-open sense
//!   refuses, with the same-start rewrite rule carved out (the D-291 heal
//!   and the supersede's close stay one row's worth). The batch rides the
//!   same guard: its W2 phase calls the same [`upsert_w2_version`], and its
//!   routing change is [`route_concept`]'s, read per concept.
//! - `trg_concept_versions_single_open` widens from "open-ended `NEW`"
//!   to the interval predicate itself (same-start carve-out kept), behind
//!   the v29 rung that drops and re-issues it — the [D-126] recipe, since
//!   `CREATE TRIGGER IF NOT EXISTS` keeps an old body on every file the
//!   baseline has already stamped.

#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;

use macrame::prelude::*;
use macrame::BranchId;
use macrame::WriteBatch;

const T_LATE: &str = "2026-02-01T00:00:00.000000Z";
const T4: &str = "2026-03-01T00:00:00.000000Z";
const T6: &str = "2026-04-01T00:00:00.000000Z";
const T_END1: &str = "2026-04-30T00:00:00.000000Z";
const T5: &str = "2026-05-01T00:00:00.000000Z";
const FOREVER: &str = "9999-12-31T23:59:59.999999Z";

async fn tick() {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}

async fn scalar(db: &Database, sql: &str) -> i64 {
    let mut rows = db.read_conn().query(sql, ()).await.unwrap();
    let v: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    v
}

/// The vista store: the trunk mints `early` at `T0`, forks `vista` at `F0`,
/// then mints `late` at `T_LATE` — recorded after the fork cutoff, so `late`
/// is invisible to vista's resolve until vista adopts it ([D-315]'s act).
async fn vista_store(harness: &TestHarness) -> std::sync::Arc<Database> {
    const T0: &str = "2026-01-01T00:00:00.000000Z";
    const F0: &str = "2026-01-05T00:00:00.000000Z";
    let db: std::sync::Arc<Database> =
        std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());
    db.upsert_concept(
        ConceptUpsert::new("early", "the trunk's early belief")
            .valid_from(T0)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    db.fork_at(BranchId::new("vista").unwrap(), BranchId::main(), F0)
        .await
        .unwrap();
    tick().await;
    db.upsert_concept(
        ConceptUpsert::new("late", "the trunk's late mint")
            .valid_from(T_LATE)
            .valid_to(FOREVER),
    )
    .await
    .unwrap();
    tick().await;
    db
}

/// Vista adopts `late` (its own version row spanning `[T4, FOREVER)`), then
/// writes a plain bounded window under it. The review's confirmed cell: the
/// second write is refused by the overlay's own law, the ledger ends with the
/// one row it began with, and a read at `[T4, T6)`'s middle still answers the
/// adopted belief rather than picking among two rows the law never admitted.
#[tokio::test]
async fn a_write_under_the_writer_s_own_rows_refuses_and_the_row_it_guards_stays_the_answer() {
    let harness = TestHarness::new();
    let db = vista_store(&harness).await;
    let view = db.view(BranchId::new("vista").unwrap());

    view.upsert_concept(
        ConceptUpsert::new("late", "vista's adopted belief")
            .valid_from(T4)
            .valid_to(FOREVER),
    )
    .await
    .expect("the adoption is the ordinary first version [D-315] names");
    tick().await;

    let refused = view
        .upsert_concept(
            ConceptUpsert::new("late", "the overlapping bounded rewrite")
                .valid_from(T_LATE)
                .valid_to(T6),
        )
        .await;
    let err = match refused {
        Ok(()) => panic!(
            "the review's confirmed cell: the bounded rewrite over the \
             lineage's own open belief stored both"
        ),
        Err(e) => e,
    };
    let text = err.to_string();
    assert!(
        text.contains("open") || text.contains("overlap"),
        "the refusal names the law the write broke, not engine text: {text}"
    );

    // The ledger ends where the write refused it: one version row on the
    // pair, no cut anywhere, and the adopted belief still the walk's answer
    // in the overlap the refusal covers.
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_versions WHERE id = 'late' AND branch_id = 'vista'"
        )
        .await,
        1,
        "the refused window did not join the own rows"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_cuts WHERE id = 'late' AND branch_id = 'vista'"
        )
        .await,
        0,
        "nothing else was cut by the refused act"
    );

    let mid = "2026-03-15T00:00:00.000000Z";
    let rows = db
        .concepts_by_id(
            &["late".to_string()],
            ReadPlan::new()
                .on(BranchId::new("vista").unwrap())
                .valid_at(mid)
                .recorded_at("2027-06-01T00:00:00.000000Z"),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the middle of the overlap answers one belief"
    );
    assert!(
        rows[0].title.contains("adopted"),
        "the answer is the belief the law admitted: {rows:?}"
    );
}

/// The same law, one shape deeper: with only *bounded* rows of its own (the
/// route's walk answers Nothing at the rewrite's start, and the path's
/// open-intruder scan only seeks open-ended rows), the first review shape
/// overlapped a recognizer further down. The interval partition in
/// `reject_overlapping_w2` is what refuses it, and it refuses as the
/// [D-291] partition names a bounded-vs-bounded clash: not
/// `ConceptSingleOpen` (two open intervals) but the overlap.
#[tokio::test]
async fn the_interval_partition_names_a_bounded_clash_the_overlap_it_is() {
    let harness = TestHarness::new();
    let db = vista_store(&harness).await;
    let view = db.view(BranchId::new("vista").unwrap());

    // The bounded first belief: nothing on vista is ever *open*.
    view.upsert_concept(
        ConceptUpsert::new("late", "vista's bounded adoption")
            .valid_from(T4)
            .valid_to(T_END1),
    )
    .await
    .expect("the bounded adoption is Vista's ordinary first version");
    tick().await;

    // Rewrites with a start inside it, ending past its end: every guard
    // at the write's start stays silent.
    let refused = view
        .upsert_concept(
            ConceptUpsert::new("late", "the overlapping rewrite")
                .valid_from("2026-03-28T00:00:00.000000Z")
                .valid_to(T5),
        )
        .await;
    match refused {
        Ok(()) => panic!("a bounded rewrite over a bounded own row stored both"),
        Err(e) => {
            let text = e.to_string();
            assert!(
                text.contains("overlap") && !text.contains("already has"),
                "the [D-291] partition names a bounded-vs-bounded clash the \
                 overlap it is: {text}"
            );
        }
    }

    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_versions WHERE id = 'late' AND branch_id = 'vista'"
        )
        .await,
        1,
        "the refused window did not join the own rows"
    );

    // The adjacent shape stays legal: the same-[D-291] interval half that
    // refuses a clash admits the belief that begins where the own row ends.
    view.upsert_concept(
        ConceptUpsert::new("late", "the adjacent continuation")
            .valid_from(T5)
            .valid_to(FOREVER),
    )
    .await
    .expect("the adjacent continuation is not a clash");
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_versions WHERE id = 'late' AND branch_id = 'vista'"
        )
        .await,
        2,
        "the adjacent window joined the pair"
    );
}

/// The retirement of a writer's own adopted id, at an instant the walk
/// answers Tellingly empty because the adoption begins later. The old
/// dispatch misnamed it a *fresh* adoption refusal (the review's plausible
/// finding): vista has in fact adopted `late`. The route now recognizes the
/// writer's own overlay history, so the retirement goes to the overlay's own
/// vocabulary — and an instant where the lineage holds no belief to retire
/// is refused with the sentence the act names, the reviewer's own wording,
/// and not a single row is written on either vocabulary.
#[tokio::test]
async fn a_re_retirement_of_an_adopted_id_is_recognized_and_refused_as_such() {
    let harness = TestHarness::new();
    let db = vista_store(&harness).await;
    let log_before_adoption = scalar(&db, "SELECT COUNT(*) FROM transaction_log").await;
    let view = db.view(BranchId::new("vista").unwrap());

    view.upsert_concept(
        ConceptUpsert::new("late", "vista's adopted belief")
            .valid_from(T4)
            .valid_to(FOREVER),
    )
    .await
    .expect("the adoption rides in first");
    tick().await;

    // Vista retires at an instant before its adoption begins. The dispatch's
    // old answer named a fresh adoption; the new answer names what the act
    // asked an instant for: a belief worth retiring at `T_LATE`, and Vista
    // has none there (its own row starts at `T4`).
    let refused = view
        .upsert_concept(
            ConceptUpsert::new("late", "the retirement attempt")
                .retired(true)
                .valid_from(T_LATE)
                .valid_to(FOREVER),
        )
        .await;
    match refused {
        Ok(()) => panic!("the retirement wrote a vocabulary whose instant held no belief"),
        Err(e) => {
            let text = e.to_string();
            assert!(
                text.contains("nothing visible") && !text.contains("fresh adoption"),
                "the retirement refusal names what the act cannot see, in the \
                 reviewer's wording: {text}"
            );
        }
    }

    // Nothing was written: the ledger grew nothing but the adoption's own
    // rows, and there is no cut anywhere.
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM transaction_log").await,
        log_before_adoption + 1,
        "only the adoption's row carries the ledger onward"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_cuts WHERE id = 'late' AND branch_id = 'vista'"
        )
        .await,
        0,
        "no cut row stands for an act that named nothing"
    );

    // And the true [D-315] retirement works: vista retires at a window
    // STARTING AFTER its adoption — the instant the lineage names its open
    // belief at, and the overlay's own close rule runs on it (a retirement
    // at the adoption's own start would ask the close law to produce an
    // empty interval, which is the same-instance refusal the D-312 forward
    // law keeps).
    view.upsert_concept(
        ConceptUpsert::new("late", "the retirement after the window")
            .retired(true)
            .valid_from(T6)
            .valid_to(FOREVER),
    )
    .await
    .expect("the retirement closes vista's own belief");
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) FROM concept_versions \
                 WHERE id = 'late' AND branch_id = 'vista' \
                   AND valid_to = '{T6}'"
            )
        )
        .await,
        1,
        "the retirement closed the adopted window at the retirement's start"
    );
}

/// The batch rides the same route and the same guard, so its two acts on an
/// adopted id reproduce the two refusals, and the retired shape that IS
/// visible retires by the overlay's close rule.
#[tokio::test]
async fn the_batch_s_acts_on_an_adopted_id_ride_the_same_arms() {
    let harness = TestHarness::new();
    let db = vista_store(&harness).await;
    let view = db.view(BranchId::new("vista").unwrap());

    view.upsert_concept(
        ConceptUpsert::new("late", "vista's adopted belief")
            .valid_from(T4)
            .valid_to(FOREVER),
    )
    .await
    .expect("adoption first");
    tick().await;

    // One. The overlapping bounded rewrite in a batch is refused, and the
    // batch writes nothing for it.
    let refused = db
        .write_batch(
            WriteBatch::new().concept(
                ConceptUpsert::new("late", "the overlapping bounded rewrite")
                    .on_branch(BranchId::new("vista").unwrap())
                    .valid_from(T_LATE)
                    .valid_to(T6),
            ),
        )
        .await;
    assert!(
        refused.is_err(),
        "the batch refuses the window its single-write shape refuses"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_versions WHERE id = 'late' AND branch_id = 'vista'"
        )
        .await,
        1
    );

    // Two. The re-retirement at the invisible instant is refused with the
    // same sentence the single write refuses with.
    let refused = db
        .write_batch(
            WriteBatch::new().concept(
                ConceptUpsert::new("late", "the retirement attempt")
                    .retired(true)
                    .on_branch(BranchId::new("vista").unwrap())
                    .valid_from(T_LATE)
                    .valid_to(FOREVER),
            ),
        )
        .await;
    assert!(
        refused.is_err(),
        "the batch refuses the retirement its single-write shape refuses"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_cuts WHERE id = 'late' AND branch_id = 'vista'"
        )
        .await,
        0
    );

    // Three. The visible retirement closes vista's own belief, the
    // single-write act's shape: the retirement starts after the adoption,
    // where the D-312 forward law calls for a close.
    db.write_batch(
        WriteBatch::new().concept(
            ConceptUpsert::new("late", "the retirement after the window")
                .retired(true)
                .on_branch(BranchId::new("vista").unwrap())
                .valid_from(T6)
                .valid_to(FOREVER),
        ),
    )
    .await
    .expect("the batched retirement closes the adopted belief");
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) FROM concept_versions \
                 WHERE id = 'late' AND branch_id = 'vista' \
                   AND valid_to = '{T6}'"
            )
        )
        .await,
        1,
        "the batched retirement closed the adopted window"
    );
}

/// The trigger carries the interval law to a writer the API cannot see — the
/// same [D-126] recipe the v26 → v27 rung used for the overlay's row laws
/// ball these into a v29 rung here. Two cells: a bounded insert under an
/// open-ended own row is refused with the trigger's sentence, and an
/// adjacent insert past a bounded own row goes in.
#[tokio::test]
async fn a_direct_writer_holds_the_interval_law_too() {
    let harness = TestHarness::new();
    let db = vista_store(&harness).await;
    let view = db.view(BranchId::new("vista").unwrap());

    view.upsert_concept(
        ConceptUpsert::new("late", "vista's adopted belief")
            .valid_from(T4)
            .valid_to(FOREVER),
    )
    .await
    .expect("adoption first — the row the direct writer meets");

    let raw = raw_conn(db.path()).await;
    // Bounded, overlapping the adoption: refused by the widened when.
    let refused = raw
        .execute(
            "INSERT INTO concept_versions \
             (id, branch_id, valid_from, valid_to, title, content, extra, recorded_at) \
             VALUES ('late', 'vista', ?1, ?2, '', '', '{}', '2026-06-01T00:00:00.000000Z')",
            libsql::params![T_LATE, T6],
        )
        .await;
    match refused {
        Ok(_) => panic!("a direct bounded insert under an open own row was stored"),
        Err(e) => {
            let m = e.to_string();
            assert!(
                m.contains("already has an open interval"),
                "the refusal is the single-open sentence, not engine text: {m}"
            );
        }
    }

    // The adjacent-after-bounded shape — a second concept id, and a window
    // nothing on vista reaches — is admitted.
    let ok = raw
        .execute(
            "INSERT INTO concept_versions \
             (id, branch_id, valid_from, valid_to, title, content, extra, recorded_at) \
             VALUES ('early', 'vista', ?1, ?2, '', '', '{}', '2026-06-01T00:00:00.000100Z')",
            libsql::params![T4, T6],
        )
        .await;
    assert!(
        ok.is_ok(),
        "the adjacent window is admitted to the direct writer too: {ok:?}"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM concept_versions WHERE id = 'early' AND branch_id = 'vista'"
        )
        .await,
        1
    );
}

async fn raw_conn(path: &std::path::Path) -> libsql::Connection {
    libsql::Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}
