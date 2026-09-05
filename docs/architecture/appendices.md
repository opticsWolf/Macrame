<!--nav-->
← [previous](s14-python-bindings.md) · [index](README.md)
<!--/nav-->

## Appendix A — Public API (normative)

Rewritten in 0.5.4 against the implementation ([D-040](s13-decision-register.md#d-040)). The prior text was a sketch written before the code existed and had drifted from it in about half its entries; a normative surface that does not describe the surface is worse than none, because it is cited. A.1 is what the crate exposes today. **Refreshed again in 0.7.0**: it had gone two releases without one and cited nothing past [D-075](s13-decision-register.md#d-075), so the whole 0.6.0 surface was absent from a document marked normative — `diagnostic_conn`, `verify_snapshot_chain`, `rebuild_current_chunked`, `shadow_step`, `archive_windowed`, `estimated_bulk_hold`, `metrics`, `path`, and `TraversalBuilder::as_of`. `tests/doc_sync_tests.rs` now fails the build when a public `Database` method is missing here. A.2 records what the sketch promised and the crate does not have, so the gap is legible from this document rather than only from a compile error.

**Amended in 0.13.35 ([D-208](s13-decision-register.md#d-208)): one canonical path per item.** Everything below is reachable at exactly one module path, plus two convenience surfaces that carry *names* and never *namespaces* — the crate root and `macrame::prelude`. A module is public only where it is the canonical home of what it holds rather than a second route to it, which is three of them: `connection::chunk_rows`, `schema::ddl`, `util::timestamp`. Twenty-three others became `pub(crate)` in that release; the items in them did not move. `tests/public_path_tests.rs` holds the rule against `public-api.txt`.

A.1 — The surface as it exists

```rust
use macrame::prelude::*;
// The prelude does not re-export the analytics functions, `Subgraph`, or
// `reciprocal_rank_fusion`; those come from `macrame::graph` and
// `macrame::vector` directly. See A.2.

// -- Lifecycle --
let db = Database::open("macrame_knowledge.db").await?;   // migrations run here
db.close().await?;                                        // drain the actor, then final snapshot

// The cadence is tunable and `None` disables it (0.5.5, D-053). An injected
// clock is floored against the ledger before the actor starts (0.6.0, D-062),
// so it cannot issue a stamp below what is already stored.
let db = Database::open_with_cadence(path, Some(SnapshotCadence::default())).await?;
let db = Database::open_with_clock(path, None, Arc::new(FakeClock::new(t0))).await?;

// The consolidated form (0.12.12, D-155). The three above stay and delegate
// here; this is where every knob 0.13.0 adds arrives. Since 0.15.13 the struct
// is `#[non_exhaustive]` with a setter per field (W15.3, C-11, D-255), so a
// field added later is a method added later. This paragraph claimed the
// attribute from the day it was written and the struct did not carry it until
// then -- one of three places that did, against `Tuning`'s own docs arguing at
// length that it should not. Note `CadencePolicy` rather than
// `Option<SnapshotCadence>`: `None` in the older constructors means *disabled*,
// and a `Default`-derived struct whose default silently stops writing anchors
// is a trap.
let db = Database::open_tuned(path, Tuning::default()
    .cadence(CadencePolicy::Disabled)
    // Applied to the write connection, the only one that commits (0.12.14,
    // D-157). Disabled is only correct paired with an explicit checkpoint();
    // the default stays at SQLite's 1,000 pages.
    .wal_autocheckpoint(WalCheckpointPolicy::Disabled)
    // Two knobs, not one (0.12.15, D-158): one long-lived writer holding the
    // lock, against read-only connections that are plural. SQLite's units --
    // negative is KiB, positive is pages. Unset runs no pragma, so the default
    // stays SQLite's -2000.
    .writer_cache_size(-64_000)
    .reader_cache_size(-8_000)
    // What to do about a stored recorded_at in the future (0.13.5, D-178).
    // Default refuses beyond a day; Allow opens the file to be read and does
    // not repair it.
    .future_stamps(FutureStampPolicy::Default)
).await?;

// The cadence is the same shape (0.15.13): two setters over a Default.
let cadence = SnapshotCadence::default()
    .every_entries(50_000)
    .poll_interval(Duration::from_secs(30));

// Accessors on the handle. There is no public write connection: the sole
// write-capable connection lives inside the actor and cannot be named.
db.read_conn();        // &libsql::Connection, PRAGMA query_only = ON (D-019)
db.path();             // &Path -- the file this handle opened (0.6.0)
db.clock();            // &Arc<dyn Clock>
db.schema_version();   // u32
db.archive_path();     // &Path
db.snapshots_dir();    // &Path

// Move the WAL back into the main file (0.12.13, D-156). A HighPriCommand:
// asking for a checkpoint is asking for it now. `is_complete()` is the answer
// most callers want; `busy` is the one they must not ignore, because a busy
// checkpoint is an Ok whose WAL is still there.
let report = db.checkpoint().await?;       // CheckpointReport { busy, log_frames, checkpointed_frames }

// A read-only connection of the caller's own (0.6.0, D-091). SQLITE_OPEN_READ_ONLY
// is an OS-level boundary; read_conn()'s PRAGMA is a guardrail its holder can
// turn off in one statement, and it is *shared*, so a long reporting query there
// competes with every traversal in the process.
// Configured since 0.12.16 (D-159): busy_timeout and the reader cache size,
// i.e. the half of configure() a read-only connection can use. Until then it
// ran with SQLite's defaults, including busy_timeout=0 against every other
// connection's 5 s.
let conn = db.diagnostic_conn().await?;

// Actor latency counters, behind --features metrics (0.6.0, D-079). **On by
// default since 0.12.11** (D-154): the cost was measured at under 0.2% of a
// write and indistinguishable from zero, and a counter nobody has compiled in
// is not diagnostic. `--no-default-features` still removes it entirely.
#[cfg(feature = "metrics")]
let snap: MetricsSnapshot = db.metrics();

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

// Both singular writes above are one transaction each, and each pays the
// ~0.8 ms per-transaction floor whole (0.12.7, W3.4, D-090). Correct for one
// row; wrong in a loop, where a thousand rows spend ~0.8 s in transaction
// overhead alone and mint a thousand distinct recorded_at stamps for what is
// probably one act. The bulk forms below are not a fast path with caveats --
// they are what the loop is trying to be. Neither singular form is deprecated:
// only the caller knows whether their thousand rows are one act (Doctrine III).

// -- Bulk writes: the fidelity boundary of §5.1.6 --
// write_bulk_atomic is the one write with no latency bound. Ask first
// (0.6.0, D-081): the batch is one act under one stamp and cannot be chunked,
// so this duration is time every other writer spends waiting.
let held: Duration = estimated_bulk_hold(&edges);   // ~33 ms / 500 rows, ~2.2 s / 20K
if held > BULK_ATOMIC_WARN_HOLD { /* 250 ms; the call warns above this */ }

db.write_bulk_atomic(edges).await?;    // one transaction, one stamp, one stall

// The four chunked paths return Result<usize, BulkInterrupted>, not Result<usize>
// (0.13.8, W7.6, D-181). Atomic per chunk means a failure leaves a prefix
// committed, so the error carries `written` -- the count of rows that are in
// the database and staying there. `?` into a Result<_, DbError> still compiles
// and drops the count, which is the caller saying they will not act on it.
db.bulk_import(edges).await?;          // chunked up to chunk_rows::EDGES, atomic per chunk
db.write_concepts(concepts).await?;    // chunked up to chunk_rows::CONCEPTS, atomic per chunk

// ...and each has a `_with` sibling taking a BulkControl, which is how a
// chunked write is cancelled or watched. Both act at chunk boundaries, which
// is the seam the adaptive loop already has: bulk_import_with,
// write_concepts_with, upsert_embeddings_with, write_analytics_annotations_with.
let token = CancelToken::new();        // .cancel() from any thread or task
let control = BulkControl::new()       // ...or BulkControl::new() for neither
    .cancel_with(token.clone())        // stops at the next boundary; nothing rolls back
    .on_progress(|p: BulkProgress| {   // p.written / p.total / p.rows / p.held
        eprintln!("{}/{}", p.written, p.total);
    });
match db.bulk_import_with(more_edges, control).await {
    Ok(n) => { /* n rows, all of them */ }
    Err(e) => {
        // e.written rows landed and stayed. e.was_cancelled() separates "I
        // stopped it" from "it broke"; e.cause is the DbError either way, and
        // CancelToken::is_cancelled reads a token back.
        eprintln!("stopped after {}: {}", e.written, e.cause);
    }
}

// The four constants are per-path and measured, not one shared number
// (0.5.6, D-058), and since 0.12.0 they are CEILINGS rather than sizes
// (D-146): the loop starts there and sizes each chunk from the last one's
// measured hold, down to a floor of 35 rows. So the boundaries -- and the
// recorded_at stamps a bulk write produces -- depend on the machine, which
// is §5.1.6's fidelity boundary and is stated there.
// chunk_rows::{EDGES 90, CONCEPTS 70, ANNOTATIONS 600,
// EMBEDDINGS 30}. util::HYDRATE_CHUNK (400) is a different kind of
// constant -- a bind-variable ceiling SQLite imposes, not a latency choice.

// -- Traversal (read side; takes a connection, not the handle) --
let ids = TraversalBuilder::new(root)
    .max_depth(3)
    .edge_types(vec!["CITES".into()])
    .min_weight(0.5)
    .as_of_valid(past_ts)                             // 0.13.2 (D-174): what was TRUE then
    .as_of_recorded(past_ts)                          // ...and what we BELIEVED then
    .attribute_mode(AttributeMode::AtTime)            // ...and this fixes the *text*
    .execute_ids(db.read_conn(), ts).await?;          // ids only

// Either instant alone is the ordinary case; both together is the bitemporal
// cell. `as_of(ts)` was one parameter on two clocks through 0.13.1 and is gone
// (D-174). `as_of_recorded` folds the hot log and raises
// RecordedInstantUnreachable at an instant the archive took the answer from
// -- and so does AttributeMode::AtTime, which folds the same log for the text
// (0.13.16, D-189). Instants at or after the newest surviving log row are
// answered as before, `now` included (0.15.4, D-246). For the rest, use
// reconstruct(), which takes the archive path.

// An instant without a stated attribute mode is an error, not a default
// (0.6.0, D-085; extended to both axes in 0.13.2). The two are independent
// questions, and a historical topology with a defaulted Current returns the
// past's graph wearing the present's titles -- a legitimate thing to want and a
// terrible thing to get by accident. It was a tracing::warn! until 0.6.0, which
// reaches nobody without a subscriber.
TraversalBuilder::new(root).as_of_valid(past_ts)
    .execute(db.read_conn(), ts).await;               // Err(AttributeModeUnstated)
let rows = TraversalBuilder::new(root)
    .attribute_mode(AttributeMode::AtTime)
    .execute(db.read_conn(), ts).await?;              // ids + hydrated attributes

// -- Valid time (read side) --
let edges = query_as_of_edges(db.read_conn(), ts).await?;

// -- One value for what a read asks for (0.15.9, D-251, review F-34) --
// The three qualifiers every read surface carries -- lineage, valid instant,
// recorded instant -- as something that can be passed, stored and compared.
// Every field None is the ordinary read: the trunk, now, current belief.
let plan = ReadPlan::new()                 // #[non_exhaustive]; fields are pub
    .on(BranchId::new("exp")?)             // plan.branch:   Option<BranchId>
    .valid_at(tuesday)                     // plan.valid:    Option<String>
    .recorded_at(march)                    // plan.recorded: Option<String>
    .limit(1_000);                         // plan.limit:    Option<usize>
// `limit` (0.15.10, D-252) is the one field that does not narrow WHICH ROWS
// ARE TRUE -- it bounds what the read costs, so a plan carrying one describes
// a sample. What "sample" means differs by surface and each says so.
let beliefs: Vec<EdgeBelief> = db.edges(plan.clone()).await?;
// ^ the whole projection at the plan's instants, on the plan's lineage. No
//   start node, no budget: `load_subgraph` is the bounded neighbourhood read,
//   and the order is unspecified, as it is for `query_as_of_edges`.
//   It is the one edge read that takes a TRANSACTION-time instant: the same
//   `links_at_tx` fold the traversal uses, bounded by the ancestry's cutoffs.
//   `query_as_of_edges_on` is this statement with `recorded` unset and the
//   sixth column dropped, so the two cannot disagree.
//   `EdgeBelief` rather than a 5-tuple, so a row says WHICH LINEAGE holds it
//   -- the thing nearest-ancestor resolution makes unreconstructable by hand.
let walk = TraversalBuilder::new(root).plan(plan);   // sets every field
let same: ReadPlan = walk.read_plan()?;              // and reads them back
// `plan()` REPLACES rather than amends: a plan is the read, so applying
// ReadPlan::new() clears an as_of_recorded set earlier. The three setters stay
// -- `plan()` is additive, C-11's builder rework is breaking, and they do not
// share a release. `read_plan()` is Err(InvalidBranchId) for a `branch` string
// that is not one, which is the only way a builder can hold a name a plan
// cannot; a lineage that is merely unregistered is the READ's refusal.
// A plan validates its branch and nothing else. UnknownBranch,
// RecordedInstantUnreachable and InvalidTimestamp all belong to the read.

// -- A concept can outlive its lineage (0.15.11, D-253, review C-3) --
// `archive_branch` takes the lineage's `branches` row with it; a cold concept
// keeps the `branch_id` it was minted on. Asking for one back after the other
// is the single input where the two arms disagree.
match db.rehydrate(&["c1"]).await {
    Err(DbError::BranchArchived { branch, concept }) => {
        // Was: ErrorKind::Engine, "FOREIGN KEY constraint failed" -- naming
        // neither, and blaming the table being written rather than the one
        // with the missing row. Now ErrorKind::Branch, naming both.
        db.fork(BranchId::new(&branch)?, BranchId::main()).await?;
        db.rehydrate(&[concept.as_str()]).await?;   // and now it succeeds
    }
    other => { other?; }
}
// Nothing is written when it refuses -- the whole rehydrate is one
// transaction, ids ahead of the refused one included. `cold.branches` still
// holds the lineage's parent, fork point and the instant it left, so the
// re-registration can be faithful rather than a guess.

// -- A ceiling that bounds work, not the answer (0.15.10, D-252, review C-8) --
let bounded = TraversalBuilder::new(root).max_depth(4).limit(500);
let (ids, outcome) = bounded.execute_ids_explained(db.read_conn(), ts).await?;
// `limit` emits LIMIT ?n INSIDE the recursive CTE, where SQLite's recursion
// halts. On the outer SELECT it would bound nothing: that projection sorts,
// and a sort materialises the whole walk first. Measured on a 20,050-edge hub
// graph -- outer LIMIT 20: 20,050 edges visited; LIMIT 20 inside: 7,250.
//
// `n` counts WALK ROWS, not answers. The walk holds (node_id, depth) and
// dedupes on the pair, so a node reached at two depths spends two of them and
// the projection then drops retired concepts. So: AT MOST n ids, the ones
// NEAREST the start (the queue is breadth-first), and fewer than n does not
// mean the graph was smaller.
if outcome.hit_limit() { /* more of the graph satisfies the traversal */ }
// ^ WalkOutcome, exact rather than inferred, from the walk's own row count in
//   the same statement. `ids.len() == n` cannot answer it.
// `execute()` and `load_subgraph_with` honour a limit and CANNOT report it --
// a subgraph's own bound is byte_budget, which REFUSES (SubgraphTooLarge)
// rather than truncating. `FilteredVectorSearch::probe_cap` is this ceiling
// under the name that surface already had.

// -- Transaction time (read side) --
let state: MaterializedState = db.reconstruct(ts).await?;   // composes (D-049)
// Or the free function, when the caller is holding a connection rather than a
// handle. `None` for the snapshot directory is correct and folds the whole log.
let state = reconstruct(db.read_conn(), ts,
                        Some(db.archive_path()), Some(db.snapshots_dir())).await?;
state.seq_anchor;   // i64
state.timestamp;    // String
state.concepts;     // HashMap<String, NodeAttributes>
state.edges;        // Vec<EdgeBelief> -- a struct, not a tuple, since 0.14.5
                    // (D-222): the five fields above plus `branch_id`, because a
                    // forked ledger holds two beliefs about one edge and a
                    // 5-tuple silently kept whichever the fold emitted last.
                    // #[non_exhaustive]; build with EdgeBelief::new(..) and
                    // .on_branch(..). SNAP_FORMAT_VERSION went 3 -> 4 with it.
state.predates_recorded_history;  // bool -- nothing had been recorded yet at `ts`
                                  // (0.8.0, D-121). An empty state is empty for two
                                  // different reasons and this says which.

// Snapshot n composes onto snapshot n-1 and nothing ever folds the whole log,
// so an error at any link is copied forward and every read agrees with it.
// This folds from genesis independently and compares (0.6.0, D-092).
let check: ChainCheck = db.verify_snapshot_chain(ts).await?;
check.diverged();                          // bool -- the question worth asking
check.composed_anchor;                     // reported, *never compared*: the composed
check.folded_anchor;                       //   answer and the fold legitimately differ
check.concept_disagreements;               // Vec<String>, capped at SAMPLE_LIMIT = 32
check.edge_disagreements;                  // edges are compared as a *set*
check.truncated;                           // true when either list hit the cap
// It reports and does not repair. Under Doctrine VI a snapshot is derivative:
// the fix is to delete the snapshot directory, which the caller can do without
// this function, and rewriting the file would destroy the only evidence that
// composition has a defect.

// -- Lineage: branching (§15, D-213 … D-224) --
// A branch is transaction time with a tree order, not a third axis (D-213).
let alt: Branch = db.fork(BranchId::new("turn/17/alt/1")?,   // high-pri, ONE row
                          BranchId::main()).await?;          //   in `branches`
alt.id;         // BranchId          alt.forked_at;  // Option<String>, None on the trunk
alt.parent;     // Option<BranchId>  alt.created_at; // String
let all: Vec<Branch> = db.branches().await?;   // read side; trunk first, then creation

// A fork is O(1) in rows written: no ledger table is read, copied or touched.
// The branch inherits its parent's history by *resolution at read* -- the
// ancestry chain, bounded by each fork point on the path down (D-220, D-223).
let seen = TraversalBuilder::new(root)
    .on_branch(alt.id.clone())                 // unset means the trunk, never a union
    .execute_ids(db.read_conn(), ts).await?;
let edges = query_as_of_edges_on(db.read_conn(), ts, Some(&alt.id)).await?;
// ^ resolved since 0.14.4 and CUT AT THE FORK POINT only since 0.14.10
//   (D-227): it is the read that does not go through TraversalBuilder,
//   so D-223's repair reached the other two and not this one. Same
//   cause, second symptom: it is also the read that had no `branch=` in
//   Python until then -- `db.query_as_of_edges(ts, branch=...)` (§14).
let g = db.load_subgraph_with(&builder.on_branch(alt.id), ts, budget).await?;
// Refusals: UnknownBranch (unregistered, in `fork` and in every read that names
// one), BranchExists (taken, `"main"` included), InvalidBranchId, and
// ForkPrecedesParent -- the cross-row half no CHECK can see (D-224).
//
// Writable since 0.14.8 (D-225). A lineage is named on the assertion, never
// on the method -- `on_branch` is a builder step, so an unqualified write means
// the trunk exactly as it always did.
db.assert_edge(EdgeAssertion::new(source, target, "CITES")
                   .valid_from(vf)
                   .on_branch(alt.id.clone())).await?;
db.upsert_concept(ConceptUpsert::new(id, title)
                      .on_branch(alt.id.clone())).await?;   // MINTS; may not
                                                            //   restate an
                                                            //   inherited id
// Retirement across a lineage boundary writes the branch's OWN closed row at
// the ancestor's key; the parent's row is never touched (Doctrine III).
db.retire_edge_on(source, target, "CITES", vf, vt, alt.id.clone()).await?;
// The `_on` suffix rather than a sixth argument, after `query_as_of_edges_on`:
// a positional `Option<BranchId>` would make every existing call site read as
// though it had made a lineage decision. Python has keyword defaults, so there
// it is one method taking `branch=` (§14).
//
// Threading the lineage through every call is what `BranchView` is for
// (0.14.9, D-226). It buys ergonomics and no capability: every method on it is
// the `Database` method with the branch filled in.
let view: BranchView = db.view(alt.id.clone());     // `&Arc<Self>`: the view
view.assert_edge(EdgeAssertion::new(source, target, "CITES")
                     .valid_from(vf)).await?;       //   holds the handle and
view.retire_edge(source, target, "CITES", vf, vt).await?;   // CANNOT close it --
let seen = view.traversal(source)                   //   `close` takes `self`
    .execute_ids(view.read_conn(), vf).await?;      //   and an Arc cannot
view.database();     // for archive / checkpoint / verify -- file, not lineage
// An assertion naming NO branch is stamped; one naming a DIFFERENT branch is
// BranchMismatch, not a silent relabel. Construction does no I/O and cannot
// fail: whether the lineage is registered is every operation's own question.
// In Python the view is a Python class and there is no `db.view(...)` -- the
// Arc is what that method exists to clone, and Python has none (§14).

// What one lineage believes that another does not (0.14.11, D-228). ONE
// statement over TWO resolutions, not two reads: `read_conn()` is shared and
// public, so two reads are two snapshots and can report a difference that
// existed at no instant. Which is why the four CTE builders take a name tag.
let rows: Vec<Divergence> = db.diff(&alt.id, &BranchId::main()).await?;
let rows = view.diff(&BranchId::main()).await?;    // this lineage on the LEFT
rows[0].source_id;  rows[0].target_id;  rows[0].edge_type;   // the edge key
rows[0].valid_from; rows[0].valid_to;   rows[0].weight;      // what is compared
rows[0].branch_id;                      // WHO WROTE THE ROW -- reported, never
                                        //   predicated on. §15.4 said divergence
//   "is exactly the set of rows carrying the branch's own id"; that holds for a
//   fresh fork against an unchurned parent and nothing else. A branch that
//   writes NOTHING diverges from a trunk that reweights after the fork, on a
//   row the TRUNK wrote; two siblings diverge through a row their COMMON
//   ANCESTOR wrote; and re-asserting an inherited edge at its existing value
//   writes a row and concludes nothing.
// No `ts`, and that is a decision: a shadow retirement IS a divergence about an
// instant having passed, so any valid-time filter drops it from `a`'s side and
// answers "no difference". `properties` is not compared -- nothing in the crate
// reads edge properties back. `weight` is compared exactly.

// What a lineage may not overlap is what that lineage CAN SEE: the guard runs
// the read's own resolution restricted to the edge key, so a branch is refused
// for overlapping an interval it inherited and the trunk is not refused for
// overlapping one only a branch believes. Refusals add CrossLineage (a branch
// restating an inherited concept -- `concepts` is keyed by identity), and every
// write checks its lineage before the lock, so an unregistered name is
// UnknownBranch rather than a foreign-key failure.

// -- Vectors: writes through the handle (D-048), reads direct --
let model = ModelName::new("nomic_v1")?;
db.register_model(&model, 768).await?;                 // high-pri: table + index
db.upsert_embeddings(&model, vec![                     // low-pri, chunked
    (concept_id.to_string(), vector),
]).await?;

let dim  = declared_dimension(db.read_conn(), &model).await?;
let all  = registered_models(db.read_conn()).await?;
let hits = search_vector(db.read_conn(), &query_vec, &model, 10, None).await?;
let then = search_vector(db.read_conn(), &query_vec, &model, 10, Some(ts)).await?;  // D-192
let fused = reciprocal_rank_fusion(&vector_ranks, &keyword_ranks, 60);

// -- Analytics --
// Derived output goes to analytics_annotations, never to the ledger (D-041).
db.write_analytics_annotations(vec![
    Annotation::new(concept_id, "louvain.community", "3"),
]).await?;                                             // chunked, low-priority

let g = db.load_subgraph(root, max_hops, ts, byte_budget).await?;   // -> Subgraph
// Fields are private since 0.8.0 (D-114). The surface is accessors:
//   g.node_count()  g.contains_node(id)  g.node(id) -> Option<&NodeData>
//   g.node_ids()    g.nodes()            g.out_edges(id) / g.in_edges(id)
//   g.out_adjacency() / g.in_adjacency() g.insert_node(..) / g.add_edge(..)
// NodeData and EdgeRef likewise: title(), content(), weight(), node(), ....
// content() is Option<&str> and is NOT populated by default (0.8.0, D-116):
// None means not loaded, never empty. Ask with TraversalBuilder::content(true)
// and load_subgraph_with; load_subgraph never fetches it.
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

// The chunked repair (0.6.0, D-082). It builds a shadow table across many small
// transactions and swaps it in under one, so links_current is never partially
// populated -- which is what makes chunking safe here at all.
let report = db.rebuild_current_chunked().await?;      // RebuildReport

// Or drive the states directly. The actor is stateless per command, so the
// epoch travels out to the caller and back rather than being remembered: one
// remembered slot would be shared, and silently corrupted, by two rebuilds at
// once.
let ShadowOutcome::Started { build_start, epoch } =
        db.shadow_step(ShadowStep::Begin).await? else { unreachable!() };
db.shadow_step(ShadowStep::Fill { after: last }).await?;         // -> Filled { last }
db.shadow_step(ShadowStep::Swap { build_start, epoch }).await?;  // -> Swapped { rows }
// An archive between Begin and Swap invalidates the work in progress:
// DbError::RebuildInterrupted, which means the repair *did not run*.
// links_current is untouched and the action is to retry.

db.rebuild_fts().await?;                               // rebuild concepts_fts (D-051)

// -- Planner statistics (0.12.4, D-149) --
// Before 0.12.4 nothing ran ANALYZE, so sqlite_stat1 existed in no database this
// crate had created and every plan was costed against SQLite's built-in
// defaults: ~1M rows, each bound equality column divides by ten. That estimate
// is structural -- it depends on how many columns a query binds, not on what the
// table holds -- which is D-042/D-059/D-064's defect ("captures a query because
// it contains the columns, not because it discriminates") as a standing state.
//
// Both are writes and take the write lock, scheduled as low-priority work.
// PRAGMA analysis_limit = 400 on every connection bounds the hold by the index
// count rather than the table size, which is what makes them schedulable at all.
db.analyze().await?;                                   // ANALYZE, unconditional
db.optimize().await?;                                  // PRAGMA optimize, only what is stale
// close() calls optimize() itself, so a process that opens, works and closes
// keeps its statistics current with nobody arranging it. A failure there warns
// and does not fail the close: stale statistics cost plan quality, and close()
// is where a caller learns whether their writes survived.

// -- Archive --
let report = db.archive(cutoff).await?;                // ArchiveReport { links_archived,
                                                       //   log_entries_archived, horizon }
// LINEAGE-AWARE ONLY SINCE 0.14.12 (D-229). Both predicates matched edge keys
// ACROSS branches, so one lineage's write archived another's current belief;
// and a branch's own CLOSED row at an ancestor's key was taken as "history",
// which removed the branch's disbelief and let the ancestor's open row win
// again. `audit_current` returns 0 across both -- links_current is honestly
// re-derived from a ledger that was wrongly pruned, so Doctrine VI's check has
// nothing outside the file to compare against.

// The abandonment arm (0.14.13, D-230): indexed by LINEAGE where the call above
// is indexed by time. Its links, concepts, log entries AND its `branches` row
// move in one transaction, and afterwards the name is unknown -- a read naming
// it raises UnknownBranch rather than being handed its parent's view, which is
// the whole point rather than a side effect. Refused for the trunk, for a branch
// with descendants, and for one whose concepts another lineage's hot edge names
// (BranchNotArchivable with a reason); UnknownBranch for a name never
// registered. The road map's "contiguous archivable set by construction" is
// false in both senses -- a concept is keyed by identity ledger-wide, and a
// branch's log rows are scattered rather than a prefix -- which is why the third
// refusal exists and why the `branches` row has to move with everything else.
let report: ArchiveReport = db.archive_branch(BranchId::new("abandoned")?).await?;

// Windowed: many bounded sessions instead of one unbounded hold (0.6.0, D-080).
// A window that never advances, or one implying more than MAX_ARCHIVE_SESSIONS
// (4,096), is refused rather than clamped -- rounding it up would archive over
// boundaries the caller did not choose, and the caller cannot see it happen.
let reports: Vec<ArchiveReport> =
    db.archive_windowed(cutoff, Duration::from_secs(86_400)).await?;

// The move back (0.9.0, C3). Rehydration mints no transaction-time facts and is
// invisible to both clocks: it runs inside a declared archive session, which is
// what suppresses the concept insert log trigger (marker-gated at schema v10).
// Ids not in the cold file are skipped rather than being an error.
let back: RehydrateReport = db.rehydrate(&["c1", "c2"]).await?;
back.concepts_rehydrated;   // usize
back.rowids_reassigned;     // usize -- how many could not keep their original
                            //   rowid_pk because something claimed it while they
                            //   were cold. Those get a fresh one and the FTS
                            //   content_rowid mapping is corrected to match.

// Which concepts an archive at `cutoff` would be entitled to move (0.9.0,
// D-128, C1). A free function, like `reconstruct`'s: it takes a connection
// rather than the handle, and it is read-only -- nothing archives concepts yet.
// The answer is a function of the hot state now, and archiving links first
// generally enlarges it, so ask after the link archive rather than before.
let ids: Vec<String> = archivable_concepts(db.read_conn(), cutoff).await?;
```

Every method backed by a `HighPriCommand` or a `LowPriCommand` carries the `# Latency` rustdoc section [§5.1.8](s5-modules.md#518-write-queue-latency-and-caller-timeouts-052-d-028) specifies, including the rule that a `tokio::time::timeout` bounds the caller's wait and does not cancel the command ([D-028](s13-decision-register.md#d-028)). The commands themselves are not part of this surface and never appear below; from 0.13.33 the code agrees, and both enums are `pub(crate)` ([D-206](s13-decision-register.md#d-206)).

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
| `db.vector_search(model, &v).top_k(10)` | Free function `search_vector(conn, &v, &model, k, as_of_valid)`. No builder, no `active_only`; `as_of_valid` arrived in 0.13.19 ([D-192](s13-decision-register.md#d-192)) and is the only qualifier it takes. |
| `db.hybrid_search(text, &v).rrf_k(60)` | **Closed in 0.5.5 ([D-051](s13-decision-register.md#d-051))**, as a builder rather than a handle method: `HybridSearch::new(model, text, vector).rrf_k(60).top_k(k).execute(conn)`. Hybrid search is a read, and reads are served from `read_conn` without traversing the actor, so it follows `TraversalBuilder` and `FilteredVectorSearch` rather than hanging off the handle. `rrf_k` is spelled as the sketch proposed. |
| `db.set_embedding(id, model, vec)` | **Closed in 0.5.4 ([D-048](s13-decision-register.md#d-048))**, under a different name and shape: `db.upsert_embeddings(&model, rows)`, plural and chunked, because a single-vector method invites a per-row loop that is a channel round trip per vector. `db.register_model(&model, dim)` came with it. |
| `db.audit_current()` | Free function `audit_current(conn)`. |
| `db.write_analytics_results{,_atomic}` | Three distinct calls, not a pair: `write_analytics_annotations` (derived output, chunked, off-ledger — [D-041](s13-decision-register.md#d-041)), `write_concepts` (bulk **concepts**, chunked, on-ledger — called `write_annotations` through 0.5.6, renamed in [D-075](s13-decision-register.md#d-075)), and `write_bulk_atomic` (**edges**, atomic). There is still no atomic variant of either chunked path. |

Two of these were gaps rather than naming differences. **Both are now closed**, and both are left in the table rather than deleted, because A.2 exists to record what was promised and whether it arrived — each arrived under a different signature than the sketch proposed, and that is the part worth keeping:

- ~~**The vector write path.**~~ **Closed in 0.5.4 ([D-048](s13-decision-register.md#d-048)).** `Database::register_model` and `Database::upsert_embeddings` route through the actor; the free functions remain for callers already holding a connection.
- ~~**Hybrid search.**~~ **Closed in 0.5.5 ([D-051](s13-decision-register.md#d-051)).** [§5.9](s5-modules.md#59-vector--embeddings-the-model-registry-and-search) and [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets) had both budgeted for a path where only the fusion arithmetic existed. `concepts_fts` ([§4.6](s4-schema.md#46-the-concept-text-index--the-third-derivative-table-055-d-051)) supplies the keyword half and `HybridSearch` fuses the two arms. The remaining honest caveat is that [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets)'s ≤ 50 ms is still not a gate and nothing measures it — the path now exists, but the budget is as unverified as every other one in that table.

~~The entry that remains open in this table is naming rather than capability: `write_annotations` is the bulk **concept** path and its name says otherwise.~~ **Closed in 0.5.6 ([D-075](s13-decision-register.md#d-075))** — it is `write_concepts`, and the actor variant behind it is `WriteConceptsChunk`. Nothing in A.2 is now open.

A third, cosmetic one: `macrame::prelude` re-exports `AttributeMode`, `EdgeAssertion` and `TraversalBuilder` from `graph`, but not `Subgraph`, `EdgeRef`, `NodeData`, the five algorithms, `modularity`, `CostEstimator`, or `reciprocal_rank_fusion` — so the documented analytics flow does not compile from the prelude alone.

`MaterializedState`'s missing accessors are the last one: [Doctrine VIII](s0-s3-foundations.md#doctrine-viii)'s promise that a caller "does not need to know whether they are querying the present or the past" is not yet purchased, because the two shapes differ.

## Appendix B — Glossary

Archive scope (0.5.1, **widened 0.9.0**, **and again 0.14.13**) — **there are two arms as of 0.14.13** ([D-230](s13-decision-register.md#d-230)): the time-indexed one described here, and `archive_branch(name)`, which is indexed by *lineage* and moves everything one branch holds — its links, its concepts, its log entries and its `branches` row — in one transaction, after which the name is unknown rather than empty. The set of tables targeted by the time-indexed path: links (closed intervals), transaction_log (superseded rows) and, since 0.9.0, **concepts** ([D-130](s13-decision-register.md#d-130)). ~~Concepts are never physically archived ([D-022](s13-decision-register.md#d-022)); they are managed by retired (soft-delete) and valid_to (temporal expiry).~~ **Corrected 2026-08-07:** a concept whose retirement and valid_to both precede the cutoff, and which no surviving hot link names, moves to `cold.concepts` column for column ([D-128](s13-decision-register.md#d-128)); its analytics_annotations and embeddings_* rows are deleted rather than moved, and `rehydrate` brings it back. retired and valid_to remain how a concept's *lifecycle* is expressed — they are now also two of the four clauses that decide when it may leave the hot table. As of 0.5.3, the archive session also deletes the links_current rows projecting the intervals it removed, because links_current must remain equal to the latest-belief projection of what is left in links ([Doctrine VI](s0-s3-foundations.md#doctrine-vi)) or audit_current() reports drift the moment an archive runs. Those rows are closed intervals that ended before the cutoff and can never be active in a traversal.

Archive session (0.5.3) — the window in which physical deletion from links and transaction_log is legal. It is exactly the single BEGIN IMMEDIATE … COMMIT archive transaction ([D-012](s13-decision-register.md#d-012)), delimited by the creation and dropping of the macrame_archive_session marker table in main ([D-008](s13-decision-register.md#d-008) revised). ATTACH of the cold database is issued outside the transaction and DETACH unconditionally on the way out, including on error: ATTACH is not transactional and survives ROLLBACK, so a leaked handle would make every later archive or pre-horizon reconstruct fail with "database cold is already in use".

Assertion — an immutable row in links: a statement that an edge held over an interval, as believed at a moment. Changing belief appends assertions; it never edits them.

Belief, current — for each interval key, the assertion with the greatest recorded_at. links_current materializes exactly this set.

Chunk boundary (0.4.5) — the moment between two committed chunks of a low-priority job; the only point at which the writer re-polls the high-priority queue.

Cooperative chunking (0.4.5) — the discipline by which low-priority workers split bulk writes into per-path chunks — 90 edges, 70 concepts, 600 annotations, 30 embeddings, each solved against a 3 ms duration bound ([D-058](s13-decision-register.md#d-058)) — yielding the writer to the priority poll between them. The "500–1,000-row" figure this entry carried until 0.10.0 was the 0.4.5 estimate, superseded by measurement in 0.5.6. The golden rule of [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule).

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

These are recorded so their absence is read as choice, not oversight. Continuous valid-time versioning of concept attributes in the live tables (a concepts_current twin) remains available as an upgrade path if AtTime hydration ever becomes the dominant read pattern; the API already hides which mechanism serves the answer. Streaming change notification to external consumers — the spiritual successor of the discarded CDC design — would be built on transaction_log polling with a seq_id cursor, and is a small module away whenever a consumer exists; the Write Actor's commit points would be its natural poll boundaries. Graph-neural-network features and temporal community evolution (comparing Louvain partitions across snapshots) are natural extensions of the petgraph bridge but belong to the application layer, not the ledger. Automatic writer-actor restart was declined in 0.4.5 ([D-015](s13-decision-register.md#d-015)) and remains available should operational experience argue for it; the containment errors are already shaped to support it. Phased, per-table archiving — breaking the single archive transaction into verified stages — is the recorded escape hatch should archive durations ever threaten interactive latency; until then, idle scheduling with the 100K-row scheduling-layer bound ([§5.7](s5-modules.md#57-temporalarchivers--cold-storage)) suffices. Physical concept removal for legal compliance (GDPR right to erasure) is a separate operation outside the archive path, requiring explicit handling of embeddings, log entries, and links_current rows; it is not designed here because it is not part of the ledger's normal lifecycle ([D-022](s13-decision-register.md#d-022)). Concept *archival* — distinct from erasure — is likewise deferred but no longer believed infeasible, and the shape it would take is recorded here so that scale, when it arrives, finds a design rather than a decision. [D-022](s13-decision-register.md#d-022) ruled it out on three constraints, and [Doctrine VII](s0-s3-foundations.md#doctrine-vii) dissolves two of them: an embedding is a derived artifact of a model applied to content, so an archived concept does not need its vector carried into the cold file at all — it needs enough content preserved to recompute one on rehydration. That removes both the FK-from-embeddings problem and the absence of F32_BLOB and DiskANN on the ATTACHed cold database, since no vector crosses. The third constraint stands and shapes the predicate instead: a concept is an entity, not an interval, so it has no "closed" state, and archivability must be expressed as reachability rather than as expiry — a concept is archivable when it is retired, its valid_to precedes the cutoff, and no surviving row of hot links references it in either direction. That last clause is what keeps the FK from links satisfiable without CASCADE, and it makes concept archival strictly downstream of link archival: concepts become eligible only once the edges that mention them have themselves gone cold. **Delivered in 0.9.0, and this paragraph is kept as the design it was rather than rewritten as the thing that shipped.** The predicate is `CONCEPTS_ARCHIVABLE`, readable through `temporal::archivable_concepts(conn, cutoff)` ([D-128](s13-decision-register.md#d-128), C1), with one clause more than stated here — `recorded_at < cutoff` alongside valid_to, so the two clocks are not mixed against the cutoff. Concepts physically move to `cold.concepts` ([D-130](s13-decision-register.md#d-130), C2), which needed a `v8 → v9` rung to make the delete guard conditional ([D-129](s13-decision-register.md#d-129)). What did **not** need building is the last sentence of this paragraph's own plan: `reconstruct` folds `transaction_log` and never reads the `concepts` table, so it composes across the boundary unchanged.** The two derived-row foreign keys — analytics_annotations and embeddings_* — are deliberately not clauses of it, for the [Doctrine VII](s0-s3-foundations.md#doctrine-vii) reason above: blocking archivability on a recomputable artifact would answer "not yet" forever for any concept that had ever been embedded. Cold concepts would live in a trigger-free cold.concepts table alongside cold.links, and reconstruct() would fold them by the same last-writer-wins seq_id rule already used for the log. **The semantics are settled ([D-131](s13-decision-register.md#d-131), C3) and what remains is operational.** This passage used to name two open questions. The second — whether a concept returning from cold reacquires its old identity or is treated as a new assertion — is **answered**: rehydration is a physical move back, minting no transaction-time facts, so the concept reacquires its identity because the alternative makes the transaction-time axis lie about when it was learned. That is derived from [Doctrine III](s0-s3-foundations.md#doctrine-iii) rather than chosen, and it required schema v10, since the fold resolves by `seq_id` and a log row written at rehydration would outrank the concept's own retirement. The first — `rowid_pk` — has **both** exits defined: reinstate the original when it is still free, or assign a fresh one and re-point `concepts_fts`'s `content_rowid` mapping, which `RehydrateReport::rowids_reassigned` reports. **And as of C4 nothing here is open at all** ([D-132](s13-decision-register.md#d-132)). Rehydration is **measured** — 3.71 ms fixed, ~74 µs per concept, linear to n=1,000 and superlinear above it because FTS5 index maintenance is 53% of the cost at n=10,000 ([§9](s6-s10-flows-to-dependencies.md#9-performance-budgets)) — and the batching shape is **decided rather than deferred**: `rehydrate` keeps its slice and does not window, because a 10,000-concept rehydration holds the write lock for 1.1 s against a contract that already tolerates ~50 s ([§5.1.8](s5-modules.md#518-write-queue-latency-and-caller-timeouts-052-d-028)), and windowing would trade the single-transaction atomicity that makes a partial rehydration impossible for a stall nobody has complained about. That is a measured trade with a number on both sides, which is what this paragraph was waiting for. Crate-level write-command cancellation was declined in 0.5.2 ([D-028](s13-decision-register.md#d-028)) in favor of application-layer CancellationTokens checked before send. Each deferral shares one justification: this system's value is the integrity of its two clocks, and every deferred feature was weighed against the question of whether it strengthens or complicates that integrity. None, yet, has cleared the bar.

### Named for 0.11.0, in this order

Two items opened in 0.10.0 by [D-136](s13-decision-register.md#d-136). They are recorded here rather than left in a commit message because both are *successors to a measurement*, and the thing that goes missing between releases is which number a piece of work was supposed to explain.

**1. Attribute the chunk row's ~3× budget miss. — DELIVERED in 0.11.0 ([D-142](s13-decision-register.md#d-142)).** A 90-row edge chunk into an 8,000-edge table costs **9.06 ms** against a 3 ms bound ([§9](s6-s10-flows-to-dependencies.md#9-performance-budgets)). The figure is measured and the cause is not known. What is known is what it is *not*: the missing access path D-059 diagnosed, which shipped as the `v5 → v6` rung, and which is why the same row read 47.7 ms before 0.5.6. The method already exists — [D-056](s13-decision-register.md#d-056) attributed 92% of chunk-commit cost to the two ledger triggers by dropping them and re-measuring, and [D-064](s13-decision-register.md#d-064) is a second instance of isolation finding a cause that argument had missed. Until this is done, `chunk_rows::EDGES` is a constant tuned against an empty database and defended by a number nobody can explain.

> **The answer is `trg_links_current_sync`**, and within it, secondary-index maintenance on `links_current`: 89% of the growth from an empty table to an 8,000-edge one, against ~0.35 ms for the log trigger and the base insert together and **none at all** for the single-open guard, which is [D-059](s13-decision-register.md#d-059)'s index doing its job. Page-cache pressure, foreign keys, an instrument artifact and the fixture's key distribution were each tested and each refuted. The constant is unchanged: the expensive index is [D-042](s13-decision-register.md#d-042)'s covering index for the traversal, so the obvious fix moves cost onto the read path six columns were chosen to protect. This paragraph is kept as the item it was rather than rewritten as the thing that shipped.

**2. Then re-derive the chunk constants against the [D-088](s13-decision-register.md#d-088) fixture matrix. — DELIVERED in 0.11.0 ([D-143](s13-decision-register.md#d-143)).** [D-059](s13-decision-register.md#d-059) left this open in the exact words *"the chunk constants are empty-database figures and need a realistic fixture, which requires deciding what 'realistic' means"*. The matrix **is** that decision, made in 0.6.0, and it has never been applied to `chunk_budget` — which still seeds one shape, and until 0.10.0 seeded no links at all. Four shapes, four constants.

> **Four shapes, and one constant that needs anything.** `concepts`, `annotations` and `embeddings` all meet the 3 ms bound on a populated database with 1.7–2.2× headroom, and shape cannot reach them — they never read `links`. The edge constant misses by ~2.7×: all four shapes agree that the largest size within the bound is **20**, against a constant of 90. It is **not changed to 20**, because 20 is the same defect at a different population — per-row cost grows with `links_current`, so a constant fitted at 8,000 edges is wrong at 80,000, while the throughput cost of turning eleven chunks into fifty is certain and immediate ([D-058](s13-decision-register.md#d-058)). The fix is not a number: it is for the chunk loop to stop on **elapsed time** rather than on a row count, which is named as the successor below.

**The order is not arbitrary.** If the residual turns out to be trigger cost, running the matrix first produces four numbers carrying the same unexplained component, and the isolation still has to happen afterwards. Attribution is one investigation with a known technique; the matrix is mechanical once you know what you are measuring.

> **It was trigger cost, so the order was right.** One thing [D-142](s13-decision-register.md#d-142) hands item 2 that it did not have: the residual is **shape-independent** — a chain with 8,000 distinct sources costs the same as a star with one — so the four shapes should be expected to differ in what they cost and not in what the cost is made of. A shape that comes back with a different *composition* is the surprising result worth stopping on.

**The trigger that makes either due sooner:** a proposal to change any `chunk_rows` constant, or a §9 chunk figure quoted in a user-facing context. Absent those, a documented miss with a measured number and a stated *cause unknown* is not urgent — it has been true since 0.5.5 and, as of 0.10.0, is at least falsifiable.

### Named for 0.12.0, and it is one item

**Make the chunk loop stop on elapsed time rather than on a row count. — DELIVERED in 0.12.0 ([D-146](s13-decision-register.md#d-146)).** Both 0.11.0 items are delivered and they converge on this. [D-058](s13-decision-register.md#d-058) had already re-derived §5.1.5's golden rule as *a bound on duration, where the row count is not part of it*; the four `chunk_rows` constants are an approximation of that bound fitted at one population, and [D-143](s13-decision-register.md#d-143) is the measurement showing the approximation has expired on the edge path — no row count satisfies a fixed duration on a path whose per-row cost grows with the table. The constants would become an upper bound rather than the criterion.

This is a **write-actor design change** with its own alternatives — where the clock is read, what happens to a chunk already in flight when the budget elapses, and whether a time-based loop can still promise the caller a predictable transaction size — which is exactly why it was not taken as a side effect of measuring. [D-079](s13-decision-register.md#d-079)'s over-budget hold counter is the detector that already exists, and it is why this is scheduled rather than urgent.

All three alternatives were answered ([D-146](s13-decision-register.md#d-146)): the clock is read in the actor around its own transaction, the chunk in flight always commits in full because the lock is not preemptible, and the predictable transaction size is **given up** — the boundaries are now machine-dependent and [§5.1.6](s5-modules.md#516-the-fidelity-boundary-of-chunked-writes) says so.

### Named for 0.13.0, and both come from measuring 0.12.0

**1. The first chunk is the worst one, and nothing yet stops that.** [D-146](s13-decision-register.md#d-146) measured the adaptive loop's longest hold at **7.7–10.2 ms** against **7.6–15.2 ms** for the fixed size it replaced — barely moved, because the loop starts at the ceiling and cannot know better until it has paid for one chunk at that size. The typical stall halved; the worst did not move. A warm-up chunk would fix it and is *a size chosen ahead of time by another name*, which is the thing 0.12.0 just removed — so it needs the measurement first: what does the first chunk cost at sizes between the floor and the ceiling, on the D-088 matrix, and is there a starting size that is cheap when wrong and quick to grow when right.

**2. A cold arm for R15.** [D-147](s13-decision-register.md#d-147) measured the quarantined step at 93% per attempt under sustained load, against ~78% implied by CI's own red rate and 45–75% recorded for single binaries in earlier sessions. The conclusion was that the rate is a property of the machine and the load rather than of this crate, and the measurement that would separate those two — runs spaced minutes apart on the same box — has not been taken. Until it is, no figure here predicts any other machine, which is a documented limitation and not a blocker.

**The trigger that makes either due sooner:** for the first, a report of a visible stall attributable to a bulk import; for the second, a proposal to change the retry budget, or an R15 rate quoted anywhere outside `.cargo/config.toml`.

## Appendix D — The stability contract (normative)

New in 0.13.38 ([D-211](s13-decision-register.md#d-211)). [Appendix A](appendices.md#appendix-a--public-api-normative) says what the surface **is**; this says what changing it **costs** after 1.0, and what it does not cost. Every clause below is either a restatement of a decision already in [the register](s13-decision-register.md#13-decision-register), cited, or marked **New** — in which case this is where it is decided.

**The numbers in this appendix are held to the code by `tests/stability_contract_tests.rs`.** A contract that names a version number and drifts from it is worse than one that names none, because it is cited.

### D.1 — What 1.0 freezes

*Frozen* means a change requires a **major version**.

**1. The public Rust API, item for item and path for path.** [`docs/architecture/public-api.txt`](public-api.txt) is the surface — **1,730 items**. No item is removed, no path stops resolving, and no signature narrows. Each item is reachable at exactly one canonical path, plus flat aliases at the crate root and in `macrame::prelude` ([D-208](s13-decision-register.md#d-208)). Held by `scripts/check_public_api.py` in CI and by `tests/public_path_tests.rs` in `cargo test`. The cycle that produced this surface was reviewed against 0.13.0 item by item before it was frozen — [`api-review-0.14.0.md`](api-review-0.14.0.md), [D-212](s13-decision-register.md#d-212) — which is the last release where that review is cheap.

**2. The ledger tables** — `concepts`, `links`, `transaction_log`. Additive only: `ALTER TABLE ADD COLUMN` and new indexes. A changed primary key, a dropped column or altered bitemporal semantics is a major version with an explicit ETL path, because bitemporal data is the hardest data to migrate: a rebuild means replaying history and recomputing transaction-time boundaries, which is rewriting the past ([D-036](s13-decision-register.md#d-036), [Doctrine III](s0-s3-foundations.md#doctrine-iii)).

**3. The canonical timestamp form** — exactly 27 characters, `YYYY-MM-DDTHH:MM:SS.ffffffZ`, enforced by `CHECK` on all four tables ([D-029](s13-decision-register.md#d-029)). It is a fact about the disk as well as the API, so it is frozen twice over.

**4. The eight doctrines** ([§0](s0-s3-foundations.md#0-doctrine)). Not a version boundary at all: a system that breaks one of these is a different system wearing this one's name.

**5. `DbError` as the error type of every fallible public function**, through `Result<T>` and `BulkResult<T>`. Variants may be *added* (D.2 below); none is removed, renamed, or repurposed to mean something else.

**6. The crate name and the import path.** The package is `macrame-db` on crates.io and the library is `macrame`; the wheel is `macrame-db` on PyPI and the module is `macrame`.

### D.2 — What 1.0 does not freeze

Minor-version changes, and several of them are expected rather than merely permitted.

**1. The file layout.** This is what [D-208](s13-decision-register.md#d-208) bought: a public module freezes the file it names, so all but three of them stopped being public. The three that remain — `connection::chunk_rows`, `schema::ddl`, `util::timestamp` — **are** frozen paths, and those three files cannot move. Everything else in `src/` can be reorganised without a version at all.

**2. The variants of `DbError` and of nine domain enums.** All ten are `#[non_exhaustive]` ([D-207](s13-decision-register.md#d-207)), so a new variant is a minor version. This is deliberate and load-bearing: four error variants arrived in the four waves before 1.0, each because the ledger needed to name the right subject, and a crate that cannot add one reports the wrong error instead.

**3. The derivative tables** — `links_current`, the per-model `embeddings_*` tables, and `concepts_fts`. **No schema-stability guarantee at all.** A minor version needing a different materialization drops the table, recreates it from the DDL and re-derives it inside the same migration step ([D-036](s13-decision-register.md#d-036), [Doctrine VI](s0-s3-foundations.md#doctrine-vi), [Doctrine VII](s0-s3-foundations.md#doctrine-vii)).

**4. The schema version and its migration rungs.** **v17** today. Rungs are forward-only and run at `open()`; adding one is a minor version, and refusing to open a database from a *newer* build is the behaviour, not a bug.

**5. The snapshot container format.** **v4** today. A snapshot is a cache of a fold the ledger can always reproduce, so the format is versioned and an unrecognised version is **refused rather than parsed** ([D-043](s13-decision-register.md#d-043)); a build that cannot read an old snapshot folds from the log instead. Losing every snapshot costs time and no information.

**6. Performance figures.** [§9](s6-s10-flows-to-dependencies.md#9-performance-budgets)'s budgets are measurements on one machine at one population ([D-055](s13-decision-register.md#d-055)), and R15's rate is a property of the machine rather than of this crate ([D-147](s13-decision-register.md#d-147)). Nothing here is a latency promise to a caller.

**7. The minimum supported Rust version.** **1.88** today, and it is not ours: Macrame's own code needs 1.73, and the floor is `home@0.5.12` reached through `libsql-ffi`'s *build*-dependency on bindgen. Raising it is a minor version.

**8. Anything `pub(crate)`, `#[doc(hidden)]`, or gated off the default build.** Including `temporal::fuzzing`, which exists so `fuzz/` can reach the snapshot reader and is invisible to `cargo-public-api` by construction ([D-208](s13-decision-register.md#d-208)). `property-tests` and `fuzzing` are harness features and not API; `metrics` is on by default ([D-154](s13-decision-register.md#d-154)) and that default is a choice this crate may revisit, not a contract.

### D.3 — The Python wheel (New)

**The wheel's surface is frozen on the same terms as the crate's**, and its version tracks the crate's exactly — they are bumped together and there is no independent binding version to reason about ([§14](s14-python-bindings.md#14-python-bindings)).

**One asymmetry, stated rather than glossed: Python has no compiler to enforce it.** On the Rust side an incompatible change fails a build; on the Python side it fails at a call site, in someone else's program. The enforcement is therefore two gates rather than a type system — `tests/binding_parity_tests.rs`, which lives in the crate that *defines* `DbError` and reads the binding from disk, and `tests_py/`, which exercises the built wheel. Where the binding converts a domain enum it does so infallibly, and the wildcard arm **panics** ([D-207](s13-decision-register.md#d-207)): a new Rust variant arriving unmapped is a loud failure rather than a silent misclassification, and the parity gate is what stands between the two.

### D.4 — What this document does not promise

Not frozen, not scheduled, and not defects: the timing of any operation; the query plan the engine chooses (which is why plan pinning is a *gate* and not a guarantee); memory use; the exact text of any error message, as against its variant; the contents of `docs/`, which is rationale rather than surface; and the behaviour of any two Macrame processes against one database file, which is outside [§2](s0-s3-foundations.md#2-system-context)'s single-process context.

---

---

Document complete. The normative surfaces are [§4](s4-schema.md#4-schema) and [Appendix A](appendices.md#appendix-a--public-api-normative); the decision register is the authoritative record of intent; the first code to be written is the drift-audit property test, the Monday/Wednesday/Friday attribute-fidelity test, and — as of 0.4.5 — the priority-interleaving concurrency test, because together they pin the three invariants every later change must preserve: that belief is honest, that fidelity is declared, and that the user is never made to wait for a background job.

As of 0.5.4 that instruction is discharged and generalised. The drift-audit property test exists (tests/integritypropertytests.rs) and is model-based: it recomputes the latest-belief projection independently in Rust and requires audit_current() to agree on the exact symmetric-difference count over generated histories, rather than checking that a seeded corruption produces an error. The distinction is the lesson of [D-030](s13-decision-register.md#d-030) — an integrity check whose failure mode is "always reports clean" returns the correct answer on every clean fixture, so no amount of seeded data can distinguish a working check from a broken one, and only a generator that produces states the author did not think of can. The same instrument is now applied to the doctrine itself (tests/doctrinepropertytests.rs), which pins III, IV, V, VII and VIII against histories generated through the public API — the surface on which the invariants are claimed. [Doctrine I](s0-s3-foundations.md#doctrine-i) is a claim about the dependency graph and belongs to CI; [Doctrine VII](s0-s3-foundations.md#doctrine-vii) is only half-pinned until the embeddings tables exist. Every invariant added after 0.5.4 is expected to arrive with a property test, not a fixture.
