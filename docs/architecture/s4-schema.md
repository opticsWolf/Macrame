<!--nav-->
← [previous](s0-s3-foundations.md) · [index](README.md) · [next](s5-modules.md) →
<!--/nav-->

## §4 Schema

The schema is the normative core of this document. It encodes the doctrine directly: two clocks on every row (II), immutable assertions (III), a table-based ledger (IV), deletion only by archive (V), and derivative state that is auditable and rebuildable (VI). The 0.4.5 amendment changes no DDL. The 0.5.0 release corrects the DDL's presentation and adds two integrity triggers. The 0.5.1 release adds clarifying comments and corrects prose claims. The 0.5.2 release changes no DDL. No table structure, column set, or primary key is altered in any 0.5.x release.

### Naming convention (0.5.0)

All SQL identifiers — table names, column names, index names, trigger names — use snake_case. All Rust identifiers follow the Rust convention (snake_case for functions and fields, CamelCase for types). This convention is declared here and enforced by review; any identifier introduced in a future migration must conform.

### 4.1 Concepts and per-model embeddings
```sql
CREATE TABLE concepts (
    rowid_pk         INTEGER PRIMARY KEY,       -- v8: the rowid, made explicit (D-119)
    id               TEXT NOT NULL UNIQUE,      -- ULID, 26-char Crockford base32
    title            TEXT NOT NULL,
    content          TEXT NOT NULL DEFAULT '',
    embedding_model  TEXT,                      -- names the embeddings* table holding the vector
    valid_from       TEXT NOT NULL,             -- canonical ts (below), valid time
    valid_to         TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    recorded_at      TEXT NOT NULL,             -- transaction time, crate-stamped
    retired          INTEGER NOT NULL DEFAULT 0, -- soft-delete flag (see note below)

    -- Canonical timestamp form, 0.5.4 (D-029). Fixed width is what makes
    -- lexicographic comparison equal chronological comparison; the 'Z' suffix
    -- alone is not sufficient. Applies identically to all four tables.
    CHECK (valid_from  GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z'
       AND valid_to    GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z'
       AND recorded_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z')
);

-- Semantic distinction between valid_to and retired (0.5.1):
--
--   valid_to  = DOMAIN axis. "This concept ceased to be valid in the world
--               at this time." Participates in temporal queries (as_of,
--               reconstruct). A concept with valid_to = '2025-01-01' was
--               valid until Jan 1, 2025.
--
--   retired   = APPLICATION axis. "The user has soft-deleted this concept."
--               Participates in visibility queries (traversals filter
--               retired = 0). A retired concept is hidden from normal views
--               but still exists for historical reconstruction.
--
-- They are ORTHOGONAL. A concept can be:
--   - temporally valid (valid_to = 9999...) but retired (user trashed it)
--   - temporally expired (valid_to < 9999...) but not retired (still visible
--     as a historical reference)
--   - both, or neither.
--
-- retired is NOT redundant with valid_to. Setting valid_to to a past date is a
-- domain assertion about the world; setting retired = 1 is an application
-- action about visibility. They answer different questions and serve different
-- query paths.
```

**Archivability is the first thing that reads all three of these columns at once (0.9.0, [D-128](s13-decision-register.md#d-128), C1).** The orthogonality note above is what makes that worth stating here rather than only in [§5.7](s5-modules.md#57-temporalarchivers--cold-storage): `valid_to` and `retired` answer different questions and serve different query paths, so a predicate requiring **both** — plus `recorded_at` behind the cutoff, plus no surviving hot `links` row naming the concept — is a conjunction across two axes that this schema otherwise keeps apart. It is deliberate. A concept still visible to the application (`retired = 0`) must not go cold whatever its valid time says, and a concept the user trashed while it is still temporally valid must not either. Only the corner where all three agree, and nothing points at the row, is archivable.

Nothing in this section changes for it: `CONCEPTS_ARCHIVABLE` is a query over the columns above, not a constraint on them, and no DDL moves until C2 adds `cold.concepts` and makes `trg_concepts_guard_delete` conditional ([§4.6](#46-triggers), [D-126](s13-decision-register.md#d-126)).

Per-model embedding tables are not part of the baseline schema: `register_model` creates each one, with its DiskANN index, in a single transaction ([§5.9](s5-modules.md#59-vector--embeddings-the-model-registry-and-search), [D-037](s13-decision-register.md#d-037)). The shape is fixed and the name is derived from a validated [`ModelName`](s5-modules.md#59-vector--embeddings-the-model-registry-and-search):

```sql
CREATE TABLE embeddings_nomic_v1 (
    concept_id TEXT PRIMARY KEY REFERENCES concepts(id),
    embedding  F32_BLOB(768) NOT NULL
);

CREATE INDEX vec_idx_nomic_v1
    ON embeddings_nomic_v1 (embedding) USING diskann;

-- A new model => a new table + a new index. Nothing about the old model changes.
-- The index is not optional: dimension enforcement is a property of the index,
-- not of the declared column type (D-037), so a table without one accepts
-- vectors of any length.
```

Concepts are entities with mutable attributes, and are updated in place: a title correction is an UPDATE to the same row, captured for history by the log triggers of [§4.3](s4-schema.md#43-the-transaction-log). Timestamps are stored as ISO-8601 text rather than SQLite's ambiguous DATETIME, because they must sort lexicographically, survive export to any tool, and never be silently reinterpreted as local time.

Canonical timestamp form (0.5.4, normative). Every valid_from, valid_to, and recorded_at in the schema is exactly 27 characters in the form YYYY-MM-DDTHH:MM:SS.ffffffZ — fixed width, microsecond precision, UTC. The width is the requirement; the Z suffix alone is not sufficient and the pre-0.5.4 document was wrong to imply it was. Lexicographic ordering agrees with chronological ordering only when every string has the same shape, and '2026-01-01T00:00:00Z' <= '2026-01-01T00:00:00.000000Z' evaluates to FALSE, because at the first differing byte 'Z' (0x5A) sorts after '.' (0x2E). The second-precision instant therefore compares as later than the identical microsecond-precision instant, and a traversal predicated on valid_from <= :ts silently returns an empty set — no error, no warning, just a missing subgraph. The rule is enforced by a table-level CHECK (a GLOB pattern matching the canonical shape) on all four tables rather than by convention, because this failure mode is silent and a convention cannot be violated loudly. The open-interval sentinel is correspondingly 9999-12-31T23:59:59.999999Z: a sentinel exempt from the canonical width would be the one carve-out that reintroduces the bug the width exists to prevent, and .999999 additionally makes it the maximum representable instant, which is what the half-open interval [valid_from, valid_to) needs. Callers supplying the legacy second-precision form are widened at the boundary by util::timestamp::normalize() rather than rejected; anything else — an offset, a missing Z, millisecond precision — is a typed error, because every silent repair here becomes a wrong answer in a temporal query later. See [D-029](s13-decision-register.md#d-029). recorded_at is stamped by the crate's injectable clock — not by SQL defaults — so that every row written inside one application transaction shares an identical transaction timestamp, and so that tests can control time itself. (When a bulk write is chunked across transactions — [§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule) — "one application transaction" means one chunk: each chunk receives its own stamp, and the fidelity boundary this creates is documented in [§5.1.6](s5-modules.md#516-the-fidelity-boundary-of-chunked-writes).)

F32\BLOB dimension enforcement (0.5.0, corrected 0.5.4 — see [D-037](s13-decision-register.md#d-037)). The 0.5.0–0.5.3 text claimed that libSQL enforces the declared dimension at insert time and concluded that the crate-level check "exists for error quality" while "the engine-level check exists for correctness". That is false as stated, and the correction matters because the conclusion drawn from it was the wrong way round. Measured against libSQL 0.9.30: a vector of the wrong length inserted into a column declared F32\_BLOB(768) is **accepted** while the table carries no vector index, and rejected — with the row not written — once a libsql\_vector\_idx index exists. Enforcement belongs to the DiskANN index, not to the column type.

Two rules follow. The index is created in the same transaction as the table it indexes and is never optional: a per-model table without its index has no dimension enforcement at all, so dropping the index to accelerate a bulk load silently disarms the only storage-layer check and is not a supported operation. And the crate-level check in embedding.rs is a correctness boundary rather than a courtesy, so it must be a real comparison against a real number: the dimension is resolved by reading F32\_BLOB(n) back out of PRAGMA table\_info for the model's table, which keeps exactly one copy of it — the one the engine will itself enforce. The caller still receives a typed DimMismatch ([§7](s6-s10-flows-to-dependencies.md#7-errors)) naming both dimensions. Both halves of the asymmetry are pinned by tests in tests/vector\_tests.rs, the negative half included, so if libSQL ever begins enforcing the column type on its own the test fails and [D-037](s13-decision-register.md#d-037) is revisited rather than quietly outgrown.

Embeddings are stored apart from concept rows, in tables named for their model, because the alternative — a single F32_BLOB(768) column on concepts — makes model migration structurally dangerous. The day the project moves to a 1536-dimensional model, a single shared column would hold mixed dimensions during the transition, and every vector operation on a not-yet-migrated row would fault against the type constraint. With per-model tables, migration is additive: the new table and its DiskANN index are created, rows are embedded into it at leisure, embedding_model is flipped per concept, and the old index remains fully valid until the old table is finally dropped. The DiskANN index itself is maintained entirely by libSQL — inserts into the embedding table update the index automatically — and this crate never touches index mechanics.

### 4.2 Links: assertion history and current-belief materialization
```sql
-- Full bitemporal history. One row per assertion; assertions are immutable.
CREATE TABLE links (
    source_id   TEXT NOT NULL REFERENCES concepts(id),
    target_id   TEXT NOT NULL REFERENCES concepts(id),
    edge_type   TEXT NOT NULL,                  -- validated [A-Z0-9]+ at the API boundary
    valid_from  TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    valid_to    TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    weight      REAL NOT NULL DEFAULT 1.0,
    properties  TEXT NOT NULL DEFAULT '{}',     -- JSON: provenance, confidence, external IDs
    PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at)
);

-- The two archive indexes (0.12.6, D-151, v10 -> v11). Through 0.12.5 `links`
-- carried the primary key and nothing else, which is fine for the write path
-- and wrong for the archive path: the PK leads on source_id, and both archive
-- predicates seek on something else.
--
--   idx_links_recorded_at serves `recorded_at < :cutoff`, the opening clause of
--     LINKS_ARCHIVABLE. Both the archiving SELECT and the archiving DELETE
--     scanned the whole ledger for it; measured before and after:
--       SCAN links  ->  SEARCH links USING INDEX idx_links_recorded_at
--
--   idx_links_target serves the right arm of CONCEPTS_ARCHIVABLE's
--     `links.source_id = concepts.id OR links.target_id = concepts.id`. An OR
--     is only as seekable as its worst arm, so the whole correlated subquery
--     degraded to a scan of `links` once per candidate concept -- O(concepts x
--     links). With the index SQLite takes both arms as seeks:
--       SCAN links USING COVERING INDEX  ->  MULTI-INDEX OR (both arms SEARCH)
--     Before the index the planner was already building an AUTOMATIC COVERING
--     INDEX on target_id per statement, i.e. paying for a throwaway copy of it.
--
-- These are the first indexes this schema puts on a *frozen* ledger table.
-- That is the case D-036 named: the freeze restricts the core to additive
-- operations and lists ADD COLUMN and new indexes as the two that qualify. No
-- row moves and no bitemporal semantics change.
--
-- The clock floor (`SELECT MAX(recorded_at) FROM links`, read on every open())
-- is NOT among the queries these justify, despite the review having led with
-- it: SQLite already served the bare MAX() from the PK's covering index without
-- traversing the table, and still does (D-150).
CREATE INDEX idx_links_recorded_at ON links (recorded_at);
CREATE INDEX idx_links_target      ON links (target_id);

-- Materialized current belief: the latest assertion per interval.
-- Traversals read ONLY this table.
--
-- Foreign keys are deliberately omitted (0.5.0 rationale): links_current is
-- derivative state (Doctrine VI). It is rebuilt from links by
-- rebuild_current(), which inserts in arbitrary order; FK constraints would
-- require insertion-order discipline on a table whose only integrity
-- requirement is "matches the latest-belief projection of links." Adding FKs
-- would create a second failure mode (FK violation during rebuild) on a
-- disposable cache, and would couple the rebuild path to the concepts table's
-- population order for no gain. Referential integrity is enforced at the
-- source: links carries FKs to concepts, and links_current mirrors links.
CREATE TABLE links_current (
    source_id   TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    edge_type   TEXT NOT NULL,
    valid_from  TEXT NOT NULL,
    valid_to    TEXT NOT NULL,
    weight      REAL NOT NULL,
    properties  TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (source_id, target_id, edge_type, valid_from)
);

-- Traversal covering index (0.5.4, D-042). Column order is measured, not
-- guessed: source_id is the seek column, the valid-time range follows it so
-- the recursive step walks only the slice that can match, and edge_type sits
-- after the range columns because `edge_types` is empty in the default
-- traversal. Everything the CTE reads is present, so the walk is index-only.
CREATE INDEX idx_lc_traversal_cover ON links_current
    (source_id, valid_from, valid_to, weight, edge_type, target_id);

-- Subsumes the former idx_lc_src_active (source_id, valid_to), dropped by the
-- v3 -> v4 rung: same seek column, strictly more payload, and keeping both
-- costs a second index write on every assertion.
--
-- idx_lc_tgt_active (target_id, valid_to) stood here through v7 and was
-- dropped by the v7 -> v8 rung (D-089, D-118). No query in the crate seeks on
-- target_id as a leading column -- every links_current predicate leads on
-- source_id or binds all three key columns, and Subgraph::in_adj is built in
-- Rust from the forward rows the walk already returned. Measured at -7.9% off
-- assert_edge on star_of_stars.

-- Every assertion upserts current belief; an out-of-order insert
-- cannot clobber a newer belief.
-- Requires SQLite >= 3.24.0 (2018) for ON CONFLICT ... DO UPDATE ... WHERE.
-- libSQL inherits this; the minimum version is noted here because a
-- dependency regression that drops partial-upsert support would silently
-- ignore the WHERE clause and permit belief rollback.
CREATE TRIGGER trg_links_current_sync
AFTER INSERT ON links
BEGIN
    INSERT INTO links_current
        (source_id, target_id, edge_type, valid_from, valid_to,
         weight, properties, recorded_at)
    VALUES
        (NEW.source_id, NEW.target_id, NEW.edge_type, NEW.valid_from,
         NEW.valid_to, NEW.weight, NEW.properties, NEW.recorded_at)
    ON CONFLICT(source_id, target_id, edge_type, valid_from) DO UPDATE SET
        valid_to    = excluded.valid_to,
        weight      = excluded.weight,
        properties  = excluded.properties,
        recorded_at = excluded.recorded_at
    WHERE excluded.recorded_at > links_current.recorded_at;
END;

-- At most ONE open interval per (source, target, type).
--
-- The valid_from <> NEW.valid_from predicate is load-bearing and subtle:
-- it permits re-assertion of the SAME interval (correction after mistaken
-- retirement — same valid_from, newer recorded_at) while blocking a SECOND
-- distinct open interval for the same edge. Removing or "simplifying" this
-- predicate breaks the re-assertion path. See §4.2 edge lifecycle table,
-- row 3. Do not simplify.
CREATE TRIGGER trg_links_single_open
BEFORE INSERT ON links
WHEN NEW.valid_to = '9999-12-31T23:59:59.999999Z'
     AND EXISTS (
         SELECT 1 FROM links_current
         WHERE source_id  = NEW.source_id
           AND target_id  = NEW.target_id
           AND edge_type  = NEW.edge_type
           AND valid_from <> NEW.valid_from   -- allows same-interval re-assertion
           AND valid_to   = '9999-12-31T23:59:59.999999Z'
     )
BEGIN
    SELECT RAISE(ABORT, 'macrame: edge already has an open interval; retire it first');
END;
```

**This guard is correct and, as of 0.5.5, known to be served by the wrong index ([D-059](s13-decision-register.md#d-059)).** The planner resolves its `EXISTS` as `SEARCH links_current USING COVERING INDEX idx_lc_traversal_cover (source_id=?)` — one equality column out of three available — because `idx_lc_traversal_cover` happens to contain every column the predicate mentions and so wins as a covering index, while the PK autoindex `(source_id, target_id, edge_type, valid_from)` would bind three columns but lacks `valid_to` and loses. Every insert into `links` therefore scans the source's entire out-degree: 90 edges into an 8,000-edge hub take 47.7 ms, and into a 90,000-edge hub, 1.06 s. The predicate is not the problem and must not be simplified; the missing access path is. `CREATE INDEX idx_lc_open_interval ON links_current (source_id, target_id, edge_type, valid_to, valid_from)` restores a four-column point lookup and flattens the curve (47.7 → 8.0 ms), with `valid_from` present only to keep the index covering. **Applied in 0.5.6** as the `v5 → v6` rung, `idx_lc_open_interval` — index-only and on a derivative table, so [D-036](s13-decision-register.md#d-036) permits it on the same two grounds the `v3 → v4` rung stood on. `idx_lc_traversal_cover` is kept: neither index subsumes the other, since the traversal leads on `source_id` alone while this probe binds all three equality columns.

**What this guard does *not* cover, and what does (0.5.6, [D-060](s13-decision-register.md#d-060)).** The `WHEN` clause restricts it to the open sentinel, so it is a guard on *open* intervals rather than on overlap. Two **closed** intervals for one `(source, target, edge_type)` that overlap in valid time — say `[Jan, Jun)` and `[Mar, Sep)` — satisfy every constraint in this schema, and `query_as_of_edges` at an instant inside both then returns one relationship as two edges, which every weighted algorithm downstream double-counts. That was defect AA: a wrong answer with no error.

Overlap is now refused, but **in the write actor rather than in this schema**, so the two guards partition the space: both intervals open is this trigger's case and yields `SingleOpenViolation`; every other overlapping pair is the actor's and yields `OverlappingInterval`. The consequence is stated rather than hidden — **the storage layer permits what the API refuses.** Raw SQL against the same file can still write an overlapping pair of closed intervals, and no trigger will stop it. This is one of the two such gaps that remain — [§4.7](s4-schema.md#47-what-this-schema-does-not-enforce) lists them in one place, along with the third, which schema v7 closed, and what is and is not the same about them. The alternative was a second index probe inside this trigger on every insert, on the path the paragraph above has just finished making fast, to constrain callers who were going through the actor anyway. See [D-060](s13-decision-register.md#d-060) for the full argument and for why `EdgeAssertion::normalized` — the location the implementation plan recommended — cannot host the check.

The primary key of links deserves its history, because it was the site of the project's most instructive design failure. The natural first draft keys edges by (source, target, type, valid_from) — and then cannot express correction. A user who asserts an edge, retires it, and later discovers the retirement was mistaken will try to re-assert the same interval with the original valid_from; under the 4-column key that insert collides with the historical row, and the only escape is mutating history — exactly what [Doctrine III](s0-s3-foundations.md#doctrine-iii) forbids. Adding recorded_at to the key resolves this cleanly: the same interval may be asserted any number of times, each assertion a distinct row, and the row with the greatest recorded_at is the current belief. Retroactive retirement, mistaken-retirement recovery, and belief correction all become plain inserts.

That fix, however, moves the cost of "what is the current belief?" from write time to read time — and the read path that cannot afford it is the recursive traversal, where the edge table is joined once per hop. Resolving latest-belief with a window function inside every hop join would have traded a schema flaw for a latency flaw. The resolution is links_current: a trigger-maintained materialization holding exactly one row per interval, always the latest assertion. Traversals read it with the same index profile the original schema had; history reads links; and because the materialization is derivative ([Doctrine VI](s0-s3-foundations.md#doctrine-vi)), its corruption is detectable by audit_current() and recoverable by rebuild_current() ([§5.8](s5-modules.md#58-integrity--audit-and-rebuild)). The upsert's WHERE excluded.recorded_at > links_current.recorded_at clause closes the last hole: even out-of-order inserts — replayed imports, restored backups — cannot roll belief backward.

The edge lifecycle under this schema consists entirely of inserts; nothing is ever updated:

| Action | Insert into links | Effect on links_current |
|---|---|---|
| Assert edge, effective T0 | (vf=T0, vt=9999…, ra=now) | upsert → open |
| Retroactive retirement, effective T1 | (vf=T0, vt=T1, ra=now) | upsert → closed [T0, T1) |
| Re-assert after mistaken retirement | (vf=T0, vt=9999…, ra=now₂) | upsert → open again |
| Correct a past belief (e.g. weight) | same key, corrected fields, ra=now₃ | upsert → corrected |

The properties column exists from day one because composite-primary-key tables are the most expensive place in SQLite to add a column later — ALTER TABLE requires a full rebuild — and because edge metadata (provenance, confidence, external identifiers) is not a speculative concern but an inevitability of graph work.

**`links_current_shadow` is a fourth table, and it is not in this schema (0.6.0, [D-082](s13-decision-register.md#d-082)).** The chunked rebuild ([§5.8](s5-modules.md#58-integrity--audit-and-rebuild)) creates it from `links_current`'s declared DDL, fills it across many small transactions, and swaps it in under one. It is deliberately **transient and trigger-free**: it is created by `ShadowStep::Begin` and dropped by the swap, it carries none of `links_current`'s triggers because nothing should log a row that is not yet current belief, and no migration creates it. An orphan left behind by a process that died mid-rebuild is harmless — `Begin` drops and recreates rather than reusing, so the cost is one `DROP` and never a wrong answer.

It is documented here rather than left to the module text because a reader inspecting a live database will find it and needs to know it is not part of the ledger.

### 4.3 The transaction log
```sql
CREATE TABLE transaction_log (
    seq_id      INTEGER PRIMARY KEY AUTOINCREMENT,  -- strictly monotonic (see note below)
    table_name  TEXT NOT NULL,                      -- 'concepts' | 'links'
    entity_id   TEXT NOT NULL,                      -- ULID, or s|t|type|vf for links
    operation   TEXT NOT NULL,                      -- 'I' | 'U' | 'D'
    payload     TEXT NOT NULL,                      -- json_object(...), versioned ('v')
    recorded_at TEXT NOT NULL                       -- inherited from the app transaction
);

-- seq_id monotonicity note (0.5.1):
-- AUTOINCREMENT guarantees that seq_id is strictly monotonic and that no
-- rowid is ever reused. It does NOT guarantee a gap-free sequence. If a
-- transaction is rolled back (trigger abort, disk error, deadlock), the
-- sqlite_sequence counter is still incremented, leaving a gap. All replay
-- and delta-fold logic MUST use inequality comparisons (seq_id > :anchor),
-- never equality or successor arithmetic (seq_id = :anchor + 1). The
-- anchored fold in §5.5 and the snapshot composition rule both satisfy
-- this requirement. Future maintainers: do not build gap-free assumptions.
--
-- 0.5.4 (D-049): the anchored fold now exists, so this rule finally binds
-- something. The mechanism above is wrong, though, and the correction is
-- worth keeping: a rolled-back transaction does NOT leave a gap, because
-- sqlite_sequence is written inside the transaction and rolls back with it
-- (measured on libSQL 0.9.30). Gaps are real all the same -- the archive
-- deletes superseded rows from this table, scattered through the sequence
-- rather than as a prefix. Right rule, wrong reason.

-- Index rationale (0.5.0):
--   idx_txlog_time serves the unanchored cold-start fold (no snapshot;
--     scan by recorded_at to find the starting point).
--   idx_txlog_entity serves entity-scoped hydration (§5.2 AtTime) and
--     the per-entity partition in the anchored fold.
--   The anchored fold itself (seq_id > :anchor AND recorded_at <= :ts) is a
--     rowid scan with a filter: because seq_id is INTEGER PRIMARY KEY
--     AUTOINCREMENT, it IS the rowid, so SQLite walks it in order and applies
--     the recorded_at predicate per row. No secondary index is needed.
```

**The trigger set (restored 0.5.4, from `schema::ddl` — see the note below).**

```sql
-- v10 (C3, D-131): marker-gated. An insert inside a declared archive session is
-- rehydration -- a physical move back -- and must mint no transaction-time fact.
-- The gate is not tidiness: the fold resolves by seq_id, not recorded_at, so a
-- log row written here would take a NEW seq_id while carrying the concept's
-- ORIGINAL recorded_at, outrank the later 'U' that retired it, and resurrect a
-- superseded belief at every ts after the concept's creation.
CREATE TRIGGER trg_concepts_log_insert AFTER INSERT ON concepts
WHEN NOT EXISTS (
    SELECT 1 FROM sqlite_master
    WHERE type = 'table' AND name = 'macrame_archive_session'
)
BEGIN
    INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at)
    VALUES ('concepts', NEW.id, 'I',
            json_object('v', 1, 'title', NEW.title, 'content', NEW.content,
                        'valid_from', NEW.valid_from, 'valid_to', NEW.valid_to,
                        'retired', NEW.retired),
            NEW.recorded_at);
END;

-- Deliberately NOT gated: nothing inside a session updates a concept -- archival
-- deletes, rehydration inserts -- so gating it would suppress nothing and
-- disarm a log guard for longer (D-131).
CREATE TRIGGER trg_concepts_log_update AFTER UPDATE ON concepts
BEGIN
    INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at)
    VALUES ('concepts', NEW.id, 'U', json_object(/* same shape as above */),
            NEW.recorded_at);
END;

CREATE TRIGGER trg_links_log_insert AFTER INSERT ON links
BEGIN
    INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at)
    VALUES ('links',
            NEW.source_id || '|' || NEW.target_id || '|' || NEW.edge_type || '|' || NEW.valid_from,
            'I',
            json_object('v', 1, 'source_id', NEW.source_id, 'target_id', NEW.target_id,
                        'edge_type', NEW.edge_type, 'valid_from', NEW.valid_from,
                        'valid_to', NEW.valid_to, 'weight', NEW.weight,
                        'properties', json(NEW.properties)),
            NEW.recorded_at);
END;

-- Enforce recorded_at monotonicity on concept updates (0.5.0, D-018).
-- Doctrine II requires the two clocks to be independent; a code path issuing an
-- UPDATE with a stale recorded_at would capture the new state under the old
-- timestamp, and reconstruct() between the two would return the new state. This
-- is a constraint, not a default: the crate still supplies the value.
CREATE TRIGGER trg_concepts_monotonic_ra
BEFORE UPDATE ON concepts
WHEN NEW.recorded_at <= OLD.recorded_at
BEGIN
    SELECT RAISE(ABORT, 'macrame: concept recorded_at must be strictly increasing');
END;

-- Deletion is legal only inside a declared archive session. The guard probes
-- main.sqlite_master for the marker rather than selecting from the marker
-- itself, so an illegal delete is a typed ArchiveViolation rather than a
-- "no such table" nobody can act on (D-008 revised, §5.7).
CREATE TRIGGER trg_links_guard_delete
BEFORE DELETE ON links
WHEN NOT EXISTS (
    SELECT 1 FROM sqlite_master
    WHERE type = 'table' AND name = 'macrame_archive_session'
)
BEGIN
    SELECT RAISE(ABORT, 'macrame: physical delete blocked outside archive session');
END;

-- Identical guard on transaction_log (0.5.3, D-008b): the archive removes
-- superseded log rows, so the ledger itself is a hot table an ad-hoc DELETE
-- could otherwise truncate.
CREATE TRIGGER trg_txlog_guard_delete BEFORE DELETE ON transaction_log
WHEN NOT EXISTS (SELECT 1 FROM sqlite_master
                 WHERE type = 'table' AND name = 'macrame_archive_session')
BEGIN
    SELECT RAISE(ABORT, 'macrame: physical delete blocked outside archive session');
END;

-- v9 (C2, D-126): marker-gated, matching its two siblings above. Through v8
-- this was unconditional, on D-022's reasoning that concepts are never
-- physically archived -- which C2 makes false. An ad-hoc DELETE is still
-- refused; it is refused for the same reason `links` is, rather than because
-- concept archival is impossible.
CREATE TRIGGER trg_concepts_guard_delete BEFORE DELETE ON concepts
WHEN NOT EXISTS (
    SELECT 1 FROM sqlite_master
    WHERE type = 'table' AND name = 'macrame_archive_session'
)
BEGIN
    SELECT RAISE(ABORT, 'macrame: physical delete blocked outside archive session');
END;
```

**The rung that installs it, and the two things that made it necessary (v9, [D-126](s13-decision-register.md#d-126)).** `CREATE TRIGGER IF NOT EXISTS` on an existing name keeps the **old body**, so re-issuing the baseline against a v8 database changes nothing; and `verify` compared `type` and `name` and never bodies, so the stale guard passed verification in silence. Both halves are closed. The `v8 → v9` rung is a `DROP TRIGGER` and a `CREATE` — no table rebuild, no data movement, cost independent of database size — and `verify` now additionally requires that **every** delete guard's body probe `macrame_archive_session`. It checks for the marker name rather than comparing full trigger text: a whitespace-sensitive comparison that has to be hand-updated whenever a guard is reworded is a check people switch off, whereas *this guard is gated on the archive session* is the one property all three share and none may lose.

> **Three corrections this restoration surfaced (0.5.4).** The trigger DDL above was lost entirely to the transport corruption and is recovered from `schema::ddl`, which is the implementation rather than the pre-0.5.4 prose — and the two disagree in three ways worth naming rather than quietly reconciling.
>
> **Names.** The document called these `trg_concepts_log_i` / `_u` and `trg_links_log_i`. The schema calls them `trg_concepts_log_insert`, `trg_concepts_log_update`, `trg_links_log_insert`. Prose elsewhere in this document still uses the short forms; the schema's names are the real ones.
>
> **There is no delete-logging trigger, on either table.** The pre-0.5.4 text described `trg_links_log_d` and "`trg_concepts_log_d` is analogous", writing `'D'` rows into the log. Neither exists. That is defensible — concepts are never deleted at all, and a link leaves the hot table only by being archived, where the row itself moves to the cold file rather than vanishing — but it means **no `'D'` row is ever written by this schema**, so the fold's deletion handling ([D-049](s13-decision-register.md#d-049)) is correct and currently unreachable. If archiving ever needs to be visible to a reconstruction as a deletion, that is where the trigger belongs.
>
> **Concept payloads did not carry `embedding_model`, and now do — this note is settled.** Through payload **v1** they carried title, content, valid_from, valid_to and retired, and a reconstruction could not tell which model a concept was embedded under. Payload **v2** adds the field (0.5.6 Wave 1, defect V; `ddl.rs:173`), and `trg_concepts_log_update` writes it too. [Doctrine VII](s0-s3-foundations.md#doctrine-vii) was never at issue — a model *name* is not a vector. **The v1 case survives**, because payloads are never rewritten: a database written before 0.5.6 still holds v1 rows, and folding one loses `embedding_model` from that concept's temporal reads (`ddl.rs:548`). Kept rather than struck, on this document's practice of retaining what a note corrected; retired as a *current* limitation in 0.10.0 (W4.4).

Several details of the log are load-bearing. `AUTOINCREMENT` on `seq_id` guarantees strict monotonicity without rowid reuse, which makes the sequence a trustworthy tie-breaker for entries sharing a `recorded_at` within one transaction. **It does not guarantee a gap-free sequence**, though not for the reason the 0.5.1 text gave — see the DDL comment above and [D-049](s13-decision-register.md#d-049). All replay logic uses inequality comparisons (`seq_id > :anchor`), which skip gaps correctly; no code path may assume `seq_id + 1` is the next event. The composite `entity_id` for links uses `|` as separator, which is safe because ULIDs are Crockford base32 and edge types are validated against `[A-Z0-9]+` at the API boundary — the separator cannot appear in any component. Payloads carry a version field — `'v', 1` for links, `'v', 2` for concepts since 0.5.6 Wave 1 — because the log outlives every schema migration: entries written under schema v1 must still deserialize under v7, so the deserializer branches on version and old payloads are never rewritten. Concept payloads include `content` — a reconstruction that silently drops document text is not a ledger state — but exclude embeddings per [Doctrine VII](s0-s3-foundations.md#doctrine-vii): vectors are large, immutable per version, and reconstructable from the per-model tables, which are themselves not logged.

The delete guards close the loop on [Doctrine V](s0-s3-foundations.md#doctrine-v). The archive process creates the marker table first and drops it last, so the window in which deletion is legal is exactly the window in which the verified archive transaction runs.

**`recorded_at` monotonicity is guarded on UPDATE and not on INSERT, and that asymmetry is worth stating** (0.10.0, W4.9). `trg_concepts_monotonic_ra` is a `BEFORE UPDATE` trigger firing when `NEW.recorded_at <= OLD.recorded_at`, so a concept's transaction time cannot be walked backwards by revising a row. Nothing equivalent fires on the first insert, so a raw writer can stamp a brand-new concept at any instant it likes, past or future, and the schema will take it.

**A `BEFORE INSERT` guard would not close it**, which is why one is not proposed here rather than merely not written. There is no `OLD` row to compare against, so the only available predicate is a comparison against the table's existing maximum — a read of every concept on the hot insert path, to enforce a global ordering the schema does not otherwise require, and it would still accept a *future* timestamp, which is the direction that actually corrupts a fold. The real guarantee lives one layer up, in `SystemClock`'s monotonic floor ([§5.11](s5-modules.md#511-util--ids-clocks-timestamps-and-engine-ceilings)), and a writer holding a raw connection is already outside it — the same writer can flip `PRAGMA query_only`, drop a trigger, or create the archive-session marker ([§4.7](s4-schema.md#47-what-this-schema-does-not-enforce-056-d-074)). A trigger that cost every insert a table scan and still did not bound the thing it was named for would be the [D-089](s13-decision-register.md#d-089) shape: a price paid on every write for an unclear return.

One property of the log interacts directly with the 0.4.5 amendment: chunked write-backs ([§5.1.5](s5-modules.md#515-cooperative-chunking--the-golden-rule)) produce log entries across many transactions, but seq_id remains strictly monotonic across the chunks (gaps are possible only on rollback, not on successful commit), because each chunk's trigger inserts land inside its own committed transaction in AUTOINCREMENT order. The ledger never sees the seams; only the granularity of "learned" changes.

### 4.4 Asymmetric versioning, deliberately

The schema versions its two entity kinds differently, and the asymmetry is a decision, not an oversight. links keep full assertion history with a 5-column key; concepts are updated in place with history captured only in the log. The justification is workload-shaped. The bitemporal hot path of this system is relationships: edges are interval assertions that get corrected, re-asserted, and retroactively retired as a matter of routine, and the re-assertion trap that motivated the 5-column key applies precisely to intervals. Concept attributes — titles, text — are properties of entities, where in-place update plus log capture yields identical reconstruction fidelity at strictly lower cost. And the cost argument is decisive on the read side: every traversal hop joins the edge table, so latest-belief resolution there would tax every query forever, while the terminal join to concepts happens once per result set and stays trivial against single rows.

The honest price of this asymmetry is that a valid-time traversal over live tables returns past topology with present attributes unless the caller asks otherwise. A node whose title changed on Wednesday, traversed as_of(Tuesday), appears in Tuesday's edges wearing its Wednesday name. [§5.2](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity) addresses this with AttributeMode: the default Current is fast and loudly documented (including a runtime tracing::warn! when combined with as_of), AtTime hydrates attributes from the log for the result set only, and Omit skips attributes entirely. The alternative — full 5-column versioning of concepts with a concepts_current twin — would make AtTime free at the cost of a permanent join tax on every query in the system, and for a ledger whose bitemporal center of gravity is edges, that is the wrong trade. The boundary is pinned by tests: callers who need belief-at-ts fidelity for retroactive assertions use reconstruct(ts) and traverse the materialized state ([§5.5](s5-modules.md#55-temporalreplayrs-and-temporalsnapshotrs--reconstruction-and-snapshots)), and the distinction between the two paths is stated in the rustdoc of both.

### 4.5 Analytics annotations — the second derivative table (0.5.4, D-041)

```sql
CREATE TABLE IF NOT EXISTS analytics_annotations (
    concept_id  TEXT NOT NULL REFERENCES concepts(id),
    label       TEXT NOT NULL,     -- namespaced: 'louvain.community', 'kcore.shell'
    value       TEXT NOT NULL,     -- JSON payload, opaque to this crate
    computed_at TEXT NOT NULL,     -- when the derivation last ran; NOT a clock axis
    PRIMARY KEY (concept_id, label),
    CHECK (computed_at GLOB '<canonical form>' AND 1)
);
```

`idx_annotations_label ON analytics_annotations (label)` stood here through v7 and was dropped by the `v7 → v8` rung ([D-089](s13-decision-register.md#d-089), [D-118](s13-decision-register.md#d-118)): nothing in the crate selects from this table at all — it is written by `write_analytics_annotations` and read only by callers, through their own connection — so the index was an insert-time cost with no reader.

Everything about this table is the opposite of the four above, and each opposite is deliberate.

**No log trigger.** Nothing in the trigger set fires on it, so an annotation never reaches `transaction_log`. That is [Doctrine VII](s0-s3-foundations.md#doctrine-vii)'s reasoning about embeddings applied to the other derived artifact. A community label is a function of an algorithm, a version of that algorithm, and a graph — not a statement about the world. A ledger that carries it records the analytics schedule as though it were history, and a reconstruction that wants labels should recompute them: a stored label answers what some past run of some past version of the algorithm produced, which is a different question wearing the same name.

**No delete guard.** [Doctrine V](s0-s3-foundations.md#doctrine-v) protects the hot ledger tables. This one is [Doctrine VI](s0-s3-foundations.md#doctrine-vi)'s second category, so wiping it must remain an ordinary legal operation — a rerun replaces the previous pass, and dropping the table entirely costs a recomputation and nothing else.

**`computed_at` is not a third clock.** It carries the canonical form ([D-029](s13-decision-register.md#d-029)) so it sorts like every other timestamp in the file, but it is a note about when a derivation last ran, not a transaction-time axis: it is subject to no monotonicity guard and participates in no temporal query. [Doctrine II](s0-s3-foundations.md#doctrine-ii)'s "two clocks, never mixed" is unaffected precisely because this is neither of them.

**The foreign key is safe here in a way `links_current`'s omitted ones are not.** [§4.2](s4-schema.md#42-links-assertion-history-and-current-belief-materialization) leaves `links_current` FK-free because `rebuild_current()` inserts in arbitrary order and a FK would impose insertion-order discipline on a disposable cache. There is no such problem here: this table is rebuilt by re-running an algorithm that read `concepts` in the first place.

~~And concepts are never physically deleted ([D-022](s13-decision-register.md#d-022)), so the FK can never be violated from the other side.~~ **Corrected 2026-08-07 — that half stopped being true in 0.9.0, and the FK is now doing real work.** Archival deletes concepts, and this foreign key would refuse the delete outright. So the archive session disposes of a concept's annotations **before** removing the concept itself, and that ordering is not incidental — it is what makes the move legal ([D-130](s13-decision-register.md#d-130)). Under [Doctrine VII](s0-s3-foundations.md#doctrine-vii) the annotations are recomputable from the content that did cross, so deleting them loses nothing the ledger was keeping.

### 4.6 The concept-text index — the third derivative table (0.5.5, D-051)

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS concepts_fts USING fts5(
    title,
    content,
    content='concepts',       -- external content: index the tokens, not the text
    content_rowid='rowid_pk'  -- v8: a declared column, not an implicit rowid (D-119)
);
```

The keyword half of hybrid search ([§5.9](s5-modules.md#59-vector--embeddings-the-model-registry-and-search)). Like `analytics_annotations` it is [Doctrine VI](s0-s3-foundations.md#doctrine-vi)'s second category — disposable, rebuildable, carrying no log trigger and no delete guard — and it arrives on the ladder's `v4 → v5` rung.

**External content is the whole design.** `content='concepts'` means FTS5 stores the inverted index and reads column values back from `concepts` by rowid when it needs them. There is therefore exactly one copy of the text, and the index cannot come to disagree with the concept about what the concept says. A standalone FTS table would be a second description of data the ledger already holds — the failure class [D-030](s13-decision-register.md#d-030) and [D-035](s13-decision-register.md#d-035) exist to prevent — and it would also make [D-036](s13-decision-register.md#d-036)'s rebuild a piece of our code rather than the engine's own `'rebuild'` command.

**Two triggers, and the second is the one that is easy to get wrong.** `trg_concepts_fts_insert` adds a new concept's terms. `trg_concepts_fts_update` must first issue FTS5's `'delete'` command **with the old column values**, because an external-content index stores terms rather than text and cannot work out what to retract on its own. Omit that half and the index goes on matching words the concept no longer contains, with no error and no symptom except a search that returns something that is no longer there.

**The third trigger, installed inert (v8, [D-119](s13-decision-register.md#d-119)).** Through v7 there was no delete trigger, and the recorded reason was that `trg_concepts_guard_delete` is unconditional ([D-022](s13-decision-register.md#d-022)) — concepts are never physically deleted, not even inside an archive session — so there was no delete path to keep in sync. That was true, and it was the wrong shape: this index's correctness depended on a *different* trigger staying unconditional, and nothing but a paragraph connected them.

`trg_concepts_fts_delete` now exists and issues the same `'delete'` command with `OLD` values. It **cannot fire today** — the guard is a `BEFORE DELETE` that always aborts, so no statement reaches `AFTER DELETE` — and that is the point: 0.9.0's archive session is what makes the guard conditional, and installing the capability in a rung that was already rebuilding the table costs nothing. ~~It also means **0.9.0 needs no migration of its own**.~~ **Corrected 2026-08-06 ([D-126](s13-decision-register.md#d-126)): it does not.** This trigger is one of three changes concept archival needs; making `trg_concepts_guard_delete` marker-gated is another, and that one requires a `v8 → v9` rung, because `CREATE TRIGGER IF NOT EXISTS` on an existing name keeps the **old body** and the migration ladder's `verify` compares trigger *names* and never bodies — so a re-issued baseline would leave the unconditional guard in place and pass. The rung is a `DROP TRIGGER` and a `CREATE`: no table rebuild, no data movement. `the_fts_delete_trigger_is_installed_and_inert` asserts both halves, because the inertness belongs to the other trigger and that dependency is exactly what a test should hold.

**Why `content_rowid` names a column now.** An external-content index is keyed on the content table's rowid, and through v7 `concepts`'s was implicit — which SQLite's documentation permits `VACUUM` to renumber. [D-071](s13-decision-register.md#d-071) argued the hazard unreachable because the delete guard keeps rowids dense; [D-120](s13-decision-register.md#d-120) measured a second reason nobody had identified, which is that `VACUUM` renumbers a sparse implicit rowid only for a table with **no index at all**, and `concepts` has always carried the `id` autoindex. So v7 was safe twice over, by two accidents. `rowid_pk INTEGER PRIMARY KEY` replaces both with a guarantee.

**Retirement is filtered at query time, not indexed.** An external-content index covers only the columns it declares, so `retired` is not among them and `keyword_search` filters it on the join back to `concepts`. That makes "a retired concept is not a search result" a property of one query rather than of the schema, which is exactly the kind of thing that silently stops happening — hence its own test.

Under [D-036](s13-decision-register.md#d-036) this table is periphery, not frozen core — it may be dropped, reshaped or recreated by any minor version, and the migration that does so simply reruns the analytics. It arrived on the ladder's first second rung, `v2 → v3` ([D-032](s13-decision-register.md#d-032), [D-041](s13-decision-register.md#d-041)).

<a id="47-what-this-schema-does-not-enforce"></a>
### 4.7 What this schema does not enforce (0.5.6, D-074)

Everything above is a constraint the *engine* keeps: a `CHECK`, a key, a trigger, a guard. Three invariants of this ledger were not among them; **as of schema v7 two are, and the third has been closed** ([D-083](s13-decision-register.md#d-083)). The section is kept at three rows rather than trimmed to two, because a reader who arrives from an older document needs to find the row and see that it moved. They are checked in Rust, above the storage layer, and they are gathered here because each had been recorded separately — [§4.2](s4-schema.md#42-links-assertion-history-and-current-belief-materialization) for one, [D-068](s13-decision-register.md#d-068) for the second, [§5.4](s5-modules.md#54-graphsubgraphrs-and-graphalgorithmsrs--native-in-memory-analytics) for the third — with no single place a reader could learn that the set has three members rather than one. **A fourth row was added in 0.10.0** — not a newly opened gap, but one that had always been there and that nothing had written down: row 2's raw writer can disable rows 1–3's enforcement outright.

**The property.** *A database this crate has written is not, by that fact alone, a database in which these three hold. Two of them are properties of the write API; one is a property of a read.* A file opened by any other SQLite client on the machine can be given rows the crate would refuse, and no trigger will object.

| # | Invariant | Refused by | Permitted by | Decision |
|---|---|---|---|---|
| 1 | No two valid-time intervals overlap for one `(source_id, target_id, edge_type)` | the write actor, `OverlappingInterval` | the schema — `trg_links_single_open` fires only on the open sentinel, so two *closed* overlapping intervals satisfy every constraint here | [D-060](s13-decision-register.md#d-060) |
| 2 | All writes are serialised through one connection | the handle — every write method crosses a channel, and `read_conn()` carries `PRAGMA query_only = ON` | `Database::raw()`, and the free `register_model` / `upsert_embedding`, which take a bare connection | [D-068](s13-decision-register.md#d-068) |
| 3 | ~~Edge weights are **non-negative**~~ — **closed in v7**, see below | the schema, `CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real')` | `links_current` and pre-v7 cold files, which carry no such `CHECK` — so the loader guard stays | [D-039](s13-decision-register.md#d-039), [D-083](s13-decision-register.md#d-083) |
| 4 | The archive-session marker is **absent** outside an archive session — **detected at open since 0.10.0**, see below | `Database::open`, `ArchiveSessionLeaked` — `verify()` refuses a committed `macrame_archive_session` | the schema, which cannot express "this table must not exist"; and any raw writer, which is row 2's permission used to switch off rows 1–3's enforcement | [D-008](s13-decision-register.md#d-008), [D-135](s13-decision-register.md#d-135) |

**Row 4 is row 2's consequence rather than a fourth independent hole, and it is the one that turns the others off (0.10.0).** `macrame_archive_session` is how the three delete guards and `trg_concepts_log_insert` are told an archive is in progress. An archive session creates and drops it inside one transaction, so a commit removes it and a rollback discards it — there is no crash path that leaves it behind, which is what [D-008](s13-decision-register.md#d-008)'s revision rests on and what `temporal_tests::a_failed_archive_session_leaves_the_hot_database_untouched_and_the_guards_armed` asserts. What no argument covered is a writer that creates the table *directly*: row 2 concedes such writers exist, and this is what one of them can do with a single `CREATE TABLE`. While the table is present, physical deletes from `concepts`, `links` and `transaction_log` are permitted and concept inserts write no ledger row — [Doctrine IV](s0-s3-foundations.md#doctrine-iv) and [Doctrine V](s0-s3-foundations.md#doctrine-v) suspended together, silently. Since 0.10.0 `verify()` checks for it in the `sqlite_master` pass it already makes, so the condition is refused at `Database::open` with a message naming the `DROP TABLE` that clears it. **This detects, it does not prevent** — a raw writer can still create the table and use it within one session of the process, and the schema has no way to forbid a table's existence.

**Row 3 said "non-negative *and not NaN*" until 0.6.0, and the NaN half was wrong ([D-078](s13-decision-register.md#d-078)).** NaN is refused by the storage layer, not by the loader: SQLite stores a NaN double as NULL, so `weight REAL NOT NULL` rejects it. Measured on libSQL 0.9.30 through all three doors — `assert_edge(weight = f64::NAN)`, a raw `INSERT` binding NaN, and a raw `INSERT` with the literal `0.0/0.0` — every one fails with `NOT NULL constraint failed: links.weight`. So NaN is not a weaker gap than the other two; it is **not a gap at all**, and listing it here claimed the schema was silent where it is in fact strict. The `weight.is_nan()` arm of the loader's guard is unreachable from any SQLite client on a file this schema created; it stays as defence against a future engine that stores NaN, and a test pins the current behaviour so that change would be noticed rather than discovered.

**The Python binding adds no fourth hole (0.7.0, [§14.6](s14-python-bindings.md#146-what-is-not-exposed)).** That is a design constraint on it rather than an observation about it: `Database::raw()`, `read_conn()` and the free `register_model` / `upsert_embedding` — invariant 2's three named permissions — are all deliberately unexposed, so a wheel cannot be used to reach them. `diagnostic_conn()` *is* exposed, but as methods that run a query and return rows rather than as a connection object, and it opens `SQLITE_OPEN_READ_ONLY`, so it cannot write at all. The row above is therefore unchanged by the binding's existence, which is worth stating because the natural expectation of a foreign-function layer is that it widens the surface.

**The third was not the same shape as the other two, and that is why it is the one that got closed ([D-083](s13-decision-register.md#d-083)).** For 1 and 2 the supported write path enforces the rule and the gap opens only for a writer who goes around it. For 3 the supported write path *accepted* the value: `assert_edge` with a weight of −1.5 committed, and the refusal arrived later, when something asked for a subgraph. So a file this crate wrote by itself, with no other client involved, could hold a row the crate declined to read back. That asymmetry is what made it the one worth spending a migration on, and schema v7 spends it.

**What the constraint turned out to need, none of which was foreseen.** The obvious form is `CHECK (weight >= 0.0)`. Probing on libSQL 0.9.30 found it admits two further values, and both are worse than the negative weight it was written for:

- **Text.** `REAL` is an affinity, not a type. `'abc'` cannot be converted, so it is stored as TEXT — and every text value sorts above every numeric one, so `'abc' >= 0.0` is *true*. Reading such a row back as `f64` does not return an error: it reaches `unreachable!("invalid value type")` inside libsql and **panics**, in whatever unrelated query first touches the row. Hence `typeof(weight) = 'real'`.
- **Infinity.** `9e999` is a perfectly good non-negative REAL, and the loader guard tests `< 0.0` and `is_nan()`, so it passes both. The first reading was that this is harmless — IEEE infinity propagates through addition and stays totally ordered, so Dijkstra terminates and calls the edge unusable. Wrong: the log trigger serialises the row to JSON, **JSON has no infinity**, and the payload cannot be read back. Every later `reconstruct()` fails with `ReplayCorrupt`, including the one `close()` runs. Under [Doctrine III](s0-s3-foundations.md#doctrine-iii) the log is the source of truth, so an infinite weight is not eccentric, it is corrupt. Hence `weight < 9e999`.

**Why the other two stay open, and why that is a decision rather than a delay.** 1 would add a second index probe to every insert on the path [D-059](s13-decision-register.md#d-059) had just finished making fast, to constrain callers already going through the actor — and those callers can bypass a `PRAGMA` as easily as a channel, so the trigger buys less than it costs. Re-examined in 0.6.0 and **confirmed**, not merely inherited: the actor now performs exactly the probe a trigger would, on an index built for it, with the statement prepared once. 2 is not enforceable at this layer at all — the file is reachable by any SQLite client, and making `raw()` private would buy the appearance of a guarantee rather than the guarantee. Both are settled; neither is waiting on anything.

**2 narrowed in 0.6.0, without becoming enforceable** ([D-091](s13-decision-register.md#d-091)). The row's second hole was `raw()`, and its listed legitimate uses were `EXPLAIN QUERY PLAN`, read-only reporting on a caller-owned connection, and provoking a guard in a test. `Database::diagnostic_conn()` now serves the first two behind `SQLITE_OPEN_READ_ONLY`, which is a boundary rather than a pragma — measured on libSQL 0.9.30, `PRAGMA query_only = OFF` restores writes on `read_conn()` and does **not** on a diagnostic connection. So `raw()` is `#[doc(hidden)]` and its remaining use is the third one: writing the state this section says the storage layer permits, so the tripwires below can assert the gap is where this row says it is. The free `register_model` / `upsert_embedding` are unchanged and remain the row's third hole.

**Why 3 could be closed when 1 could not.** A `CHECK` is enforced by the engine on every writer, including the raw connection row 2 is about, so it does not have the "constrains only the people already behaving" problem. It costs one comparison per insert rather than an index probe. And it was affordable *now*: SQLite has no `ADD CONSTRAINT`, so the rung is a full rebuild of `links`, which [D-032](s13-decision-register.md#d-032) makes a cheap baseline re-issue pre-1.0 and [D-036](s13-decision-register.md#d-036) makes an unmigration after it.

**Stated so it can fail.** `tests/storage_boundary_tests.rs` asserts each row from the other side. For 1 and 2 that means asserting the gap is still open — raw SQL writes the overlapping pair, `read_conn()` refuses a write where `raw()` does not — so the tests fail if a later migration closes one and leaves this section describing a limit that no longer exists. For 3 and for NaN it now runs the *opposite* way, asserting refusal, so they fail if a gap is ever re-opened. A claim about what the storage layer enforces needs a tripwire exactly as much as a claim about what it does not; NaN is the case that taught that ([D-078](s13-decision-register.md#d-078)), and infinity is the case that showed the cost of getting it wrong. A passing suite there is not a virtue — it is a fact about where the checks currently are.

