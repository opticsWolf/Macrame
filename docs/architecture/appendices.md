<!--nav-->
← [previous](s13-decision-register.md) · [index](README.md)
<!--/nav-->

## Appendix A — Public API (normative)

Rewritten in 0.5.4 against the implementation ([D-040](s13-decision-register.md#d-040)). The prior text was a sketch written before the code existed and had drifted from it in about half its entries; a normative surface that does not describe the surface is worse than none, because it is cited. A.1 is what the crate exposes today. A.2 records what the sketch promised and the crate does not have, so the gap is legible from this document rather than only from a compile error.

A.1 — The surface as it exists

```rust
use macrame::prelude::*;
// The prelude does not re-export the analytics functions, `Subgraph`, or
// `reciprocal_rank_fusion`; those come from `macrame::graph` and
// `macrame::vector` directly. See A.2.

// -- Lifecycle --
let db = Database::open("macrame_knowledge.db").await?;   // migrations run here
db.close().await?;                                        // drain the actor, then final snapshot

// Accessors on the handle. There is no public write connection: the sole
// write-capable connection lives inside the actor and cannot be named.
db.read_conn();        // &libsql::Connection, PRAGMA query_only = ON (D-019)
db.clock();            // &Arc<dyn Clock>
db.schema_version();   // u32
db.archive_path();     // &Path
db.snapshots_dir();    // &Path

// -- Edges (high-priority tier) --
// The builder is a value type, not a method chain off the handle. It is
// validated and canonicalised by `normalized()` at the boundary before it
// crosses the channel (D-034).
let edge = EdgeAssertion::new(source, target, "CITES")
    .valid_from(vf)
    .valid_to(vt)              // explicit; the sentinel is 9999-12-31T23:59:59.999999Z
    .weight(0.8)
    .properties(json);
db.assert_edge(edge).await?;

// valid_from is required, not optional: it is part of the interval key, so it
// is what identifies *which* interval is being closed (§4.2).
db.retire_edge(source, target, "CITES", valid_from, valid_to).await?;

// -- Concepts (high-priority tier) --
let concept = ConceptUpsert::new(id, title)
    .content(text)
    .embedding_model("nomic_v1")
    .valid_from(vf)
    .valid_to(vt)
    .retired(false);
db.upsert_concept(concept).await?;

// -- Bulk writes: the fidelity boundary of §5.1.6 --
db.write_bulk_atomic(edges).await?;    // one transaction, one stamp, one stall
db.bulk_import(edges).await?;          // chunked at CHUNK_ROWS, atomic per chunk
db.write_annotations(concepts).await?; // chunked at CHUNK_ROWS, atomic per chunk

// -- Traversal (read side; takes a connection, not the handle) --
let ids = TraversalBuilder::new(root)
    .max_depth(3)
    .edge_types(vec!["CITES".into()])
    .min_weight(0.5)
    .attribute_mode(AttributeMode::AtTime)
    .execute_ids(db.read_conn(), ts).await?;          // ids only
let rows = TraversalBuilder::new(root)
    .attribute_mode(AttributeMode::AtTime)
    .execute(db.read_conn(), ts).await?;              // ids + hydrated attributes

// -- Valid time (read side) --
let edges = query_as_of_edges(db.read_conn(), ts).await?;

// -- Transaction time (read side) --
let state: MaterializedState = db.reconstruct(ts).await?;   // composes (D-049)
// Or the free function, when the caller is holding a connection rather than a
// handle. `None` for the snapshot directory is correct and folds the whole log.
let state = reconstruct(db.read_conn(), ts,
                        Some(db.archive_path()), Some(db.snapshots_dir())).await?;
state.seq_anchor;   // i64
state.timestamp;    // String
state.concepts;     // HashMap<String, NodeAttributes>
state.edges;        // Vec<(source, target, edge_type, valid_from, valid_to)>

// -- Vectors: writes through the handle (D-048), reads direct --
let model = ModelName::new("nomic_v1")?;
db.register_model(&model, 768).await?;                 // high-pri: table + index
db.upsert_embeddings(&model, vec![                     // low-pri, chunked
    (concept_id.to_string(), vector),
]).await?;

let dim  = declared_dimension(db.read_conn(), &model).await?;
let all  = registered_models(db.read_conn()).await?;
let hits = search_vector(db.read_conn(), &query_vec, &model, 10).await?;
let fused = reciprocal_rank_fusion(&vector_ranks, &keyword_ranks, 60);

// -- Analytics --
// Derived output goes to analytics_annotations, never to the ledger (D-041).
db.write_analytics_annotations(vec![
    Annotation::new(concept_id, "louvain.community", "3"),
]).await?;                                             // chunked, low-priority

let g = db.load_subgraph(root, max_hops, ts, byte_budget).await?;   // -> Subgraph
let dist  = dijkstra(&g, start);                       // BTreeMap<String, f64>
let path  = astar(&g, start, goal, heuristic);         // Option<(f64, Vec<String>)>
let comps = scc(&g);                                   // Vec<Vec<String>>
let core  = k_core(&g, k);                             // BTreeSet<String>
let comm  = louvain(&g);                               // BTreeMap<String, usize>
let q     = modularity(&g, &comm);                     // f64
g.write_back_annotations(&db, "louvain.community", &values).await?;

// -- Integrity --
let drift  = audit_current(db.read_conn()).await?;     // usize; 0 in steady state
let report = db.rebuild_current().await?;              // RebuildReport, drift_after == 0
                                                       // see §5.8 for sizing (D-023)

// -- Archive --
let report = db.archive(cutoff).await?;                // ArchiveReport { links_archived,
                                                       //   log_entries_archived, horizon }
```

Every method backed by a `HighPriCommand` or a `LowPriCommand` carries the `# Latency` rustdoc section [§5.1.8](s5-modules.md#518-write-queue-latency-and-caller-timeouts-052-d-028) specifies, including the rule that a `tokio::time::timeout` bounds the caller's wait and does not cancel the command ([D-028](s13-decision-register.md#d-028)).

A.2 — Divergences from the pre-0.5.4 sketch

Recorded so that "the API says so" cannot be cited against the code. Each line is something the prior [Appendix A](appendices.md#appendix-a--public-api-normative) asserted as normative that the crate does not provide.

| Sketch | Reality |
|---|---|
| `db.assert_edge(s, t, ty).valid_from(..).send()` | The builder is `EdgeAssertion`, a value type; the handle takes it whole. |
| `db.retire_edge(s, t, ty).effective(vt)` | `valid_from` is required — without it the call cannot say which interval it closes. |
| `db.upsert_concept(id).title(t).content(c)` | Builder is `ConceptUpsert`; `title` is a constructor argument, not a setter. |
| `db.traverse(root)…run()` | No `traverse` on the handle. `TraversalBuilder::new(root)…execute(conn, ts)`. |
| `db.reconstruct(ts)` | **Closed in 0.5.4 ([D-049](s13-decision-register.md#d-049))** — the handle method exists and supplies the archive path and snapshot directory itself, which is what makes composition the default. The free function remains, with two more arguments. |
| `state.node_at` / `edge_at` / `neighbors` / `load_subgraph` | `MaterializedState` exposes public fields (`concepts`, `edges`), not query methods. The claim that it "answers with signatures identical to the live `Database`" is not true today. |
| `db.vector_search(model, &v).top_k(10)` | Free function `search_vector(conn, &v, &model, k)`. No builder, no `active_only`. |
| `db.hybrid_search(text, &v).rrf_k(60)` | **Closed in 0.5.5 ([D-051](s13-decision-register.md#d-051))**, as a builder rather than a handle method: `HybridSearch::new(model, text, vector).rrf_k(60).top_k(k).execute(conn)`. Hybrid search is a read, and reads are served from `read_conn` without traversing the actor, so it follows `TraversalBuilder` and `FilteredVectorSearch` rather than hanging off the handle. `rrf_k` is spelled as the sketch proposed. |
| `db.set_embedding(id, model, vec)` | **Closed in 0.5.4 ([D-048](s13-decision-register.md#d-048))**, under a different name and shape: `db.upsert_embeddings(&model, rows)`, plural and chunked, because a single-vector method invites a per-row loop that is a channel round trip per vector. `db.register_model(&model, dim)` came with it. |
| `db.audit_current()` | Free function `audit_current(conn)`. |
| `db.write_analytics_results{,_atomic}` | Three distinct calls, not a pair: `write_analytics_annotations` (derived output, chunked, off-ledger — [D-041](s13-decision-register.md#d-041)), `write_annotations` (bulk **concepts**, chunked, on-ledger; the name is a holdover and is due to change), and `write_bulk_atomic` (**edges**, atomic). There is still no atomic variant of either chunked path. |

Two of these were gaps rather than naming differences. **Both are now closed**, and both are left in the table rather than deleted, because A.2 exists to record what was promised and whether it arrived — each arrived under a different signature than the sketch proposed, and that is the part worth keeping:

- ~~**The vector write path.**~~ **Closed in 0.5.4 ([D-048](s13-decision-register.md#d-048)).** `Database::register_model` and `Database::upsert_embeddings` route through the actor; the free functions remain for callers already holding a connection.
- ~~**Hybrid search.**~~ **Closed in 0.5.5 ([D-051](s13-decision-register.md#d-051)).** [§5.9](s5-modules.md#59-vector--embeddings-the-model-registry-and-search) and [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets) had both budgeted for a path where only the fusion arithmetic existed. `concepts_fts` ([§4.6](s4-schema.md#46-the-concept-text-index--the-third-derivative-table-055-d-051)) supplies the keyword half and `HybridSearch` fuses the two arms. The remaining honest caveat is that [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets)'s ≤ 50 ms is still not a gate and nothing measures it — the path now exists, but the budget is as unverified as every other one in that table.

The entry that remains open in this table is naming rather than capability: `write_annotations` is the bulk **concept** path and its name says otherwise.

A third, cosmetic one: `macrame::prelude` re-exports `AttributeMode`, `EdgeAssertion` and `TraversalBuilder` from `graph`, but not `Subgraph`, `EdgeRef`, `NodeData`, the five algorithms, `modularity`, `CostEstimator`, or `reciprocal_rank_fusion` — so the documented analytics flow does not compile from the prelude alone.

`MaterializedState`'s missing accessors are the last one: [Doctrine VIII](s0-s3-foundations.md#doctrine-viii)'s promise that a caller "does not need to know whether they are querying the present or the past" is not yet purchased, because the two shapes differ.

## Appendix B — Glossary

Archive scope (0.5.1) — the set of tables targeted by the archive path: links (closed intervals) and transaction_log (superseded rows). Concepts are never physically archived ([D-022](s13-decision-register.md#d-022)); they are managed by retired (soft-delete) and valid_to (temporal expiry). As of 0.5.3, the archive session also deletes the links_current rows projecting the intervals it removed, because links_current must remain equal to the latest-belief projection of what is left in links ([Doctrine VI](s0-s3-foundations.md#doctrine-vi)) or audit_current() reports drift the moment an archive runs. Those rows are closed intervals that ended before the cutoff and can never be active in a traversal.

Archive session (0.5.3) — the window in which physical deletion from links and transaction_log is legal. It is exactly the single BEGIN IMMEDIATE … COMMIT archive transaction ([D-012](s13-decision-register.md#d-012)), delimited by the creation and dropping of the macrame_archive_session marker table in main ([D-008](s13-decision-register.md#d-008) revised). ATTACH of the cold database is issued outside the transaction and DETACH unconditionally on the way out, including on error: ATTACH is not transactional and survives ROLLBACK, so a leaked handle would make every later archive or pre-horizon reconstruct fail with "database cold is already in use".

Assertion — an immutable row in links: a statement that an edge held over an interval, as believed at a moment. Changing belief appends assertions; it never edits them.

Belief, current — for each interval key, the assertion with the greatest recorded_at. links_current materializes exactly this set.

Chunk boundary (0.4.5) — the moment between two committed chunks of a low-priority job; the only point at which the writer re-polls the high-priority queue.

Cooperative chunking (0.4.5) — the discipline by which low-priority workers split bulk writes into 500–1,000-row transactions, yielding the writer to the priority poll between them. The golden rule of [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule).

Horizon — the oldest transaction_log sequence still present in the hot file. Reconstruction older than the horizon composes from the archive via the per-query ATTACH path ([§5.5](s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots), [D-026](s13-decision-register.md#d-026)). The horizon is recorded in the cold database during each archive session; a crash between scheduled windows leaves the horizon at the last committed window, and the next run resumes from there.

Hydration — resolving historical attributes for a result set from transaction_log, as opposed to reading live concepts rows.

Interval — the half-open span [valid_from, valid_to) during which a fact held in the world. 9999-12-31T23:59:59.999999Z denotes an open interval.

Canonical timestamp (0.5.4) — the single permitted form for every valid_from, valid_to, and recorded_at: exactly 27 characters, YYYY-MM-DDTHH:MM:SS.ffffffZ, microsecond precision, UTC. Fixed width is the point — it is what makes lexicographic comparison equal chronological comparison, and a Z suffix alone does not achieve it. Enforced by CHECK on all four tables ([D-029](s13-decision-register.md#d-029)), widened from the legacy second-precision form by util::timestamp::normalize() at the crate boundary.

Drift (0.5.4, sharpened) — the symmetric difference between links_current and the latest-belief projection of links: rows the materialization holds that the projection does not, plus rows the projection has that the materialization lacks. Both directions count. audit_current() returns the total; zero is the [Doctrine VI](s0-s3-foundations.md#doctrine-vi) invariant ([D-030](s13-decision-register.md#d-030)).

Materialized state — a MaterializedState: a full reconstruction held in memory, queryable with the live API's shape.

Priority tier (0.4.5) — one of the two command channels into the Write Actor: high (user-driven, preempting) and low (background, yielding at every chunk boundary).

Replay — the window-function fold that derives belief-at-ts from the log, optionally composed over a snapshot and, for pre-horizon timestamps, over the cold database.

Responder (0.4.5) — the oneshot channel carried by every command, through which the actor answers exactly one request. Dropping it does not cancel the command ([D-028](s13-decision-register.md#d-028)).

Snapshot (0.5.0; formerly "checkpoint") — a full MaterializedState serialized with bincode, compressed with zstd, and stored as a sidecar file (snapshots/NNNNNNNN.snap.zst) anchored to a transaction_log.seq_id. Snapshots bound the cost of reconstruct() by limiting the fold to the delta since the snapshot. Not to be confused with SQLite's WAL checkpoint, which is the engine's own mechanism for flushing the write-ahead log into the main database file.

Transaction time (recorded_at) — when the database learned a fact. System axis; never user-supplied except by the injectable clock.

Valid time (valid_from / valid_to) — when a fact held in the world. Domain axis; always explicit at the API.

Write Actor (0.4.5) — the single Tokio task that owns the sole write-capable connection and executes every transaction in the system.

## Appendix C — Future Considerations, Deliberately Deferred

These are recorded so their absence is read as choice, not oversight. Continuous valid-time versioning of concept attributes in the live tables (a concepts_current twin) remains available as an upgrade path if AtTime hydration ever becomes the dominant read pattern; the API already hides which mechanism serves the answer. Streaming change notification to external consumers — the spiritual successor of the discarded CDC design — would be built on transaction_log polling with a seq_id cursor, and is a small module away whenever a consumer exists; the Write Actor's commit points would be its natural poll boundaries. Graph-neural-network features and temporal community evolution (comparing Louvain partitions across snapshots) are natural extensions of the petgraph bridge but belong to the application layer, not the ledger. Automatic writer-actor restart was declined in 0.4.5 ([D-015](s13-decision-register.md#d-015)) and remains available should operational experience argue for it; the containment errors are already shaped to support it. Phased, per-table archiving — breaking the single archive transaction into verified stages — is the recorded escape hatch should archive durations ever threaten interactive latency; until then, idle scheduling with the 100K-row scheduling-layer bound ([§5.7](s5-modules.md#57-temporalarchivers--cold-storage)) suffices. Physical concept removal for legal compliance (GDPR right to erasure) is a separate operation outside the archive path, requiring explicit handling of embeddings, log entries, and links_current rows; it is not designed here because it is not part of the ledger's normal lifecycle ([D-022](s13-decision-register.md#d-022)). Concept *archival* — distinct from erasure — is likewise deferred but no longer believed infeasible, and the shape it would take is recorded here so that scale, when it arrives, finds a design rather than a decision. [D-022](s13-decision-register.md#d-022) ruled it out on three constraints, and [Doctrine VII](s0-s3-foundations.md#doctrine-vii) dissolves two of them: an embedding is a derived artifact of a model applied to content, so an archived concept does not need its vector carried into the cold file at all — it needs enough content preserved to recompute one on rehydration. That removes both the FK-from-embeddings problem and the absence of F32_BLOB and DiskANN on the ATTACHed cold database, since no vector crosses. The third constraint stands and shapes the predicate instead: a concept is an entity, not an interval, so it has no "closed" state, and archivability must be expressed as reachability rather than as expiry — a concept is archivable when it is retired, its valid_to precedes the cutoff, and no surviving row of hot links references it in either direction. That last clause is what keeps the FK from links satisfiable without CASCADE, and it makes concept archival strictly downstream of link archival: concepts become eligible only once the edges that mention them have themselves gone cold. Cold concepts would live in a trigger-free cold.concepts table alongside cold.links, and reconstruct() would fold them by the same last-writer-wins seq_id rule already used for the log. What is not yet designed, and is the reason this stays deferred rather than scheduled: rehydration cost, and whether a concept returning from cold should reacquire its old identity or be treated as a new assertion — a [Doctrine III](s0-s3-foundations.md#doctrine-iii) question that deserves its own decision entry rather than an inherited answer. Crate-level write-command cancellation was declined in 0.5.2 ([D-028](s13-decision-register.md#d-028)) in favor of application-layer CancellationTokens checked before send. Each deferral shares one justification: this system's value is the integrity of its two clocks, and every deferred feature was weighed against the question of whether it strengthens or complicates that integrity. None, yet, has cleared the bar.

Document complete. The normative surfaces are [§4](s4-schema.md#4-schema) and [Appendix A](appendices.md#appendix-a--public-api-normative); the decision register is the authoritative record of intent; the first code to be written is the drift-audit property test, the Monday/Wednesday/Friday attribute-fidelity test, and — as of 0.4.5 — the priority-interleaving concurrency test, because together they pin the three invariants every later change must preserve: that belief is honest, that fidelity is declared, and that the user is never made to wait for a background job.

As of 0.5.4 that instruction is discharged and generalised. The drift-audit property test exists (tests/integritypropertytests.rs) and is model-based: it recomputes the latest-belief projection independently in Rust and requires audit_current() to agree on the exact symmetric-difference count over generated histories, rather than checking that a seeded corruption produces an error. The distinction is the lesson of [D-030](s13-decision-register.md#d-030) — an integrity check whose failure mode is "always reports clean" returns the correct answer on every clean fixture, so no amount of seeded data can distinguish a working check from a broken one, and only a generator that produces states the author did not think of can. The same instrument is now applied to the doctrine itself (tests/doctrinepropertytests.rs), which pins III, IV, V, VII and VIII against histories generated through the public API — the surface on which the invariants are claimed. [Doctrine I](s0-s3-foundations.md#doctrine-i) is a claim about the dependency graph and belongs to CI; [Doctrine VII](s0-s3-foundations.md#doctrine-vii) is only half-pinned until the embeddings tables exist. Every invariant added after 0.5.4 is expected to arrive with a property test, not a fixture.
