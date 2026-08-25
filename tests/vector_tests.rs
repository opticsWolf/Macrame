#[path = "common/harness.rs"]
mod harness;

use harness::TestHarness;
use macrame::error::DbError;
use macrame::graph::vector_filter::{CandidateCount, CostEstimator, VectorFilterStrategy};
use macrame::schema::migrations;
use macrame::vector::{
    declared_dimension, reciprocal_rank_fusion, register_model, registered_models, search_vector,
    upsert_embedding, EmbeddingCodec, ModelName,
};

const TS: &str = "2026-01-01T00:00:00.000000Z";

/// A migrated database with two concepts, and the writer's PRAGMAs applied.
async fn seeded(harness: &TestHarness) -> (libsql::Database, libsql::Connection) {
    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    conn.execute("PRAGMA foreign_keys = ON", ()).await.unwrap();
    migrations::run(&conn).await.unwrap();
    for id in ["c0", "c1", "c2"] {
        conn.execute(
            "INSERT INTO concepts (id, title, valid_from, recorded_at) VALUES (?1, 'N', ?2, ?2)",
            libsql::params![id, TS],
        )
        .await
        .unwrap();
    }
    (db, conn)
}

fn model() -> ModelName {
    ModelName::new("probe_v1").unwrap()
}

/// Little-endian F32 bytes, the wire form of an F32_BLOB.
fn le(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

async fn count(conn: &libsql::Connection, sql: &str) -> i64 {
    conn.query(sql, ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}

/// Registration creates the table *and* its DiskANN index, and the dimension
/// is readable back out of the schema rather than remembered by the crate.
#[tokio::test]
async fn registering_a_model_creates_its_table_and_index() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;
    let m = model();

    register_model(&conn, &m, 4).await.unwrap();

    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{}'",
                m.table()
            )
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='{}'",
                m.index()
            )
        )
        .await,
        1,
        "the index is not optional: without it the engine accepts any dimension"
    );

    assert_eq!(declared_dimension(&conn, &m).await.unwrap(), 4);
    assert_eq!(registered_models(&conn).await.unwrap(), vec![m.clone()]);

    // Idempotent, as an application calling it on every start requires.
    register_model(&conn, &m, 4).await.unwrap();
    assert_eq!(registered_models(&conn).await.unwrap().len(), 1);
}

/// **The regression that this phase would otherwise have introduced.**
///
/// `verify()` used to require exactly four tables in `sqlite_master`. Registering
/// a model adds `embeddings_*`, and libSQL's vector index adds shadow tables and
/// a shadow index of its own — so on the next open, migration verification
/// failed and a perfectly healthy database refused to open. Nothing about the
/// ledger had changed; the check was counting instead of looking.
#[tokio::test]
async fn a_database_with_a_registered_model_still_opens() {
    let harness = TestHarness::new();
    {
        let (db, conn) = seeded(&harness).await;
        register_model(&conn, &model(), 4).await.unwrap();
        upsert_embedding(&conn, &model(), "c0", &[1.0, 0.0, 0.0, 0.0])
            .await
            .unwrap();
        drop(conn);
        drop(db);
    }

    let db = libsql::Builder::new_local(&harness.db_path)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    migrations::run(&conn)
        .await
        .expect("a database carrying a registered model must reopen");

    assert_eq!(declared_dimension(&conn, &model()).await.unwrap(), 4);
}

/// The crate-level check reports the *declared* dimension, which is the number
/// the caller needs and the one the engine will enforce.
///
/// Before this phase, `search_vector` called `encode(query_vec,
/// query_vec.len(), …)` — comparing the length against itself — so `DimMismatch`
/// could not be produced through the search path at all.
#[tokio::test]
async fn a_wrong_dimension_is_refused_with_the_declared_dimension() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;
    let m = model();
    register_model(&conn, &m, 4).await.unwrap();

    let err = upsert_embedding(&conn, &m, "c0", &[1.0, 2.0])
        .await
        .unwrap_err();
    match err {
        DbError::DimMismatch {
            got,
            expected,
            model,
        } => {
            assert_eq!((got, expected, model.as_str()), (2, 4, "probe_v1"));
        }
        other => panic!("expected DimMismatch, got {other:?}"),
    }

    let err = search_vector(&conn, &[1.0, 2.0, 3.0], &m, 5)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            DbError::DimMismatch {
                got: 3,
                expected: 4,
                ..
            }
        ),
        "search must check the query vector too, got {err:?}"
    );

    assert_eq!(
        count(&conn, &format!("SELECT COUNT(*) FROM {}", m.table())).await,
        0
    );
}

/// **The storage layer enforces the dimension only because the index exists.**
///
/// §4.1 claimed `F32_BLOB(n)` rejects a wrong-length vector at insert time and
/// that the crate-level check was there "for error quality" while the engine's
/// was there "for correctness". Measured against libSQL 0.9.30 that is false as
/// stated: with no vector index the engine accepts the row. The index is what
/// enforces, so this test pins both halves — the guarantee we do have, and the
/// one we do not — because `register_model` creating the index is the only
/// reason the first half holds.
#[tokio::test]
async fn the_engine_enforces_dimension_only_where_the_index_exists() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;
    let m = model();
    register_model(&conn, &m, 4).await.unwrap();

    // Bypass the crate check entirely: hand the engine a raw two-float blob.
    let rejected = conn
        .execute(
            &format!(
                "INSERT INTO {} (concept_id, embedding) VALUES ('c0', ?1)",
                m.table()
            ),
            libsql::params![le(&[1.0, 2.0])],
        )
        .await;
    assert!(
        rejected.is_err(),
        "the indexed table must reject a short vector"
    );
    assert_eq!(
        count(&conn, &format!("SELECT COUNT(*) FROM {}", m.table())).await,
        0,
        "a rejected vector must not leave a row behind"
    );

    // And the converse, which is why the index is created with the table: an
    // unindexed F32_BLOB column enforces nothing.
    conn.execute(
        "CREATE TABLE unindexed (concept_id TEXT PRIMARY KEY, embedding F32_BLOB(4) NOT NULL)",
        (),
    )
    .await
    .unwrap();
    let accepted = conn
        .execute(
            "INSERT INTO unindexed VALUES ('c0', ?1)",
            libsql::params![le(&[1.0, 2.0])],
        )
        .await;
    assert!(
        accepted.is_ok(),
        "if this ever starts failing, libSQL has begun enforcing the column type \
         itself and the §4.1 correction (D-037) should be revisited"
    );
}

/// Nearest first, through the DiskANN index rather than a table scan.
#[tokio::test]
async fn search_returns_neighbours_nearest_first() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;
    let m = model();
    register_model(&conn, &m, 4).await.unwrap();

    upsert_embedding(&conn, &m, "c0", &[1.0, 0.0, 0.0, 0.0])
        .await
        .unwrap();
    upsert_embedding(&conn, &m, "c1", &[0.9, 0.1, 0.0, 0.0])
        .await
        .unwrap();
    upsert_embedding(&conn, &m, "c2", &[0.0, 0.0, 0.0, 1.0])
        .await
        .unwrap();

    let hits = search_vector(&conn, &[1.0, 0.0, 0.0, 0.0], &m, 3)
        .await
        .unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].concept_id, "c0");
    assert_eq!(hits[1].concept_id, "c1");
    assert_eq!(hits[2].concept_id, "c2");
    assert!(hits[0].score <= hits[1].score && hits[1].score <= hits[2].score);

    let top1 = search_vector(&conn, &[0.0, 0.0, 0.0, 1.0], &m, 1)
        .await
        .unwrap();
    assert_eq!(top1.len(), 1);
    assert_eq!(top1[0].concept_id, "c2");

    assert!(search_vector(&conn, &[1.0, 0.0, 0.0, 0.0], &m, 0)
        .await
        .unwrap()
        .is_empty());
}

/// Re-embedding replaces the vector: one row per concept per model, because an
/// embedding is derived and carries no history of its own (Doctrine VII).
#[tokio::test]
async fn re_embedding_replaces_rather_than_accumulates() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;
    let m = model();
    register_model(&conn, &m, 4).await.unwrap();

    upsert_embedding(&conn, &m, "c0", &[1.0, 0.0, 0.0, 0.0])
        .await
        .unwrap();
    upsert_embedding(&conn, &m, "c0", &[0.0, 0.0, 0.0, 1.0])
        .await
        .unwrap();

    assert_eq!(
        count(&conn, &format!("SELECT COUNT(*) FROM {}", m.table())).await,
        1
    );
    let hits = search_vector(&conn, &[0.0, 0.0, 0.0, 1.0], &m, 1)
        .await
        .unwrap();
    assert_eq!(hits[0].concept_id, "c0");
    assert!(
        hits[0].score < 0.001,
        "the newest vector must be the one stored"
    );
}

/// **Doctrine VII, the half that needed Phase 3 to become testable.**
///
/// A vector is a derived artifact and never enters the ledger. The static test
/// in `doctrine_static_tests.rs` proves no trigger *payload* mentions one; this
/// proves the stronger thing now that the tables exist — writing embeddings adds
/// nothing to `transaction_log`, and no trigger is attached to the table that
/// could.
#[tokio::test]
async fn embeddings_never_enter_the_ledger() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;
    let m = model();
    register_model(&conn, &m, 4).await.unwrap();

    let before = count(&conn, "SELECT COUNT(*) FROM transaction_log").await;
    for (id, v) in [
        ("c0", [1.0f32, 0.0, 0.0, 0.0]),
        ("c1", [0.0, 1.0, 0.0, 0.0]),
        ("c2", [0.0, 0.0, 1.0, 0.0]),
    ] {
        upsert_embedding(&conn, &m, id, &v).await.unwrap();
    }

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM transaction_log").await,
        before,
        "an embedding write reached transaction_log; Doctrine VII excludes vectors"
    );
    assert_eq!(
        count(
            &conn,
            &format!(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND tbl_name='{}'",
                m.table()
            )
        )
        .await,
        0,
        "a trigger on the embeddings table could carry a vector into the ledger"
    );
}

/// An embedding references a concept, so it cannot outlive one that never
/// existed. The FK is what keeps the per-model tables from drifting into a
/// second, unreferenced population of ids.
#[tokio::test]
async fn an_embedding_cannot_reference_a_concept_that_does_not_exist() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;
    let m = model();
    register_model(&conn, &m, 4).await.unwrap();

    let err = upsert_embedding(&conn, &m, "no_such_concept", &[1.0, 0.0, 0.0, 0.0]).await;
    assert!(err.is_err(), "an orphan embedding must be refused");
}

/// An unregistered model is a typed error naming the missing table, not an
/// opaque `no such table` from the engine.
#[tokio::test]
async fn an_unregistered_model_is_a_typed_error() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;
    let m = ModelName::new("never_registered").unwrap();

    match search_vector(&conn, &[1.0, 0.0], &m, 3).await.unwrap_err() {
        DbError::ModelNotRegistered { model, table } => {
            assert_eq!(model, "never_registered");
            assert_eq!(table, "embeddings_never_registered");
        }
        other => panic!("expected ModelNotRegistered, got {other:?}"),
    }
    assert!(declared_dimension(&conn, &m).await.is_err());
    assert!(registered_models(&conn).await.unwrap().is_empty());
}

/// Registering a name that already exists at another dimension is refused
/// rather than silently no-opped by `IF NOT EXISTS`, which would leave the
/// caller believing a dimension that is not the one in force.
#[tokio::test]
async fn re_registering_at_a_different_dimension_is_refused() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;
    let m = model();
    register_model(&conn, &m, 4).await.unwrap();

    match register_model(&conn, &m, 8).await.unwrap_err() {
        DbError::DimMismatch { got, expected, .. } => assert_eq!((got, expected), (8, 4)),
        other => panic!("expected DimMismatch, got {other:?}"),
    }
    assert_eq!(declared_dimension(&conn, &m).await.unwrap(), 4);
}

/// A model name reaches SQL as a table identifier, which cannot be bound as a
/// parameter. `ModelName` is the boundary that makes the splice safe, so the
/// ledger must survive a name built to escape it — and the name must be
/// rejected before any statement is constructed.
#[tokio::test]
async fn a_hostile_model_name_never_reaches_sql() {
    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;

    for hostile in [
        "x; DROP TABLE links; --",
        "x\"; DROP TABLE concepts; --",
        "x) --",
        "x' OR '1'='1",
    ] {
        assert!(
            matches!(ModelName::new(hostile), Err(DbError::InvalidModelName(_))),
            "{hostile:?} was accepted as a model name and would be spliced into SQL"
        );
    }

    // A name that *is* a valid identifier but collides with a ledger table is
    // harmless, because the table name is always prefixed. Worth pinning: the
    // prefix is the only thing standing between `ModelName::new("links")` and a
    // vector column being created on the ledger.
    let collide = ModelName::new("links").unwrap();
    assert_eq!(collide.table(), "embeddings_links");
    register_model(&conn, &collide, 4).await.unwrap();

    assert_eq!(count(&conn, "SELECT COUNT(*) FROM links").await, 0);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM concepts").await, 3);
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM pragma_table_info('links') WHERE name = 'embedding'"
        )
        .await,
        0,
        "registering a model must never touch a ledger table"
    );
}

#[test]
fn test_embedding_codec_roundtrip() {
    let vec = vec![1.0f32, -2.5f32, 3.15f32, 0.0f32];
    let bytes = EmbeddingCodec::encode(&vec, 4, "nomic-v1").expect("encode failed");
    let decoded = EmbeddingCodec::decode(&bytes).expect("decode failed");
    assert_eq!(vec, decoded);
}

#[test]
fn test_dim_mismatch_rejection() {
    let vec = vec![1.0f32, 2.0f32, 3.0f32];
    let res = EmbeddingCodec::encode(&vec, 768, "nomic-v1");
    assert!(res.is_err());
    if let Err(DbError::DimMismatch {
        got,
        expected,
        model,
    }) = res
    {
        assert_eq!(got, 3);
        assert_eq!(expected, 768);
        assert_eq!(model, "nomic-v1");
    } else {
        panic!("Expected DimMismatch error");
    }
}

#[test]
fn test_reciprocal_rank_fusion_scoring() {
    let vector_ranks = vec![
        "doc_a".to_string(),
        "doc_b".to_string(),
        "doc_c".to_string(),
    ];
    let keyword_ranks = vec![
        "doc_b".to_string(),
        "doc_a".to_string(),
        "doc_d".to_string(),
    ];

    let fused = reciprocal_rank_fusion(&vector_ranks, &keyword_ranks, 60);
    assert!(!fused.is_empty());

    // Both doc_a and doc_b appear in top positions, doc_a (1st vec, 2nd kw) & doc_b (2nd vec, 1st kw) share top scores
    let top_doc = &fused[0].0;
    assert!(top_doc == "doc_a" || top_doc == "doc_b");
}

/// The cost model in isolation: a loose filter prices `PostFilter` cheaper, a
/// tight one prices the exact scan cheaper.
///
/// This replaces a test that asserted `select_strategy(100/1000/10000)` returned
/// the three variants in order — a test of two hard-coded thresholds and of a
/// `TwoPhaseTempTable` whose mechanisms libSQL does not offer. The strategies it
/// named are down to two, and the selector now reads the `byte_budget` it used
/// to carry unused. See `vector_filter_tests.rs` for the executable half.
#[test]
fn test_vector_filter_cost_estimator() {
    // 100K vectors of 768 dimensions.
    let estimator = CostEstimator::new(10_000_000, 100_000, 768 * 4);

    let loose = estimator
        .estimate(10, CandidateCount::Exact(90_000))
        .unwrap();
    assert_eq!(loose.strategy, VectorFilterStrategy::PostFilter);

    let tight = estimator.estimate(10, CandidateCount::Exact(50)).unwrap();
    assert_eq!(tight.strategy, VectorFilterStrategy::PreFilterCTE);

    // The budget is a ceiling on the candidate set, not decoration.
    let over = estimator.estimate(10, CandidateCount::Exact(10_000_000));
    assert!(
        matches!(over, Err(DbError::SubgraphTooLarge { .. })),
        "got {over:?}"
    );
}

// -- D-048: the vector write path reaches the actor -------------------------

/// **The regression test for "green and unreachable at the same time".**
///
/// Every other test in this file opens its own `libsql::Connection` and reaches
/// around the Write Actor, which is why the vector suite passed for a whole
/// release while an application had no way to store a vector at all:
/// `upsert_embedding` and `register_model` take a raw connection, `read_conn`
/// is `query_only`, and the write connection lives inside the actor.
///
/// So the constraint here is not what is asserted, it is what is *used*. This
/// test touches `Database` and nothing else on the write side. If the only path
/// to a stored vector is a connection the caller had to build, this does not
/// compile.
#[tokio::test]
async fn an_application_can_register_and_embed_through_the_handle_alone() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    let m = ModelName::new("handle_v1").unwrap();

    db.upsert_concept(ConceptUpsert::new("c0", "First").valid_from(TS))
        .await
        .unwrap();
    db.upsert_concept(ConceptUpsert::new("c1", "Second").valid_from(TS))
        .await
        .unwrap();

    db.register_model(&m, 4).await.unwrap();
    let written = db
        .upsert_embeddings(
            &m,
            vec![
                ("c0".to_string(), vec![1.0, 0.0, 0.0, 0.0]),
                ("c1".to_string(), vec![0.0, 1.0, 0.0, 0.0]),
            ],
        )
        .await
        .unwrap();
    assert_eq!(written, 2);

    // Reads still go direct, which is the point: they never traverse the actor.
    let hits = search_vector(db.read_conn(), &[1.0, 0.0, 0.0, 0.0], &m, 2)
        .await
        .unwrap();
    assert_eq!(
        hits[0].concept_id, "c0",
        "nearest neighbour is the identical vector"
    );

    db.close().await.unwrap();
}

/// Re-embedding replaces rather than versioning (Doctrine VII), and the write
/// leaves the ledger alone — there are no triggers on an embeddings table.
#[tokio::test]
async fn re_embedding_replaces_and_never_touches_the_ledger() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    let m = ModelName::new("handle_v1").unwrap();
    db.upsert_concept(ConceptUpsert::new("c0", "First").valid_from(TS))
        .await
        .unwrap();
    db.register_model(&m, 4).await.unwrap();

    async fn log_len(db: &Database) -> i64 {
        let mut rows = db
            .read_conn()
            .query("SELECT COUNT(*) FROM transaction_log", ())
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }
    let before = log_len(&db).await;

    for v in [vec![1.0f32, 0.0, 0.0, 0.0], vec![0.0, 0.0, 0.0, 1.0]] {
        db.upsert_embeddings(&m, vec![("c0".to_string(), v)])
            .await
            .unwrap();
    }

    let mut rows = db
        .read_conn()
        .query(&format!("SELECT COUNT(*) FROM {}", m.table()), ())
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(n, 1, "a second embedding must replace the first");
    assert_eq!(
        log_len(&db).await,
        before,
        "an embedding reached the ledger"
    );
}

/// A chunk is one transaction, so a bad vector in the middle takes the whole
/// chunk with it rather than leaving a prefix behind.
#[tokio::test]
async fn a_dimension_mismatch_rolls_back_its_whole_chunk() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    let m = ModelName::new("handle_v1").unwrap();
    for id in ["c0", "c1"] {
        db.upsert_concept(ConceptUpsert::new(id, "N").valid_from(TS))
            .await
            .unwrap();
    }
    db.register_model(&m, 4).await.unwrap();

    let err = db
        .upsert_embeddings(
            &m,
            vec![
                ("c0".to_string(), vec![1.0, 0.0, 0.0, 0.0]),
                ("c1".to_string(), vec![1.0, 0.0]), // wrong width
            ],
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err.cause, DbError::DimMismatch { .. }),
        "got {err:?}"
    );
    assert_eq!(err.written, 0, "the batch was one chunk and it rolled back");

    let mut rows = db
        .read_conn()
        .query(&format!("SELECT COUNT(*) FROM {}", m.table()), ())
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(n, 0, "the good row before the bad one must not survive");
}

/// Embedding a model nobody registered is a typed refusal, not an engine error
/// about a missing table.
#[tokio::test]
async fn embedding_an_unregistered_model_is_refused_by_name() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    let m = ModelName::new("never_registered").unwrap();

    let err = db
        .upsert_embeddings(&m, vec![("c0".to_string(), vec![1.0])])
        .await
        .unwrap_err();
    assert!(
        matches!(err.cause, DbError::ModelNotRegistered { .. }),
        "got {err:?}"
    );
}

/// More rows than `chunk_rows::EMBEDDINGS`, so the chunking loop runs more than
/// once and every chunk's rows land.
#[tokio::test]
async fn a_backfill_larger_than_one_chunk_lands_completely() {
    use macrame::prelude::*;

    let harness = TestHarness::new();
    let db = Database::open(&harness.db_path).await.unwrap();
    let m = ModelName::new("handle_v1").unwrap();

    let n = chunk_rows::EMBEDDINGS + 7;
    let mut concepts = Vec::with_capacity(n);
    for i in 0..n {
        concepts.push(ConceptUpsert::new(format!("c{i:06}"), "N").valid_from(TS));
    }
    db.write_concepts(concepts).await.unwrap();
    db.register_model(&m, 2).await.unwrap();

    let rows: Vec<(String, Vec<f32>)> = (0..n)
        .map(|i| (format!("c{i:06}"), vec![i as f32, 1.0]))
        .collect();
    assert_eq!(db.upsert_embeddings(&m, rows).await.unwrap(), n);

    let mut got = db
        .read_conn()
        .query(&format!("SELECT COUNT(*) FROM {}", m.table()), ())
        .await
        .unwrap();
    let stored: i64 = got.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(stored as usize, n);
}

/// **A retired concept is not a search result, and `top_k` is still a count**
/// (W9.3, F-31).
///
/// `keyword_search` has carried `AND c.retired = 0` since it was written;
/// `search_vector` never did, so the same retirement was invisible to one arm
/// of `hybrid_search` and plainly visible to the other. The ledger says
/// retirement is what *stops belief* in a concept, and a reader that returns it
/// anyway is reporting something the ledger says is not there.
///
/// **The second assertion is the one with a design decision behind it.**
/// `vector_top_k` chooses `k` rows before any predicate of ours can see them,
/// so filtering afterwards returns fewer than `k` — and `top_k` quietly
/// becoming a ceiling is a behaviour change for every existing caller. Asking
/// for 2 with a retired concept sitting second must return **two** live
/// neighbours, which is only possible if the index was asked for more than two.
#[tokio::test]
async fn a_retired_concept_is_not_a_vector_search_result() {
    const LATER: &str = "2026-02-01T00:00:00.000000Z";

    let harness = TestHarness::new();
    let (_db, conn) = seeded(&harness).await;
    let m = model();
    register_model(&conn, &m, 4).await.unwrap();

    // Nearest to farthest: c0, c1, c2.
    upsert_embedding(&conn, &m, "c0", &[1.0, 0.0, 0.0, 0.0])
        .await
        .unwrap();
    upsert_embedding(&conn, &m, "c1", &[0.9, 0.1, 0.0, 0.0])
        .await
        .unwrap();
    upsert_embedding(&conn, &m, "c2", &[0.5, 0.5, 0.0, 0.0])
        .await
        .unwrap();

    // The interesting position: second-nearest, so it is inside any top-2 the
    // index would choose and outside nothing.
    conn.execute(
        "UPDATE concepts SET retired = 1, recorded_at = ?1 WHERE id = 'c1'",
        libsql::params![LATER],
    )
    .await
    .unwrap();

    let all = search_vector(&conn, &[1.0, 0.0, 0.0, 0.0], &m, 3)
        .await
        .unwrap();
    assert_eq!(
        all.iter()
            .map(|h| h.concept_id.as_str())
            .collect::<Vec<_>>(),
        vec!["c0", "c2"],
        "a retired concept is not a result"
    );

    let two = search_vector(&conn, &[1.0, 0.0, 0.0, 0.0], &m, 2)
        .await
        .unwrap();
    assert_eq!(
        two.iter()
            .map(|h| h.concept_id.as_str())
            .collect::<Vec<_>>(),
        vec!["c0", "c2"],
        "top_k is a count, not a ceiling: the index must be asked for more than \
         k when the filter takes rows out of it"
    );

    // And the embedding is still there. Retirement is a statement about belief
    // in the concept, not a licence to delete a derived row (Doctrine VII).
    assert_eq!(
        count(&conn, &format!("SELECT COUNT(*) FROM {}", m.table())).await,
        3
    );
}
