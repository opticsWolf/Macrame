//! DDL statements for the Macrame bitemporal schema as specified in §4.

/// GLOB pattern matching the canonical timestamp form `YYYY-MM-DDTHH:MM:SS.ffffffZ`.
///
/// A macro rather than a `const` so it can be spliced into the DDL literals by
/// `concat!`, which only accepts literals. Kept byte-identical to
/// [`crate::util::timestamp::CANONICAL_TS_GLOB`] by the unit test at the bottom
/// of this file — the storage-layer guard and the Rust-layer guard must agree
/// or one of them is decorative.
macro_rules! ts_glob {
    () => {
        "[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]Z"
    };
}

/// Table-level CHECK asserting every temporal column is canonical (§4.1, 0.5.4).
///
/// Timestamps are compared lexicographically everywhere — in SQL predicates, in
/// `MAX(recorded_at)` when the clock recovers its floor, and in Rust `str`
/// ordering. That is sound only if every value has the same width, so mixing
/// `...T00:00:00Z` with `...T00:00:00.000000Z` makes `<=` disagree with
/// chronology and traversals return empty sets with no error. The `Z` suffix
/// alone does not achieve this; a fixed width does, and a CHECK is what makes
/// it a property of the data rather than a convention.
macro_rules! canonical_ts_check {
    ($($col:literal),+ $(,)?) => {
        concat!("CHECK (", $( $col, " GLOB '", ts_glob!(), "' AND ", )+ "1)")
    };
}

/// A macro rather than a `const` for the same reason as [`ts_glob`]: `concat!`
/// splices it into the table DDL and only accepts literals. [`WEIGHT_CHECK`] is
/// the same text as a value, and carries the reasoning.
macro_rules! weight_check {
    () => {
        "CHECK (weight >= 0.0 AND weight < 9e999 AND typeof(weight) = 'real')"
    };
}

/// Table-level CHECK on `links.weight` (§4.7, T2.1, D-083).
///
/// Three clauses. Only the first is the one the item asked for; the other two
/// were found by probing what the first still admits.
///
/// `weight >= 0.0` is the item as written: shortest-path analytics are unsound
/// over negative weights, so Dijkstra and A\* refuse the graph at load time
/// (D-039). Until now that refusal was the *only* place the property was
/// enforced, which made it §4.7's one genuinely open gap — a database this crate
/// wrote by itself could hold a row this crate would not read back.
///
/// `typeof(weight) = 'real'` closes a hole the item does not mention and which
/// probing found. `REAL` in SQLite is an **affinity**, not a type: values that
/// can be converted are, and values that cannot are stored as they came. `'abc'`
/// cannot become a number, so it is stored as TEXT — and in SQLite's type
/// ordering every text value sorts above every numeric one, so `'abc' >= 0.0`
/// is *true* and the first clause passes it through.
///
/// That is not a wrong answer on the read side. It is a **panic**: reading a
/// text `weight` as `f64` reaches `unreachable!("invalid value type")` inside
/// libsql 0.9.30, in whatever unrelated query first touches the row. Measured,
/// not reasoned about — see `examples/weight_check_probe.rs`.
///
/// The clause costs one `typeof` per insert and refuses nothing legitimate:
/// `3`, `'5'` and `1.0` all arrive as REAL through affinity conversion and pass.
/// It is taken **now** rather than in a later rung because SQLite has no
/// `ADD CONSTRAINT` — every clause added later costs another full rebuild of the
/// largest table in the schema.
///
/// `weight < 9e999` refuses `+∞`, and the reason is not the one anybody
/// predicted. The plan expected the CHECK to admit infinity and argued the
/// loader guard would catch it; the guard tests `< 0.0` and `is_nan()`, so it
/// does not. The next guess — mine — was that this is harmless, since IEEE
/// infinity propagates through addition and stays totally ordered, leaving
/// Dijkstra terminating with "that edge is unusable": an odd answer, not a wrong
/// one.
///
/// Both were wrong, and a test found it. **An infinite weight makes the
/// transaction log unreplayable.** The log trigger serialises the row to JSON,
/// and JSON has no representation for infinity, so the payload round-trips into
/// `ReplayCorrupt { reason: "number out of range" }` — every later
/// `reconstruct()` fails, including the one `close()` performs. The ledger is
/// the source of truth under Doctrine III, so a value that cannot survive the
/// log is not an eccentric weight, it is a corrupt one.
///
/// `9e999` is the idiom because SQLite has no `isinf`: the literal overflows to
/// `+∞` on parse, and `inf < inf` is false. Finite values, including `1e308`,
/// pass.
///
/// The loader guard still **stays**, for the reason the constraint cannot cover:
/// `links_current` carries no CHECK, and neither do cold files created before
/// this rung.
pub const WEIGHT_CHECK: &str = weight_check!();

/// The `RAISE(ABORT, …)` messages the schema's guards emit (§4.3).
///
/// Spliced into the trigger DDL *and* matched by [`crate::error::abort_kind`],
/// so the guard and its classifier cannot drift. When they drift the failure is
/// silent in the worst direction: the guard still fires, but the typed error
/// (`SingleOpenViolation`, `RecordedAtRegression`, `ArchiveViolation`) degrades
/// into an opaque `Engine` error that no caller can match on.
macro_rules! abort_single_open {
    () => {
        "macrame: edge already has an open interval; retire it first"
    };
}
macro_rules! abort_monotonic_ra {
    () => {
        "macrame: concept recorded_at must be strictly increasing"
    };
}
macro_rules! abort_delete_guard {
    () => {
        "macrame: physical delete blocked outside archive session"
    };
}
macro_rules! abort_cross_lineage {
    () => {
        "macrame: concept belongs to another lineage; a branch inherits concepts, it does not restate them"
    };
}
macro_rules! abort_branch_immutable {
    () => {
        "macrame: branch_id is provenance and cannot be changed"
    };
}
macro_rules! abort_branches_frozen {
    () => {
        "macrame: branch records are append-only"
    };
}

pub const ABORT_SINGLE_OPEN: &str = abort_single_open!();
pub const ABORT_MONOTONIC_RA: &str = abort_monotonic_ra!();
pub const ABORT_DELETE_GUARD: &str = abort_delete_guard!();
pub const ABORT_CROSS_LINEAGE: &str = abort_cross_lineage!();
pub const ABORT_BRANCH_IMMUTABLE: &str = abort_branch_immutable!();
pub const ABORT_BRANCHES_FROZEN: &str = abort_branches_frozen!();

/// The root lineage every pre-v12 row is stamped with (§15.2, v12, D-214).
///
/// A macro as well as a `const` for [`ts_glob`]'s reason: it is spliced into
/// the column defaults by `concat!`, which takes only literals. One spelling,
/// so the default in the DDL, the seed row, and the Rust layer cannot drift
/// into three databases that disagree about what the trunk is called.
macro_rules! main_branch {
    () => {
        "main"
    };
}
pub const MAIN_BRANCH: &str = main_branch!();

/// The `branch_id` column, identical on all four ledger tables (§15.2, D-214).
///
/// The default is what makes the rung `ALTER TABLE` rather than a rewrite —
/// SQLite records a constant default in the schema header and rewrites no row,
/// measured at 83–139 µs over 20,000 rows in `examples/branch_identity_probe.rs`
/// §1.
///
/// # The `REFERENCES` clause, and the condition it is actually gated on
///
/// SQLite specifies that a column added by `ALTER TABLE … ADD COLUMN` carrying
/// a `REFERENCES` clause **must default to NULL** when foreign keys are
/// enabled, because pre-existing rows cannot be validated against the new
/// parent. That collides head-on with `NOT NULL DEFAULT 'main'`.
///
/// libSQL 0.9.30 applies that rule **dynamically rather than statically**, and
/// probe §15 pins the four cases: the statement is refused only when the table
/// **holds rows** *and* foreign keys are **on**. An empty table takes it with
/// keys on; a populated table takes it with keys off. Being inside a
/// transaction changes nothing either way.
///
/// This is the whole reason the v11 → v12 rung sets
/// [`suspends_foreign_keys`]. It is worth being exact about what that buys,
/// because "suspend the constraint to install the constraint" invites the
/// suspicion that the result is decorative — §15 measures it and it is not.
/// After an ALTER taken with keys suspended the clause is in `sqlite_master`,
/// `PRAGMA foreign_key_list(concepts)` reports the key, an insert naming an
/// unknown branch is refused with extended code 787, and deleting a referenced
/// branch is refused **by the engine** rather than by a trigger. Enforcement is
/// a per-connection pragma; the constraint is schema. Suspending the first
/// never weakened the second.
///
/// Nor does the suspension launder a violation past the commit: `apply_step`
/// runs `PRAGMA foreign_key_check` inside the transaction, and §15 confirms it
/// reports the orphan when one is deliberately planted during the window.
///
/// Taken deliberately, with the dependency named in §19 rather than absorbed.
/// The exposure is narrow and it is on the **upgrade** path only: fresh
/// databases put the clause in a `CREATE TABLE`, where no engine has ever
/// disputed it. If upstream ever tightens to SQLite's static reading, the rung
/// fails **loudly** — at a named step, inside `BEGIN IMMEDIATE`, leaving the
/// database honestly at v11 — and the fallback is one line: drop the clause
/// from the ALTER and let the `branches` write guard carry lineage integrity
/// alone, which is the weaker guarantee and the one D-030 has a name for.
///
/// [`suspends_foreign_keys`]: super::migrations
macro_rules! branch_column {
    () => {
        concat!(
            "branch_id TEXT NOT NULL DEFAULT '",
            main_branch!(),
            "' REFERENCES branches(branch_id)"
        )
    };
}
pub const BRANCH_COLUMN: &str = branch_column!();

/// Marker table probed by the delete guards (D-008 revised).
///
/// The archive session creates this table and drops it again inside the single
/// `BEGIN IMMEDIATE … COMMIT` archive transaction, so it never exists as
/// committed state. Connection-locality — the property the original
/// `temp.sqlite_master` probe was reaching for — is preserved by two
/// independent mechanisms: uncommitted DDL is visible only to the writing
/// connection, and the archive transaction holds the write lock for its
/// duration, so no other connection can reach the guard at all.
pub const ARCHIVE_SESSION_MARKER: &str = "macrame_archive_session";

/// The concepts insert log trigger, **marker-gated since v10** (0.9.0, C3).
///
/// # Why an archive session must not log a concept insert
///
/// Rehydration is a physical move back and mints no transaction-time facts
/// (§2.3): the concept returns to the hot table, the log entries describing it
/// were never removed, and nothing about what was believed — or when — has
/// changed. An unconditional `AFTER INSERT` makes that impossible to honour,
/// because the move *is* an insert.
///
/// **And the damage is worse than a spurious row, which is what forced the
/// rung.** The rehydrated row carries its **original** `recorded_at`, but the
/// log row it would write gets a **new** `seq_id` at the end of the log. The
/// fold partitions by `(table_name, entity_id)` and takes
/// `ROW_NUMBER() OVER (… ORDER BY seq_id DESC) = 1` — last writer wins by
/// *sequence*, not by timestamp. So the rehydration `'I'` would outrank the
/// later `'U'` that retired the concept, and every `reconstruct` after the
/// original creation time would return it **un-retired**. Rehydration would
/// resurrect a belief the ledger had superseded, silently and retroactively,
/// which is precisely what [Doctrine III] forbids.
///
/// Only the *insert* trigger is gated. `trg_concepts_log_update` stays
/// unconditional because nothing inside a session updates a concept — archival
/// deletes and rehydration inserts — so gating it would suppress nothing and
/// widen the hole for no reason.
pub const CREATE_CONCEPTS_LOG_INSERT: &str = concat!(
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_log_insert
    AFTER INSERT ON concepts
    WHEN NOT EXISTS (
        SELECT 1 FROM sqlite_master
        WHERE type = 'table' AND name = '"#,
    "macrame_archive_session",
    r#"'
    )
    BEGIN
        INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at, branch_id)
        VALUES ('concepts', NEW.id, 'I',
                json_object('v', 2, 'title', NEW.title, 'content', NEW.content,
                            'valid_from', NEW.valid_from, 'valid_to', NEW.valid_to,
                            'retired', NEW.retired,
                            'embedding_model', NEW.embedding_model),
                NEW.recorded_at, NEW.branch_id);
    END;
    "#
);

/// The concepts delete guard, **marker-gated since v9** (0.9.0, C2, D-126).
///
/// A `pub const` rather than an anonymous entry in [`CREATE_TRIGGERS`] because
/// two readers need exactly this text: the baseline, which installs it on a new
/// database, and the `v8 → v9` rung, which replaces the v8 body on an existing
/// one. A second copy is a copy that drifts, and this trigger is the one whose
/// body carries a doctrine decision.
///
/// # What changed, and why re-issuing the baseline could not do it
///
/// Through v8 this guard was **unconditional**: `BEFORE DELETE ON concepts`
/// aborting every time, on the reasoning that concepts are never physically
/// archived ([D-022](../../docs/architecture/s13-decision-register.md)). C2
/// makes that false — a declared archive session may now move a retired,
/// unreferenced concept to the cold file — so the guard takes the same shape its
/// two siblings have had since 0.5.3: it fires **unless** the archive-session
/// marker is present.
///
/// It needs a rung of its own, and that was measured rather than assumed
/// (D-126). `CREATE TRIGGER IF NOT EXISTS` on an existing name keeps the **old
/// body** — re-issuing the baseline against a v8 database leaves the
/// unconditional guard exactly where it was — and `verify` compared `type` and
/// `name` and never bodies, so the stale guard passed verification in silence.
/// Both halves are now closed: the rung drops and recreates, and `verify`
/// checks that every delete guard's body probes the marker.
pub const CREATE_CONCEPTS_GUARD_DELETE: &str = concat!(
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_guard_delete
    BEFORE DELETE ON concepts
    WHEN NOT EXISTS (
        SELECT 1 FROM sqlite_master
        WHERE type = 'table' AND name = '"#,
    "macrame_archive_session",
    r#"'
    )
    BEGIN
        SELECT RAISE(ABORT, '"#,
    abort_delete_guard!(),
    r#"');
    END;
    "#
);

/// The `concepts` ledger table (§4.1).
///
/// # `rowid_pk` is explicit, and that is the whole point (v8, D-119)
///
/// Through v7 this table declared `id TEXT PRIMARY KEY`, which left its rowid
/// **implicit** — and `concepts_fts` is external-content keyed on that rowid.
/// `VACUUM` renumbers implicit rowids, which would silently decouple the search
/// index from the rows it indexes: no error, no integrity-check failure, just
/// results that stop matching.
///
/// [D-071](../../docs/architecture/s13-decision-register.md) proved the hazard
/// unreachable *by consequence rather than by design* — `trg_concepts_guard_delete`
/// is unconditional, so rowids are dense `1..n` and `VACUUM`'s renumbering is
/// the identity map. 0.9.0's archival makes them sparse and makes the hazard
/// real, so v8 replaces the accident with a column: an `INTEGER PRIMARY KEY` is
/// a stored value, and `VACUUM` preserves it whether the numbering is dense or
/// not (measured in `examples/concepts_rebuild_probe.rs` §5).
///
/// SQLite permits one primary key per table, so `id` becomes `NOT NULL UNIQUE`.
/// That keeps it a valid foreign-key parent for `links.source_id` /
/// `links.target_id` and keeps `ON CONFLICT(id)` working, but it **is** a
/// primary-key change — which [D-036](../../docs/architecture/s13-decision-register.md)
/// forbids outright after 1.0. Taken pre-1.0 on purpose, or never.
/// The lineage register (§15.2, v12, D-214).
///
/// Four columns and no more, because a branch is **not** a third temporal axis
/// (Doctrine II): it carries no interval of its own, only the point in the
/// second clock where it diverged.
///
/// `parent_id` is a self-referencing foreign key, declarable here because it
/// sits in a `CREATE TABLE` where SQLite permits forward and self references
/// freely. `NULL` marks the root, and the paired `CHECK` makes "root" a single
/// state rather than two columns that can disagree: a row with a parent and no
/// fork point is a lineage whose ancestry cannot be resolved, and a row with a
/// fork point and no parent is a divergence from nothing.
///
/// `forked_at` is in the **`recorded_at` domain** — the transaction-time
/// instant the lineage diverged, which is what §15.3's visibility cutoffs are
/// computed over. Not a valid-time bound: a branch does not believe things
/// about a period, it believes them from a moment onward.
///
/// The ordering `CHECK` is row-local on purpose. `forked_at <= created_at` is
/// checkable from the row itself; an ordering against the *parent's* row is
/// not, and a `CHECK` cannot see another row. The cross-row half is `fork()`'s
/// to enforce at D-034's boundary, and saying so here is cheaper than a
/// constraint that looks complete and is not.
///
/// **Which cross-row ordering, corrected in 0.14.7.** This said "the fork point
/// is at or after the parent's *creation*" from v12 until `fork()` existed to
/// enforce it, and that turned out to be uncheckable rather than merely
/// unenforced: [`seed_root_branch`](crate::schema) stamps the trunk's
/// `created_at` from `SystemTime::now()` during migration — before the
/// database's injected clock is resolved, and it cannot simply run after,
/// because the clock's floor is read from tables the migration creates. So
/// `created_at` is not on the ledger's timeline and comparing a `forked_at` to
/// it is comparing two clocks. What [`Database::fork`](crate::Database::fork) enforces instead is
/// `forked_at >= parent.forked_at`, both issued by the same clock, which makes
/// fork points non-decreasing down any root path.
pub const CREATE_BRANCHES_TABLE: &str = concat!(
    r#"
CREATE TABLE IF NOT EXISTS branches (
    branch_id   TEXT NOT NULL PRIMARY KEY,
    parent_id   TEXT REFERENCES branches(branch_id),
    forked_at   TEXT,
    created_at  TEXT NOT NULL,
    CHECK ((parent_id IS NULL) = (forked_at IS NULL)),
    CHECK (forked_at IS NULL OR forked_at <= created_at),
    CHECK (forked_at IS NULL OR forked_at GLOB '"#,
    ts_glob!(),
    r#"'),
    "#,
    canonical_ts_check!("created_at"),
    r#"
);
"#
);

/// Seed the root lineage, idempotently.
///
/// One statement shared by the baseline and the v11 → v12 rung, taking
/// `created_at` as a parameter. `OR IGNORE` rather than `IF NOT EXISTS`
/// gymnastics because both callers may run against a database that already has
/// the row — the rung on a retry, the baseline never, but a single statement
/// that is safe for both is one fewer thing to reason about.
///
/// This must run **before** any row is stamped, on both paths: every
/// `branch_id` default names `'main'`, and the foreign key means a database
/// without this row cannot accept a single write.
pub const SEED_MAIN_BRANCH: &str = concat!(
    "INSERT OR IGNORE INTO branches (branch_id, parent_id, forked_at, created_at) \
     VALUES ('",
    main_branch!(),
    "', NULL, NULL, ?1)"
);

/// `branches` is append-only outside an archive session (§15.2, §15.4).
///
/// The two guards no longer say the same thing, and 0.14.13 is where they
/// parted. This one stays **unconditional**: no session of any kind may edit a
/// lineage record in place. [`CREATE_BRANCHES_GUARD_DELETE`] is now gated on
/// the archive-session marker like its three siblings, because
/// [`crate::Database::archive_branch`] made removing a lineage record a legal
/// operation — see that guard for what changed and why the change needed a
/// rung of its own.
///
/// # Why `UPDATE` is refused whole-row
///
/// The foreign key already refuses renaming or deleting a lineage any row
/// still points at, so this guard is not what keeps the ledger from being
/// orphaned. What it keeps is narrower and harder to see: `parent_id` and
/// `forked_at` are the inputs to ancestry, so editing either **re-derives the
/// visibility of rows already written**, with no new assertion anywhere. That
/// is the move [Doctrine III] forbids, reachable by one raw-SQL statement, and
/// no foreign key has anything to say about it.
///
/// Whole-row rather than a named subset because nothing on the row legitimately
/// changes, and a whole-row guard needs no maintenance the day a column is
/// added.
///
/// [Doctrine III]: ../../docs/architecture/README.md
pub const CREATE_BRANCHES_GUARD_UPDATE: &str = concat!(
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_branches_frozen_update
    BEFORE UPDATE ON branches
    BEGIN
        SELECT RAISE(ABORT, '"#,
    abort_branches_frozen!(),
    r#"');
    END;
    "#
);

/// The delete half of the rule, **marker-gated since v13** (0.14.13, §15.4,
/// [D-230](../../docs/architecture/s13-decision-register.md#d-230)).
///
/// # What changed
///
/// Through v12 this guard was unconditional, and its own docstring said why:
/// *"there is no session in which removing a lineage record is legal — branches
/// are never archived"*. [`crate::Database::archive_branch`] makes that false.
/// The sentence was a true description of the operations that existed, written
/// as though it were a property of the table, which is the shape D-035 asks to
/// be stated rather than assumed.
///
/// **The lineage row must move, and that is forced rather than chosen.** An
/// abandonment arm that took the branch's `links` and left its `branches` row
/// would leave `hot_log_reach` unsound: that probe's argument rests on *the
/// newest row per entity is never archivable*, which holds for a predicate
/// needing a later row to exist and fails for one that takes a whole lineage.
/// Moving the `branches` row is what makes a hot fold that omits the lineage
/// **correct rather than silently short** — every read and write naming the
/// name now raises [`crate::DbError::UnknownBranch`], which is a refusal, not a
/// wrong answer.
///
/// # Why it needed a rung
///
/// [`CREATE_CONCEPTS_GUARD_DELETE`]'s reason, measured once already (D-126):
/// `CREATE TRIGGER IF NOT EXISTS` on an existing name keeps the **old body**,
/// so re-issuing the baseline against a v12 database leaves the unconditional
/// guard exactly where it is and `archive_branch` fails on every ledger that
/// was not created by this build. The v12 → v13 rung drops and recreates, and
/// `verify` now carries this name in `DELETE_GUARDS`, so a database whose
/// guard predates the change is refused at open with a sentence rather than at
/// archive time with a trigger abort.
///
/// The update guard is deliberately **not** gated — see
/// [`CREATE_BRANCHES_GUARD_UPDATE`]. Archival is a move; there is still no
/// session in which editing a lineage's parent or fork point is legal, and
/// gating both would have suspended a rule the operation does not need
/// suspended.
pub const CREATE_BRANCHES_GUARD_DELETE: &str = concat!(
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_branches_frozen_delete
    BEFORE DELETE ON branches
    WHEN NOT EXISTS (
        SELECT 1 FROM sqlite_master
        WHERE type = 'table' AND name = '"#,
    "macrame_archive_session",
    r#"'
    )
    BEGIN
        SELECT RAISE(ABORT, '"#,
    abort_branches_frozen!(),
    r#"');
    END;
    "#
);

/// A branch inherits concepts; it does not restate them (§15.2, D-214).
///
/// `concepts` is a current-state projection keyed by identity — `id` is
/// `NOT NULL UNIQUE` and the write path uses `ON CONFLICT(id) DO UPDATE` — so
/// two lineages holding different beliefs about one concept is two rows with
/// one `id`, which the unique index refuses on its own (probe §2). What it
/// refuses it refuses as a *constraint failure*, naming nothing; this guard
/// turns the same refusal into a sentence that says which rule was broken.
///
/// It fires **before** `ON CONFLICT` is considered, which is not obvious and
/// was measured rather than assumed (probe §7): a cross-lineage upsert is
/// refused, a same-lineage one is accepted, and a new id is accepted.
///
/// Exact-branch equality, not ancestry. A branch that may restate its parent's
/// concepts is the overlay design, and the overlay is deferred with its reopen
/// trigger named (D-214) — a guard that quietly permitted the ancestry case
/// would ship half of it with none of the machinery that makes it correct.
pub const CREATE_CONCEPTS_GUARD_LINEAGE: &str = concat!(
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_cross_lineage
    BEFORE INSERT ON concepts
    WHEN EXISTS (
        SELECT 1 FROM concepts
        WHERE id = NEW.id AND branch_id <> NEW.branch_id
    )
    BEGIN
        SELECT RAISE(ABORT, '"#,
    abort_cross_lineage!(),
    r#"');
    END;
    "#
);

/// `branch_id` records where a row was minted, and minting happened once.
///
/// The column is **provenance, not identity** (D-214), and the distinction is
/// exactly what this guard keeps true. An `UPDATE` that moved a concept between
/// lineages would rewrite where a belief came from without asserting anything
/// new — the same shape as editing `branches.parent_id`, and forbidden for the
/// same reason.
pub const CREATE_CONCEPTS_GUARD_BRANCH: &str = concat!(
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_branch_immutable
    BEFORE UPDATE ON concepts
    WHEN NEW.branch_id <> OLD.branch_id
    BEGIN
        SELECT RAISE(ABORT, '"#,
    abort_branch_immutable!(),
    r#"');
    END;
    "#
);

pub const CREATE_CONCEPTS_TABLE: &str = concat!(
    r#"
CREATE TABLE IF NOT EXISTS concepts (
    rowid_pk         INTEGER PRIMARY KEY,
    id               TEXT NOT NULL UNIQUE,
    title            TEXT NOT NULL,
    content          TEXT NOT NULL DEFAULT '',
    embedding_model  TEXT,
    valid_from       TEXT NOT NULL,
    valid_to         TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    recorded_at      TEXT NOT NULL,
    retired          INTEGER NOT NULL DEFAULT 0,
    "#,
    branch_column!(),
    r#",
    "#,
    canonical_ts_check!("valid_from", "valid_to", "recorded_at"),
    r#"
);
"#
);

pub const CREATE_LINKS_TABLE: &str = concat!(
    r#"
CREATE TABLE IF NOT EXISTS links (
    source_id   TEXT NOT NULL REFERENCES concepts(id),
    target_id   TEXT NOT NULL REFERENCES concepts(id),
    edge_type   TEXT NOT NULL,
    valid_from  TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    valid_to    TEXT NOT NULL DEFAULT '9999-12-31T23:59:59.999999Z',
    weight      REAL NOT NULL DEFAULT 1.0,
    properties  TEXT NOT NULL DEFAULT '{}',
    "#,
    branch_column!(),
    r#",
    PRIMARY KEY (source_id, target_id, edge_type, valid_from, recorded_at),
    "#,
    weight_check!(),
    r#",
    "#,
    canonical_ts_check!("valid_from", "valid_to", "recorded_at"),
    r#"
);
"#
);

pub const CREATE_LINKS_CURRENT_TABLE: &str = concat!(
    r#"
CREATE TABLE IF NOT EXISTS links_current (
    source_id   TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    edge_type   TEXT NOT NULL,
    valid_from  TEXT NOT NULL,
    valid_to    TEXT NOT NULL,
    weight      REAL NOT NULL,
    properties  TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    "#,
    branch_column!(),
    r#",
    PRIMARY KEY (source_id, target_id, edge_type, valid_from, branch_id),
    "#,
    canonical_ts_check!("valid_from", "valid_to", "recorded_at"),
    r#"
);
"#
);

pub const CREATE_TRANSACTION_LOG_TABLE: &str = concat!(
    r#"
CREATE TABLE IF NOT EXISTS transaction_log (
    seq_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    table_name  TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    operation   TEXT NOT NULL,
    payload     TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    "#,
    branch_column!(),
    r#",
    "#,
    canonical_ts_check!("recorded_at"),
    r#"
);
"#
);

/// The per-model embedding table (§4.1, D-005), for a validated model name.
///
/// A function rather than a `const` because the table's identity *and its
/// column type* both depend on the model: `F32_BLOB(dim)` carries the declared
/// dimension in the schema, which is what [`crate::vector::declared_dimension`]
/// reads back so the crate never keeps a second copy of it.
///
/// Deliberately not part of the baseline migration. Which models exist is an
/// application's choice made over time, not a property of the schema version,
/// and D-036 classifies these tables as disposable periphery: a migration may
/// drop one and re-embed. `IF NOT EXISTS` makes registration idempotent.
///
/// No temporal columns, on purpose. Doctrine VII makes an embedding a derived
/// artifact of a model applied to content — it has no valid time of its own, and
/// giving it a `recorded_at` would put a third clock next to the two §2 permits
/// and invite queries that mix them.
pub fn create_embeddings_table(model: &crate::vector::ModelName, dim: usize) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {table} (
    concept_id  TEXT PRIMARY KEY REFERENCES concepts(id),
    embedding   F32_BLOB({dim}) NOT NULL
);",
        table = model.table(),
    )
}

/// The DiskANN index over a model's vectors.
///
/// **Load-bearing for correctness, not only for speed.** Measured against
/// libSQL 0.9.30: a blob of the wrong length inserted into an `F32_BLOB(4)`
/// column is *accepted* while no vector index exists, and rejected — with the
/// row not landing — once one does. §4.1 previously claimed the column type
/// enforced its own dimension at insert time; it does not. So this index is
/// created together with the table it indexes and is never optional, and
/// dropping it to speed up a bulk load would silently disarm the only
/// storage-layer check on dimension.
pub fn create_embeddings_index(model: &crate::vector::ModelName) -> String {
    format!(
        "CREATE INDEX IF NOT EXISTS {index} ON {table} (libsql_vector_idx(embedding));",
        index = model.index(),
        table = model.table(),
    )
}

/// Derived analytics output, keyed by concept and label (§5.4, D-041).
///
/// Deliberately outside the ledger. Three properties are load-bearing and each
/// is the opposite of what the four normative tables above do.
///
/// **No log trigger.** Nothing in [`CREATE_TRIGGERS`] fires on this table, so an
/// annotation never reaches `transaction_log`. That is Doctrine VII's reasoning
/// about embeddings applied to the other derived artifact: a community label is
/// a function of an algorithm, a version of that algorithm, and a graph — not a
/// statement about the world, and a ledger that records it is recording the
/// analytics schedule as though it were history. A reconstruction that wants
/// labels recomputes them, which is the only honest way to ask what a past
/// graph's communities *were*.
///
/// **No delete guard.** Doctrine V protects the hot ledger tables; this table is
/// derivative state in Doctrine VI's second category, so wiping it must stay a
/// legal, ordinary operation — a rerun replaces the previous pass, and dropping
/// the whole table costs nothing but the recomputation.
///
/// **Upsert on `(concept_id, label)`.** One current value per label per concept.
/// Storing a history of successive runs here would be the ledger again, by
/// another name.
///
/// The foreign key is safe in a way `links_current`'s omitted ones are not:
/// concepts are never physically deleted (D-022), and this table is rebuilt by
/// re-running an algorithm that read `concepts` in the first place, so there is
/// no insertion-order problem to solve.
pub const CREATE_ANALYTICS_ANNOTATIONS_TABLE: &str = concat!(
    r#"
CREATE TABLE IF NOT EXISTS analytics_annotations (
    concept_id  TEXT NOT NULL REFERENCES concepts(id),
    label       TEXT NOT NULL,
    value       TEXT NOT NULL,
    computed_at TEXT NOT NULL,
    PRIMARY KEY (concept_id, label),
    "#,
    canonical_ts_check!("computed_at"),
    r#"
);
"#
);

/// The keyword half of hybrid search: an FTS5 index over concept text (§5.9).
///
/// **External content.** The table declares `content='concepts'`, so the tokens
/// are indexed but the text itself is not duplicated — FTS5 reads it back from
/// `concepts` by rowid when it needs a column value. Two reasons beyond the
/// storage saving, and the second is the one that decided it:
///
/// * There is exactly one copy of the text, so the index cannot disagree with
///   the concept about what the concept says. A standalone FTS table would be a
///   second description of data the ledger already holds, which is the failure
///   class D-030 and D-035 exist to prevent.
/// * `INSERT INTO concepts_fts(concepts_fts) VALUES('rebuild')` reconstructs the
///   whole index from the content table in one statement. D-036 requires every
///   derivative table to be rebuildable from the ledger, and here that is the
///   engine's own operation rather than code of ours that has to be kept honest.
///
/// The cost is that external-content tables do not maintain themselves: an
/// `UPDATE` must retract the *old* terms before adding the new ones, using the
/// old column values. That is what `trg_concepts_fts_update` does, and getting
/// it wrong leaves an index that still matches text no concept contains.
///
/// **`content_rowid` names `rowid_pk`, not `rowid` (v8, D-119).** They are the
/// same value — an `INTEGER PRIMARY KEY` *is* the rowid — but naming the column
/// is what makes the key a declared one rather than an implicit one `VACUUM` is
/// free to renumber. See [`CREATE_CONCEPTS_TABLE`].
pub const CREATE_CONCEPTS_FTS: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS concepts_fts USING fts5(
    title,
    content,
    content='concepts',
    content_rowid='rowid_pk'
);
"#;

/// FTS5's own consistency check — **and it cannot see the failure that matters**
/// (§5.9, D-071).
///
/// Kept as a named constant so the finding has somewhere to live, and used by
/// `an_emptied_fts_index_still_passes_integrity_check`, which is a tripwire
/// rather than a guarantee.
///
/// On this libSQL build (0.9.30), `'integrity-check'` verifies the index's
/// *internal* consistency and not its agreement with the content table. Measured:
/// after `'delete-all'` the index answers zero matches where it answered ten, and
/// both `'integrity-check'` and `'integrity-check', 0` still report success. So a
/// `verify_fts()` built on this would report a healthy index for an empty one —
/// which is why there is no `verify_fts()`. See D-071.
pub const VERIFY_CONCEPTS_FTS: &str =
    "INSERT INTO concepts_fts (concepts_fts) VALUES ('integrity-check');";

/// Reconstruct the FTS index from `concepts` (§5.9, D-036).
///
/// The engine's own operation, so the rebuild path is not a second
/// implementation of the triggers that could drift from them.
pub const REBUILD_CONCEPTS_FTS: &str =
    "INSERT INTO concepts_fts (concepts_fts) VALUES ('rebuild');";

/// Refresh the query planner's statistics (0.12.4, D-149).
///
/// # Why this exists at all
///
/// Until 0.12.4 nothing in this crate ever ran `ANALYZE`, so `sqlite_stat1` did
/// not exist in any database Macrame had created and **every plan was costed
/// against SQLite's built-in defaults**: assume ~1M rows, assume each bound
/// equality column divides the search by ten. That estimate is *structural* — a
/// function of how many columns a query binds, not of what the table holds.
///
/// Which is a restatement of this schema's own worst recurring defect. From
/// `tests/index_plan_tests.rs`: *"a covering index captures a query because it
/// contains the columns, not because it discriminates."* D-042, D-059 and D-064
/// are three instances of a planner doing the only thing available to it.
/// [`CREATE_INDICES`] declares two indices that both lead on `source_id`, and
/// with no statistics the planner separates them by column count alone.
///
/// # Bounded by construction
///
/// `ANALYZE` is a **write** — it writes `sqlite_stat1` and takes the write lock —
/// so unbounded on a populated `links_current` it is exactly the kind of
/// unbudgeted hold `CHUNK_BUDGET` exists to prevent. [`ANALYSIS_LIMIT`], set once
/// per connection in `configure`, caps the rows examined per index and makes the
/// cost a function of the index count rather than the table size. That is what
/// lets this be scheduled as ordinary low-priority work.
pub const ANALYZE: &str = "ANALYZE;";

/// Re-analyse only what has gone stale (0.12.4, D-149).
///
/// SQLite tracks how much each table has changed since its last analysis and
/// runs `ANALYZE` only where it believes the statistics no longer hold. A no-op
/// when nothing has moved, which is what makes it safe to call on a schedule
/// rather than only on demand.
///
/// Bounded by [`ANALYSIS_LIMIT`] like everything else on the connection.
pub const OPTIMIZE: &str = "PRAGMA optimize;";

/// The row cap that makes [`ANALYZE`] budgetable.
///
/// 400 is SQLite's own documented recommendation. It buys approximate statistics
/// in roughly constant time instead of exact statistics in time proportional to
/// the table — and approximate is emphatically enough here, because the decision
/// being informed is *which of two indices discriminates*, not a cardinality
/// estimate anyone reads.
///
/// Set on the connection rather than around each call, so it also bounds the
/// analysis [`OPTIMIZE`] triggers internally. A limit that applied only to the
/// explicit path would leave the scheduled one unbounded, which is the half that
/// runs without anybody watching.
///
/// # Measured in 0.12.23: it is a constant factor, not a bound (D-166)
///
/// D-149 claimed this makes `ANALYZE`'s cost "a function of the index count
/// rather than the table size". Measured on this schema — `examples/analyze_hold.rs`,
/// which times the crate's own hold beside the same file analysed with the
/// pragma off and on:
///
/// | edges | crate's hold | limit off | limit 400 |
/// |---|---|---|---|
/// | 10,000 | 5.26 ms | 18.4 ms | 6.01 ms |
/// | 40,000 | 19.1 ms | 78.6 ms | 19.4 ms |
///
/// The pragma **is** in force — the crate's hold tracks the capped arm and not
/// the uncapped one, which is how it is established at all, since the
/// connection that runs `ANALYZE` is the actor's and no test can reach it. It
/// is worth 3.1× at 10,000 edges and 4.1× at 40,000.
///
/// What it does not do is remove the table from the equation: over that 4×
/// range the capped time grew 3.2×. So `analyze()` on a 40,000-edge ledger
/// holds the write lock for ~19 ms, about 6× [`crate::CHUNK_BUDGET`], and
/// [`crate::metrics::CommandKind::Analyze`] is **not** budget-exempt — it
/// appears in `metrics().budget_violations()` and always had.
///
/// Since 0.13.24 that kind is `analyze()` alone; `optimize()` reports as
/// [`crate::metrics::CommandKind::Optimize`] and is separately, deliberately
/// not exempt (W10.5,
/// [D-197](../../docs/architecture/s13-decision-register.md#d-197)).
pub const ANALYSIS_LIMIT: &str = "PRAGMA analysis_limit = 400";

/// Every index the schema declares.
///
/// # Two entries left in v8, and why the list is now allowed to be short
///
/// `idx_annotations_label` and `idx_lc_tgt_active` were dropped by the v7 → v8
/// rung ([D-089](../../docs/architecture/s13-decision-register.md), completed by
/// D-118). Neither had a reader anywhere in the crate — `analytics_annotations`
/// is never selected from here at all, and no query seeks on
/// `links_current.target_id` as a leading column — so each was an index write
/// per insert, forever, buying nothing. One of them was on the crate's hottest
/// write path.
///
/// `tests/index_plan_tests.rs` now requires the unread set to be **empty**,
/// which turns "these two are known bad" into "an index with no reader is a red
/// test". That is the guarantee this list is kept short by.
///
/// # `idx_links_target` is not `idx_lc_tgt_active` coming back
///
/// The two look like the same index and are not, which is worth stating because
/// the resemblance is the trap. `idx_lc_tgt_active` was `(target_id, valid_to)`
/// on **`links_current`**, the materialized projection, and it was dropped
/// because *nothing in the crate seeks on it* — no reader, pure write cost.
/// `idx_links_target` is `(target_id)` on **`links`**, the ledger, and it exists
/// because `CONCEPTS_ARCHIVABLE` seeks on exactly that column and the plan is
/// measured before and after.
///
/// D-089's rule was never "no index on a target column". It was "an index needs
/// a named query that seeks on it", and the registry is what enforces the
/// difference rather than this paragraph.
/// The two indices on `links_current`, named because a rung has to restore
/// them (§15.2, D-214).
///
/// `links_current` is derivative, so the v11 → v12 rung re-creates it rather
/// than altering it — and `DROP TABLE` takes the table's indices with it.
/// Neither [`CREATE_LINKS_CURRENT_TABLE`] nor `rebuild_within` puts them back:
/// the first declares a table and the second fills one. The open-time schema
/// verifier is what noticed, which is the argument for having it.
///
/// `pub(crate)` rather than `pub`: every other const this module publishes
/// describes the schema a caller might want to read, and these two exist
/// only so a rung can put back what its own `DROP TABLE` removed. The
/// published form of an index is still [`CREATE_INDICES`], which contains
/// both of these.
///
/// Named consts rather than a `CREATE_INDICES` scan for `ON links_current`,
/// because a rung should state which indices it owes rather than derive the
/// list from a definition that will keep changing after it. If a later release
/// adds a third index here, that release's rung adds it — this one is a
/// statement about v12 and stays one.
pub(crate) const LC_TRAVERSAL_COVER: &str = "CREATE INDEX IF NOT EXISTS \
     idx_lc_traversal_cover ON links_current \
     (source_id, valid_from, valid_to, weight, edge_type, target_id);";

/// See [`LC_TRAVERSAL_COVER`].
pub(crate) const LC_OPEN_INTERVAL: &str = "CREATE INDEX IF NOT EXISTS \
     idx_lc_open_interval ON links_current \
     (source_id, target_id, edge_type, valid_to, valid_from);";

/// The lineage read's own index (0.14.14, §15.4, D-231, shipped v13 -> v14).
///
/// See [`CREATE_INDICES`] for what seeks on it and why it is a **second** index
/// rather than a column added to [`LC_TRAVERSAL_COVER`], which is what §15.4
/// asked for.
pub(crate) const LC_LINEAGE_CUT: &str = "CREATE INDEX IF NOT EXISTS \
     idx_lc_lineage_cut ON links_current \
     (branch_id, recorded_at, source_id, target_id, edge_type, valid_from, \
      valid_to, weight);";

pub const CREATE_INDICES: &[&str] = &[
    // Covering index for the traversal CTE (§5.2, D-042).
    //
    // Column order is load-bearing and was measured with EXPLAIN QUERY PLAN.
    // The seek column is `source_id`; everything after it is there so the
    // recursive step never touches the base table. The two range columns come
    // next and `edge_type` comes *after* them, because `edge_types` is empty
    // unless a caller sets it: with `edge_type` in second position SQLite
    // declines the index for the unfiltered traversal — the default one — and
    // silently falls back to a non-covering plan.
    //
    //   (source_id, edge_type, valid_from, ...)   filtered: COVERING
    //                                             unfiltered: NOT covering
    //   (source_id, valid_from, valid_to, weight, edge_type, target_id)
    //                                             both: COVERING
    //
    // This subsumes the former idx_lc_src_active (source_id, valid_to): same
    // prefix column, strictly more payload. Keeping both would pay two index
    // writes per assertion on a table that already takes three writes.
    LC_TRAVERSAL_COVER,
    // The single-open-interval probe's own index (D-059, shipped v5 -> v6).
    //
    // `trg_links_single_open` runs an `EXISTS` on every edge insert, keyed on
    // (source_id, target_id, edge_type, valid_to) with valid_from as an
    // inequality. Before this index the planner served that probe from
    // `idx_lc_traversal_cover` with only `source_id` bound — it wins as a
    // covering index over the primary-key autoindex, which lacks `valid_to` —
    // so **every insert scanned its source's entire out-degree**. Measured on a
    // fixed 90-row chunk: 4.4 ms into an empty table, 18.4 ms into a
    // 2,000-edge hub, 47.7 ms into an 8,000-edge one, and 1.06 s into 90,000.
    // Growth in the table, not in the chunk.
    //
    // With this index the same 90 rows into the 8,000-edge hub take 8.0 ms and
    // stay flat. It matters beyond bulk import: the probe is on the insert path,
    // so an interactive `assert_edge` against a high-degree node paid the same
    // scan, and that is the path CHUNK_BUDGET's 3 ms exists to protect.
    //
    // Column order follows the trigger's WHERE exactly — the three equalities
    // first, then `valid_to` which is compared to the sentinel, then
    // `valid_from` which is the `<>` and cannot be a seek column. This does not
    // subsume `idx_lc_traversal_cover` and is not subsumed by it: that one leads
    // on `source_id` alone for the recursive walk, this one needs all three
    // equality columns bound. Both are kept, which is a fourth index write per
    // assertion buying a scan's removal from the same operation.
    LC_OPEN_INTERVAL,
    // The lineage read's base scans (0.14.14, W12.14, §15.4, D-231, shipped
    // v13 -> v14).
    //
    // `graph::lineage::churned_cte` and `links_cut_cte` are the only two
    // statements in the crate that read `links_current` **by lineage**, and
    // five call sites emit them: the traversal, `query_as_of_edges_on`,
    // `load_subgraph_with` and `diff`'s two tagged copies. Both drive from the
    // materialised `lineage` set — `JOIN lineage g ON g.branch_id =
    // lc.branch_id` — and both then compare `lc.recorded_at` to that lineage's
    // cutoff. So the seek is `(branch_id, recorded_at)` and the payload is
    // every other column the two arms project.
    //
    // Without it SQLite builds the index itself, three times per branched read:
    // `AUTOMATIC PARTIAL COVERING INDEX (branch_id=?)` twice over
    // `links_current` and once over the `links_cut` co-routine. The third is
    // not a table and no index can serve it; the first two are, and this is
    // them. Measured (`examples/branch_index_rung_probe.rs`, 1,110 edges,
    // chain of 10, best of 25):
    //
    //   branched read, no post-fork churn    6.50 -> 5.43 ms   1.20x
    //   branched read, 10% post-fork churn  16.94 -> 7.45 ms   2.28x
    //   trunk traversal                      1.64 -> 1.64 ms   unchanged
    //   2,000 assertions                     18.3 -> 20.6 ms   +12.6%
    //
    // **Why a second index and not a column on `idx_lc_traversal_cover`,
    // against what §15.4 and D-219 both say.** D-219 measured three shapes and
    // preferred folding `branch_id` in after the range columns; it measured
    // them against `branch_id IN (ancestry)`, which that same probe run proved
    // is not a resolution and which 0.14.4 therefore did not ship. Under the
    // reader that did ship, the walk joins a CTE and never touches this table,
    // so the folded shape is never consulted and buys **nothing** — 6.18 vs
    // 6.27 ms, inside the run-to-run spread. And every single-index shape that
    // leads on `branch_id` — the one §15.3 proposed included — takes the trunk
    // walk off its covering index altogether:
    //
    //   SEARCH l USING INDEX idx_lc_open_interval (source_id=?)
    //
    // one bound column and not covering, which is what
    // `the_shipped_traversal_cte_stays_on_the_covering_index` exists to refuse.
    // The two shapes stopped sharing an access path when the reader stopped
    // walking `links_current` directly, so they can no longer share an index.
    LC_LINEAGE_CUT,
    "CREATE INDEX IF NOT EXISTS idx_txlog_time ON transaction_log (recorded_at);",
    "CREATE INDEX IF NOT EXISTS idx_txlog_entity ON transaction_log (entity_id);",
    // The archive cutoff's seek column on the ledger table (0.12.6, W3.1,
    // D-151, review §2.1, shipped v10 -> v11).
    //
    // `links` carried a primary key and nothing else. `LINKS_ARCHIVABLE` opens
    // with `recorded_at < :cutoff`, and the primary key leads on `source_id`, so
    // there was nothing for that bound to seek on: both the archiving SELECT and
    // the archiving DELETE scanned the entire ledger. Measured on the
    // populated-and-analysed fixture in `tests/index_plan_tests.rs`:
    //
    //   before   SCAN links | CORRELATED SCALAR SUBQUERY 1 | SEARCH newer ...
    //   after    SEARCH links USING INDEX idx_links_recorded_at (recorded_at<?)
    //
    // The inner supersession probe was never the problem — it binds the whole
    // primary-key prefix and always did.
    //
    // **This index is justified on those two queries and not on the clock
    // floor.** Review §2.1 led with `recorded_at_floor`, the `MAX(recorded_at)`
    // read on every `open()`, and counted it among the scans this would close.
    // It is not one: SQLite already served the bare `MAX()` from the primary
    // key's covering index without traversing the table, and after this index it
    // does the same thing through a different covering index. The startup cost
    // the review predicted did not exist, so the justification rests entirely on
    // the archive path — see D-150 for how that was caught, and D-089 for why an
    // index bought on a believed benefit is the failure mode being avoided.
    "CREATE INDEX IF NOT EXISTS idx_links_recorded_at ON links (recorded_at);",
    // The other half of the concept-archival reachability check (0.12.6, W3.2,
    // D-151, review §2.2, shipped v10 -> v11).
    //
    // `CONCEPTS_ARCHIVABLE` asks whether any surviving link mentions a concept
    // *in either direction*: `links.source_id = concepts.id OR links.target_id =
    // concepts.id`. The primary key serves the left arm. Nothing served the
    // right one, and an `OR` is only as seekable as its worst arm, so the whole
    // correlated subquery degraded to a scan of `links` **once per candidate
    // concept** — O(concepts x links).
    //
    //   before   CORRELATED SCALAR SUBQUERY 1
    //              | SCAN links USING COVERING INDEX sqlite_autoindex_links_1
    //   after    CORRELATED SCALAR SUBQUERY 1 | MULTI-INDEX OR
    //              | INDEX 1 | SEARCH links USING COVERING INDEX
    //                           sqlite_autoindex_links_1 (source_id=?)
    //              | INDEX 2 | SEARCH links USING INDEX
    //                           idx_links_target (target_id=?)
    //
    // `MULTI-INDEX OR` is SQLite deciding to run both arms as seeks and union
    // the rowids, which is exactly the plan the index was added to make
    // available. Both the SELECT and the DELETE form pick it up.
    //
    // A single-column index on the ledger's hottest write path needs the
    // strongest justification available, and it has one beyond the plan shape:
    // *before* this index the planner was building `AUTOMATIC COVERING INDEX
    // (target_id=?)` at query time to answer the same question. It had already
    // concluded the index was worth having and was paying to construct a
    // throwaway copy per statement.
    "CREATE INDEX IF NOT EXISTS idx_links_target ON links (target_id);",
];

/// Every trigger the schema declares.
///
/// **`IF NOT EXISTS` means a changed body does not reach an existing file.**
/// `migrations::verify` checks trigger *presence by name*, which is deliberate
/// (a count refuses healthy databases) but does not and cannot notice that a
/// trigger present under the right name carries an older body. A database
/// stamped v5 by an earlier build therefore keeps whatever trigger text it was
/// created with until a rung drops and recreates it.
///
/// This is why the payload carries a version. Changing a log trigger's payload
/// splits the database population in two — files created after the change write
/// the new shape, files created before keep writing the old one — and the only
/// thing that makes that survivable is that every reader accepts both. A
/// payload change that did *not* bump `v` would be indistinguishable at read
/// time from corruption, which is the case `DbError::PayloadVersion` exists for.
///
/// The v1 → v2 concept payload (defect V) is deliberately left to ride along on
/// the next rung that has to move `user_version` anyway rather than claiming one
/// of its own: an old file loses `embedding_model` from its temporal reads, which
/// is exactly the behaviour it had before, and gains it the moment it is
/// migrated. Nothing regresses in the meantime.
/// `links_current` maintenance, one row per open belief **per lineage**.
///
/// A named `const` since v12 for [`CREATE_CONCEPTS_LOG_INSERT`]'s reason: the
/// rung has to re-issue this exact body, and a rung with its own copy is a copy
/// that drifts. The conflict target matches the table's primary key, which now
/// ends in `branch_id` — without that, a branch asserting an edge its parent
/// already holds would *overwrite* the parent's row instead of adding its own.
pub const CREATE_LINKS_CURRENT_SYNC: &str = r#"
    CREATE TRIGGER IF NOT EXISTS trg_links_current_sync
    AFTER INSERT ON links
    BEGIN
        INSERT INTO links_current
            (source_id, target_id, edge_type, valid_from, valid_to,
             weight, properties, recorded_at, branch_id)
        VALUES
            (NEW.source_id, NEW.target_id, NEW.edge_type, NEW.valid_from,
             NEW.valid_to, NEW.weight, NEW.properties, NEW.recorded_at,
             NEW.branch_id)
        ON CONFLICT(source_id, target_id, edge_type, valid_from, branch_id) DO UPDATE SET
            valid_to    = excluded.valid_to,
            weight      = excluded.weight,
            properties  = excluded.properties,
            recorded_at = excluded.recorded_at
        WHERE excluded.recorded_at > links_current.recorded_at;
    END;
"#;

/// One open interval per edge **per lineage** (§4.3, branch-scoped at v12).
///
/// The `branch_id` clause is row-level and deliberately not ancestry-aware. A
/// branch that inherits an open interval from its parent and asserts its own is
/// not violating this rule — it is superseding a belief, which is the thing a
/// branch is for.
///
/// # The question this comment parked, answered at 0.14.8 (D-225)
///
/// *Whether the inherited interval should also close.* It should not, and
/// cannot: closing the ancestor's row is the parent corruption Doctrine III
/// forbids, and `links` is append-only so no statement in the crate could do
/// it. What a branch writes instead is its **own** row at the ancestor's key,
/// which the read prefers by `dist` — shadow retirement.
///
/// The half a trigger genuinely cannot answer went to the Rust layer, where
/// the ancestry is reachable: `lineage::overlap_candidates_resolved` refuses an
/// assertion whose interval overlaps **what the writing lineage can see**,
/// which is the read's definition applied to the write. That is a guard against
/// callers going through the actor and not against raw SQL, which is the same
/// honest cost `reject_overlapping_interval` has carried since D-060 — a
/// trigger able to make it would need a recursive ancestry walk on every
/// insert, on the path D-059 exists to keep fast.
pub const CREATE_LINKS_SINGLE_OPEN: &str = concat!(
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_links_single_open
    BEFORE INSERT ON links
    WHEN NEW.valid_to = '9999-12-31T23:59:59.999999Z'
         AND EXISTS (
             SELECT 1 FROM links_current
             WHERE source_id  = NEW.source_id
               AND target_id  = NEW.target_id
               AND edge_type  = NEW.edge_type
               AND branch_id  = NEW.branch_id
               AND valid_from <> NEW.valid_from
               AND valid_to   = '9999-12-31T23:59:59.999999Z'
         )
    BEGIN
        SELECT RAISE(ABORT, '"#,
    abort_single_open!(),
    r#"');
    END;
    "#
);

/// The update half of the concepts log. See [`CREATE_CONCEPTS_LOG_INSERT`].
///
/// Unconditional where its insert sibling is marker-gated, and the asymmetry is
/// deliberate: nothing inside an archive session updates a concept, so gating
/// this would suppress nothing.
///
/// `branch_id` is in the column list since v12 and the omission would have been
/// expensive. `concepts` permits a **same-lineage** update — the guards refuse
/// cross-lineage inserts and `branch_id` changes, not this — so a branch
/// correcting a concept it minted would have logged the change against `'main'`,
/// putting a branch's own history in the trunk's fold and leaving the row
/// invisible to the abandonment sweep that §15.5's `archive` arm performs.
pub const CREATE_CONCEPTS_LOG_UPDATE: &str = r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_log_update
    AFTER UPDATE ON concepts
    BEGIN
        INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at, branch_id)
        VALUES ('concepts', NEW.id, 'U',
                json_object('v', 2, 'title', NEW.title, 'content', NEW.content,
                            'valid_from', NEW.valid_from, 'valid_to', NEW.valid_to,
                            'retired', NEW.retired,
                            'embedding_model', NEW.embedding_model),
                NEW.recorded_at, NEW.branch_id);
    END;
"#;

/// The links log, and the entry whose `entity_id` is composed rather than copied.
///
/// `source|target|type|valid_from` identifies an edge assertion and carries **no
/// lineage**, which is why `branch_id` had to become a column of its own rather
/// than a fifth field in that string. Re-keying `entity_id` was the other
/// option and was rejected: it changes what a log entry identifies, so rows
/// written before the rung would no longer match rows written after it, and the
/// fold would silently split one edge's history in two.
///
/// With the column present, the four folds in `temporal::replay` — a private
/// module, so the name is plain text rather than a link that would not resolve —
/// partition by `(table_name, entity_id, branch_id)` and two lineages'
/// assertions about one edge stay two beliefs. Without it they collapse to
/// whichever has the higher `seq_id` — no error, no drift report, just one
/// lineage's belief gone.
pub const CREATE_LINKS_LOG_INSERT: &str = r#"
    CREATE TRIGGER IF NOT EXISTS trg_links_log_insert
    AFTER INSERT ON links
    BEGIN
        INSERT INTO transaction_log (table_name, entity_id, operation, payload, recorded_at, branch_id)
        VALUES ('links',
                NEW.source_id || '|' || NEW.target_id || '|' || NEW.edge_type || '|' || NEW.valid_from,
                'I',
                json_object('v', 1, 'source_id', NEW.source_id, 'target_id', NEW.target_id,
                            'edge_type', NEW.edge_type, 'valid_from', NEW.valid_from,
                            'valid_to', NEW.valid_to, 'weight', NEW.weight,
                            'properties', json(NEW.properties)),
                NEW.recorded_at, NEW.branch_id);
    END;
"#;

pub const CREATE_TRIGGERS: &[&str] = &[
    CREATE_LINKS_CURRENT_SYNC,
    CREATE_LINKS_SINGLE_OPEN,
    concat!(
        r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_monotonic_ra
    BEFORE UPDATE ON concepts
    WHEN NEW.recorded_at <= OLD.recorded_at
    BEGIN
        SELECT RAISE(ABORT, '"#,
        abort_monotonic_ra!(),
        r#"');
    END;
    "#
    ),
    // Payload v2 adds `embedding_model` (defect V). Before it, the field was
    // written by nobody and read by two — `replay::fold_delta` and
    // `as_of::hydrate_attributes` both asked the payload for it and both always
    // saw null, so `AttributeMode::AtTime`, the faithful mode Doctrine VIII
    // exists to offer, returned a *less* complete record than `Current`.
    //
    // The version number moves because the shape is a compat surface: readers
    // must be able to tell "this build wrote no model" from "this payload
    // predates the field". v1 is still accepted and folds with the field absent,
    // which is what makes this safe without a migration rung — see the note on
    // [`CREATE_TRIGGERS`].
    CREATE_CONCEPTS_LOG_INSERT,
    CREATE_CONCEPTS_LOG_UPDATE,
    CREATE_LINKS_LOG_INSERT,
    CREATE_CONCEPTS_GUARD_DELETE,
    // v12 (§15.2, D-214). Order matters only in that every one of these names a
    // table the baseline has already created; `verify` recovers the names from
    // this array, so a trigger added here is a trigger the ladder must produce.
    CREATE_CONCEPTS_GUARD_LINEAGE,
    CREATE_CONCEPTS_GUARD_BRANCH,
    CREATE_BRANCHES_GUARD_UPDATE,
    CREATE_BRANCHES_GUARD_DELETE,
    // D-008 (revised): probe main.sqlite_master for the archive-session marker.
    // SQLite forbids a trigger in `main` from referencing objects in another
    // database, temp included, so the original temp.sqlite_master probe fails
    // at CREATE TRIGGER time and is unimplementable.
    concat!(
        r#"
    CREATE TRIGGER IF NOT EXISTS trg_links_guard_delete
    BEFORE DELETE ON links
    WHEN NOT EXISTS (
        SELECT 1 FROM sqlite_master
        WHERE type = 'table' AND name = 'macrame_archive_session'
    )
    BEGIN
        SELECT RAISE(ABORT, '"#,
        abort_delete_guard!(),
        r#"');
    END;
    "#
    ),
    concat!(
        r#"
    CREATE TRIGGER IF NOT EXISTS trg_txlog_guard_delete
    BEFORE DELETE ON transaction_log
    WHEN NOT EXISTS (
        SELECT 1 FROM sqlite_master
        WHERE type = 'table' AND name = 'macrame_archive_session'
    )
    BEGIN
        SELECT RAISE(ABORT, '"#,
        abort_delete_guard!(),
        r#"');
    END;
    "#
    ),
    // --- FTS sync (§5.9) ------------------------------------------------
    //
    // These write to `concepts_fts` and to nothing else. In particular they do
    // not touch `transaction_log`: an FTS index is derived from concept text
    // the ledger already records, so logging it would record the same fact
    // twice — the reasoning Doctrine VII applies to embeddings, and the reason
    // `doctrine_static_tests` scans this array.
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_fts_insert
    AFTER INSERT ON concepts
    BEGIN
        INSERT INTO concepts_fts (rowid, title, content)
        VALUES (NEW.rowid_pk, NEW.title, NEW.content);
    END;
    "#,
    // The retraction is not optional and not symmetric with the insert. An
    // external-content FTS5 index stores terms, not text, so replacing a row
    // means telling it which terms to *remove* — and it needs the old column
    // values to work that out. Omit this and the index keeps matching words the
    // concept no longer contains, with no error and no way to notice except by
    // searching for something that is no longer there.
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_fts_update
    AFTER UPDATE ON concepts
    BEGIN
        INSERT INTO concepts_fts (concepts_fts, rowid, title, content)
        VALUES ('delete', OLD.rowid_pk, OLD.title, OLD.content);
        INSERT INTO concepts_fts (rowid, title, content)
        VALUES (NEW.rowid_pk, NEW.title, NEW.content);
    END;
    "#,
    // The third trigger, installed **inert** by v8 (§4.6, D-119).
    //
    // Through v7 this array had no delete trigger, and the stated reason was
    // that `trg_concepts_guard_delete` is unconditional (D-022) so no delete
    // path exists to keep in sync. That was true and it was the wrong shape:
    // the index's correctness depended on a *different* trigger staying
    // unconditional, and nothing connected the two except a comment.
    //
    // It cannot fire today — the guard is a `BEFORE DELETE` that always aborts,
    // so the statement never reaches `AFTER DELETE`. It is here because 0.9.0's
    // archive session is what makes the guard conditional, and the moment that
    // lands the index would go silently stale without this. Installing the
    // capability in the rung that is already rebuilding the table costs nothing.
    //
    // It does **not** mean 0.9.0 needs no migration of its own — that claim was
    // written here and it is wrong (D-126, corrected 0.8.0 pre-tag). This trigger
    // is C2's step 3; step 2 is making `trg_concepts_guard_delete` conditional,
    // and that is a `v8 → v9` rung, because `CREATE TRIGGER IF NOT EXISTS` on an
    // existing name keeps the **old body** and `verify()` compares names only, so
    // a re-issued baseline would leave the unconditional guard in place and pass.
    // Deliberately not fixed here: the archive-session marker exists during
    // *links* archival too, so a conditional concepts guard shipped in 0.8.0
    // would leave concepts deletable during those sessions.
    //
    // `the_fts_delete_trigger_is_installed_and_inert` (wave1_regression_tests)
    // pins both halves rather than assuming either.
    r#"
    CREATE TRIGGER IF NOT EXISTS trg_concepts_fts_delete
    AFTER DELETE ON concepts
    BEGIN
        INSERT INTO concepts_fts (concepts_fts, rowid, title, content)
        VALUES ('delete', OLD.rowid_pk, OLD.title, OLD.content);
    END;
    "#,
];

#[cfg(test)]
mod tests {
    use crate::util::timestamp::{CANONICAL_TS_GLOB, OPEN_SENTINEL};

    /// The DDL's CHECK pattern and the Rust-side pattern must be the same
    /// pattern. If they drift, one layer accepts what the other rejects and the
    /// canonical-form invariant is enforced in name only.
    #[test]
    fn ddl_glob_matches_the_rust_canonical_pattern() {
        assert_eq!(format!("'{}'", ts_glob!()), CANONICAL_TS_GLOB);
    }

    /// Every DDL statement that declares a temporal default must use the
    /// canonical sentinel; a second-precision default would be rejected by the
    /// very CHECK sitting next to it.
    #[test]
    fn ddl_defaults_use_the_canonical_sentinel() {
        for ddl in [
            super::CREATE_CONCEPTS_TABLE,
            super::CREATE_LINKS_TABLE,
            super::CREATE_LINKS_CURRENT_TABLE,
            super::CREATE_TRANSACTION_LOG_TABLE,
        ] {
            assert!(
                !ddl.contains("9999-12-31T23:59:59Z"),
                "DDL still carries the pre-0.5.4 second-precision sentinel: {ddl}"
            );
        }
        for trigger in super::CREATE_TRIGGERS {
            assert!(
                !trigger.contains("9999-12-31T23:59:59Z"),
                "trigger still carries the pre-0.5.4 sentinel: {trigger}"
            );
        }
        assert!(super::CREATE_LINKS_TABLE.contains(OPEN_SENTINEL));
    }

    /// Every abort message the classifier matches on must actually appear in the
    /// DDL that emits it. `concat!` makes this true by construction today; the
    /// test is what keeps it true if someone re-inlines a literal.
    #[test]
    fn every_abort_message_appears_in_a_trigger() {
        for msg in [
            super::ABORT_SINGLE_OPEN,
            super::ABORT_MONOTONIC_RA,
            super::ABORT_DELETE_GUARD,
        ] {
            assert!(
                super::CREATE_TRIGGERS.iter().any(|t| t.contains(msg)),
                "no trigger emits {msg:?}, so its typed error is unreachable"
            );
        }
    }
}
