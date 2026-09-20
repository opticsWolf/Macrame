# Macrame Update Plan v0.18.0 — the flexible ledger

**Status:** planned · **Branch:** `dev/0.18.0` · **Date:** 2026-09-20
**Driving consumer:** OKFgraph rework — see
`macrame-flexible-ledger-proposal-v2.md` in that repo for the consumption story.

Per-usecase extensibility **without** user DDL: open the attribute layer and add
guarded sidecars, while the ledger core — time, assertions, materialization,
archive, branching — stays fixed and guarantee-bearing.

**0.18 ships P2, P3, P1.** P4 is 0.19 and cannot start earlier: after the
redesign recorded in [D-281](#d-281-draft), blob eligibility is computed from
references inside log payloads, which do not exist until
[D-278](#d-278-draft) lands.

| Item | Work | Release | Schema |
|---|---|---|---|
| P2 — edge-kind charset | ~2 h | 0.18 | none |
| P3 — KV sidecar | ~2 d | 0.18 | v19 → v20 |
| P1 — `concepts.extra` | 4 d | 0.18 | v20 → v21, payload v3, snapshot v5 |
| P4 — blob store | 4 d+ | 0.19 | v21 → v22 |

---

## Draft decision-register entries

**These are drafts.** §13 records decisions that have shipped, and each entry
carries the release that made it. Nothing below is pasted into
`architecture/s13-decision-register.md` until the work lands and the version in
parentheses is true. Numbers D-278 … D-282 are reserved here, with lettered
sub-entries D-278a, D-278b and D-282a; D-277 is the last allocated.

**On the lettering.** The register's only precedent is D-008a / D-008b — two
sub-decisions hung off D-008 in the release that revised it — so the *shape*
matches (a subordinate decision, same cycle, same parent) while the *count* does
not: three lettered entries in one cycle against two in the register's whole
history. Kept lettered anyway, because each genuinely subordinates — the upsert
arm and the index registration are decisions *about* `concepts.extra`, and the
per-shape ceiling is a decision *about* the version gates — and flattening them
to D-283 … D-285 would put peers in the numbering where the reasoning has a
parent. It is a one-line change if the register's owner disagrees, and the
disagreement is worth having before these land rather than after.

Links are written as they will resolve **from inside `s13-decision-register.md`**,
not from this file.

---

<a id="d-278-draft"></a>**D-278** — `concepts.extra`, and it is versioned (0.18.0, P1).
App-defined attributes get a JSON column on the concept row rather than a table
of their own, and rather than user DDL: `ALTER TABLE ADD COLUMN` is what
[D-036](s13-decision-register.md#d-036) permits on a ledger table, additive and
without a major version even after 1.0, and a column on the row inherits
lineage, retirement and the bitemporal key for free. **The column enters the log
payload, which is the whole cost of the item.** The proposal that produced it
assumed the opposite — that riding the concept row meant history covered it with
no trigger work — and that is false for the reason 0.5.6 Wave 1 already
established: the triggers emit an explicit `json_object`, not `SELECT *`, so a
column absent from that list is invisible to `reconstruct` and to every
`AttributeMode::AtTime` read. Wave 1 classified exactly this as a silent defect
when the omitted column was `embedding_model` and fixed it with payload v2; the
same omission with a different column is the same defect. **The *concepts*
payload version therefore becomes 3; the links marker is untouched at 1** — the
two shapes version independently, which is what
[D-282a](s13-decision-register.md#d-282a) makes the read side finally honour.
The rung **drops and recreates** both concept log
triggers rather than re-issuing the baseline —
[D-126](s13-decision-register.md#d-126) /
[D-129](s13-decision-register.md#d-129): `CREATE TRIGGER IF NOT EXISTS` on an
existing name keeps the old body, and `verify` body-checks only the delete
guards, so a re-issued baseline leaves a v2 trigger writing incomplete history
and passes verification in silence. Both decode sites (`temporal::replay`,
`temporal::as_of`) read the new field as `Option`, so v1 and v2 payloads keep
folding on the existing ceiling-not-match path. `NodeAttributes` gains the field
and the snapshot container goes to v5 — see
[D-282](s13-decision-register.md#d-282). The archive round-trip carries it
across all six explicit column lists plus `ColdConcept`, because
[D-130](s13-decision-register.md#d-130) requires a concept to move *column for
column*: a move that drops a column is a rewrite, and
[Doctrine V](s0-s3-foundations.md#doctrine-v) does not permit an absence the
ledger cannot explain. A `cold_has_extra` probe mirrors `cold_has_branch` so a
pre-0.18 cold file is tolerated on the read path and upgraded only by the
writer, inside its own transaction. Cap 64 KiB at the API boundary; bulk bytes
are [D-281](s13-decision-register.md#d-281)'s problem, not an attribute.
Rejected: *user `CREATE TABLE`* (that is Ladybug's job; bending the ledger there
unmakes this one); *an attribute-per-row table* (an EAV join on every attribute
read, and a second place lineage and retirement would have to be re-derived);
*sidecar files beside the database* (no transactional relationship to the row
they describe, and nothing carries them across a backup); *keeping `extra` out
of the payload to avoid the version bump* (the log would mint an entry for a
change it does not describe — Wave 1's defect, knowingly re-introduced).

<a id="d-278a-draft"></a>**D-278a** — The upsert arm preserves `extra` on omission (0.18.0, P1).
`ConceptUpsert::extra` is `Option<String>`: `None` leaves the column alone,
`Some(v)` replaces it wholesale. The question is the one the `branch_id` comment
beside `UPSERT_CONCEPT` already works through for a different column, and it
reaches the opposite answer here for a different reason. `branch_id` is
provenance and is left where it was minted; `extra` is *content* and must be
correctable, so it belongs in the `DO UPDATE SET` list — but assigning
`excluded.extra` like every other column means a caller who re-upserts a concept
without calling `.extra()` silently wipes the app's attributes to `{}`, and
because [D-278](s13-decision-register.md#d-278) made the column versioned, that
erasure is recorded as a deliberate belief change rather than a mistake.
Preserve-on-omission is implemented by referencing the bind parameter directly
in the update clause, inside the one shared statement, so the single and chunked
write paths still cannot drift ([D-056](s13-decision-register.md#d-056)). No gap
in the history: the log trigger reads the post-update row, so the payload
records the resulting value rather than the supplied one. **Clearing is
`Some("{}")`, and there is no state below it**: the column is `NOT NULL DEFAULT
'{}'`, so SQL NULL is unreachable by construction and the `Option` in the
builder means *unstated*, never *null*. Nothing has to distinguish an absent
field from a JSON `null`, because only one of the two can reach the column.
Rejected: *full
replace* (a silent wipe on a path callers reach by forgetting one builder call);
*`json_patch` merge* (keys merge automatically, but `extra` could then never be
replaced wholesale and deleting a key would mean asserting null — RFC 7396
semantics, a surprise the first time it is met).

<a id="d-278b-draft"></a>**D-278b** — App-registered expression indexes re-assert, and carry no registry (0.18.0, P1).
`register_extra_index(path)` validates the JSON path and issues a
create-if-absent; the app calls it unconditionally at startup. That is the whole
mechanism. A registry table was considered for one specific worry — that a
restored backup silently loses an index nothing records — and unconditional
re-assertion answers it without a table, because the next open rebuilds what the
file is missing. Nothing else needs changing: `verify` checks for the *absence
of what it requires* and was taught in 0.9.0 to tolerate objects the baseline
did not create, naming D-036's index permission as one of the three reasons it
stopped counting `sqlite_master`. Core ships the `layer` convention index.
**This is the schema's first expression index** — none exist today — so the
`EXPLAIN` assertion is an acceptance gate rather than a formality. Rejected: *a
registry table* (a new table, plus a question about whether it is lineage-scoped
and whether it archives, to buy inspectability nobody has asked for);
*built-in indexes only* (one fast key and a hand-rolled scan for every other,
which is the workaround the item exists to delete).

<a id="d-279-draft"></a>**D-279** — Edge kinds relax their charset, and gain two rules doing it (0.18.0, P2).
`edge_type` becomes `[A-Za-z0-9_:.\-]+` so applications can namespace their own
kinds — `okf:links-to`, `myapp:cites` — without a rename step, and core kinds
stay bare. No DDL and no migration: the check is at the API boundary, in one
function, which the Python binding reaches through `InvalidEdgeTypeError` rather
than duplicating. The `|`-delimited `entity_id` composed by
`trg_links_log_insert` is unaffected, since no character added is `|`; the
`[A-Z0-9]+` assertion in `MaterializedState::entity_id`'s doc comment is
rewritten to say what still holds. **Two rules arrive with the relaxation.**
*One case per kind*: a kind is all-upper or all-lower, and mixing is refused —
`edge_type` sits inside the primary keys of `links` and `links_current`, no
schema object uses a case-insensitive collation, and uppercase-only made the
collision impossible by construction. Without the rule `okf:cites` and
`okf:Cites` are two edge kinds that are one row apart and indistinguishable in
every log line, error message and diff, which is the silent-wrong-answer shape
this crate treats as its worst failure. **The rule narrows that class; it does
not close it.** `okf:cites` and `OKF:CITES` both pass and remain distinct rows
under BINARY collation, so what is bought is the *near-miss* — two spellings a
reader cannot tell apart — and not collision-impossibility, which only the
uppercase-only rule had and which the relaxation spends. Stated plainly because
the weaker guarantee is the one being shipped. *Maximum 64 characters*: there was no
length rule at all, the kind goes into a primary key and into the composed
history identifier, and a relaxed charset is exactly when kinds start getting
longer. **The cap cannot refuse data the file already holds, and the reason is
not that 64 is a generous number** — the old rule had no length bound, so a
database may legally hold a 100-character kind. It is that
`validate_edge_type`'s own doc records where it runs: *from
`EdgeAssertion::normalized` on the write path only*, and
`TraversalBuilder::edge_types` never called it. Nothing re-validates a stored
kind — not the fold, not `verify`, not the archive round-trip, not rehydration —
so an over-length kind already on disk keeps reading and traversing, and only a
new write of one is refused. **That property is load-bearing for this cap and
should be asserted**, because a future reader that starts validating on read
would turn it into a rule that refuses existing files.

**The validator's own test flips.** `edge_types_are_uppercase_alphanumeric`
lists `"knows"`, `"KNOWS_WELL"` and `"KNOWS-WELL"` among seven cases it asserts
are *rejected*; all three become legal, while `"A|B"`, `"O'BRIEN"`, `"ÉTAT"` and
the empty string stay refused. The test is rewritten as part of this item rather
than discovered failing, and the new cases — mixed case, over-length, and each
newly admitted character — go in beside the survivors.

Forward incompatibility, release-noted: a 0.18 database holding lowercase kinds
handed to a 0.17 binary fails validation. Rejected: *normalizing case at the boundary*
(collisions become impossible, but the kind comes back as something the caller
never wrote, and this crate rewrites no caller input anywhere else);
*case-sensitive and documented* (invents no policy and costs nothing, and leaves
a foot-gun whose only symptom is two rows where someone expected one);
*leaving the length unbounded* (true today and defensible today; the charset
change is what makes it stop being true).

<a id="d-280-draft"></a>**D-280** — `kv_store`: operational state, outside the ledger entirely (0.18.0, P3).
Hashes, epochs, counters and cursors are neither belief nor content. They need
durability and a backup story and have no history worth keeping, so they get a
plain `WITHOUT ROWID` key/value table with **no log trigger, no archive
membership and no `branch_id`** — [Doctrine VII](s0-s3-foundations.md#doctrine-vii)'s
reasoning about embeddings applied to a different derivative. **No `branch_id`
is a semantic, not an omission, and it is stated here rather than inferred:**
the store is branch-global. One cursor, one epoch, one counter across every
lineage, unchanged by working on a branch, because operational state describes
the *process* rather than anything the ledger believes. Nothing in the crate
trips over that — a branch is a row in `branches` plus a label carried on
writes, so there is no physical copy and no path that enumerates tables to
duplicate them — but an application storing per-branch state in `kv_store` is
storing it in the wrong place, and the key convention is where it would say so.
`updated_at` carries `canonical_ts_check!` as every table in this schema that
stamps a timestamp does — six uses, not the four D.1 item 3's wording names.
D.1 item 3 freezes
the canonical form as *a fact about the disk as well as the API*, and a column
the crate stamps as canonical should be one the file enforces — the alternative
is a raw writer putting anything there and nothing noticing.
`kv_scan(prefix, limit)` takes `limit` as a required argument rather than an
`Option`, on the same reasoning as every other bounded read here. **And the
backup story is stated correctly**, because the draft that proposed this table
stated it wrong: a snapshot is a zstd `bincode` `MaterializedState` in a
snapshots *directory* and contains no KV, so what carries KV is a **file-level
backup**, while transaction-time reconstruction ignores the table entirely. That
distinction is the whole exclusion argument. Rejected: *versioned KV* (re-creates
the ledger through the back door, for state whose previous value nobody wants);
*a hard per-app key partition* (one app's prefix is another app's query; the
convention is documented and `kv_scan` is where it is enforced if anywhere).

<a id="d-281-draft"></a>**D-281** — Blobs are reclaimed by the archive, not by a refcount (0.19.0, P4).
The content-addressed blob store ships with `blob_put` / `blob_get` /
`blob_stat` and **no `blob_link`, `blob_unlink` or `blob_gc`**. The proposal's
refcount design was withdrawn rather than adjusted, because
[D-278](s13-decision-register.md#d-278) makes it unsound: `extra` is versioned,
so a concept version that named blob `X` names it permanently, and a counter
that tracks only present references reaches zero while a past state still
depends on the bytes. The sweep then deletes them — no error, no counter, and
the symptom arrives months later as a missing asset in an old revision. That is
the absence [Doctrine V](s0-s3-foundations.md#doctrine-v) forbids, arriving as
routine housekeeping. **This system already has exactly one reclamation path**,
and Doctrine V permits no physical delete outside it, so the blob store uses it:
a blob is eligible when every log entry naming it has gone cold — *reachability,
not expiry*, which is [D-128](s13-decision-register.md#d-128)'s predicate shape
for concepts — and it then **moves to `cold.blobs`** rather than being deleted,
so archived states stay readable. Consequently `blobs` is **not** a
three-exclusion sidecar like [D-280](s13-decision-register.md#d-280)'s table: it
keeps no log trigger and no lineage, and it *is* an archive participant.
**Eligibility is cross-lineage, and that falls out rather than being chosen:**
every lineage writes into one `transaction_log`, so "every entry naming it has
gone cold" is already a question about the whole log, and a blob named only by a
side branch's entries stays hot until those entries go — which they do through
`archive_branch`, the named operation that moves one lineage's rows wholesale.
A blob therefore outlives the branch that introduced it exactly as long as that
branch's log entries do, and abandoning a branch reclaims its bytes only by
archiving it. Merge changes nothing: a merge writes new entries naming the same
content address, which is one more reference to a pool that was already shared. It is a
**rowid table with a `UNIQUE` index on `sha256`**, not `WITHOUT ROWID`, which
would store a multi-megabyte payload in the index b-tree as an overflow chain
hanging off the key structure. Single-blob cap is **8 MiB, overridable through
`Tuning`**: libsql 0.9.30 exposes blobs only as `Value::Blob(Vec<u8>)` with no
incremental API, so the streaming read the proposal deferred is unimplementable
at this driver version and every operation materializes the file whole at
roughly 2–3× its size in peak memory — 64 MiB would be ~190 MiB per operation in
a crate that boxed an error variant over 168 bytes
([D-075](s13-decision-register.md#d-075)). Blob writes are **exempt from
`CHUNK_BUDGET` by contract**, with their own warn-above-hold threshold on
`BULK_ATOMIC_WARN_HOLD`'s precedent: the `CHUNK_ROWS_*` constants bound
*duration*, a blob is one unsplittable row, and no row count expresses the
bound — so a `CHUNK_ROWS_BLOBS` would have been a measured number that could not
do its job. WAL pressure is the existing `Tuning::wal_autocheckpoint` policy
([D-157](s13-decision-register.md#d-157)), not a new knob. Rejected:
*refcount plus `blob_gc`* (unsound against a versioned `extra`, as above);
*reachability-checked GC* (correct, but a second reclamation path beside the
archive, needing an index over log payloads to avoid a full scan, and still
physically deleting outside a session); *append-only with no reclamation*
(safe and honest, and defers an unbounded file to whoever hits it first).

<a id="d-282-draft"></a>**D-282** — Two version gates move with the payload, and `verify` learns the third (0.18.0, P1).
[D-278](s13-decision-register.md#d-278)'s new field crosses three separately
versioned things, and each gets its number moved rather than being allowed to
fail quietly. **Concepts payload → v3**, refused by older builds on *their*
ceiling check — 0.17's global 2 refuses a v3 row whatever shape it describes —
so once 0.18 writes any concept, a 0.17 binary cannot fold the log at all, which
is the designed behaviour and a release-note line rather than a regression. It
carries a corollary worth printing beside it: **links-only writes from a 0.18
binary still fold under 0.17**, because the marker is per entry and the links
shape stays at 1. 0.18's own ceiling stops being global — see
[D-282a](s13-decision-register.md#d-282a). **`SNAP_FORMAT_VERSION` → v5**, on
[D-043](s13-decision-register.md#d-043)'s standing reason and
[D-221](s13-decision-register.md#d-221)'s precedent: `bincode` is not
self-describing, so a v4 file read by a v5 build does not fail — it parses into
the wrong values, and a snapshot is the first thing a restart reaches for. The
field carries `#[serde(default)]` as well, and **D-221's reason for pairing them
is worth restating because it is easy to read as something stronger than it
is**: the default cannot rescue a v4 file, since a serde default pads no bytes
into a short `bincode` buffer and the attribute is invisible to that decode.
The version constant does *all* of the protective work here. What the default
buys is that the **field** stays additive if the container is ever versioned
some other way — a hedge against a future change, not a decode path — which is
precisely how D-221 put it for `EdgeBelief::branch_id`. The two are not
redundant; they also do not both protect this release. **And `verify` gains a body probe** for the
version marker in `trg_concepts_log_insert` and `trg_concepts_log_update`,
alongside the archive-session probe it runs on the delete guards. The gap is
exact: `verify` reads bodies for the three guards and names only them, so a
concept log trigger with the right name and a v2 body is invisible to it — the
same shape [D-129](s13-decision-register.md#d-129) closed for the concepts
delete guard, on a trigger that check does not cover. A stale trigger now
refuses the open instead of writing incomplete history. The probe is the version
marker and not the trigger's whole text, for D-126's reason: a full-text
comparison fails on whitespace and becomes the kind of check people disable.
Rejected: *trusting the rung* (this precise failure has happened once already,
and body-checking exists because of it); *comparing whole trigger bodies*
(re-pinned every time a comment moves).

<a id="d-282a-draft"></a>**D-282a** — The payload ceiling is per shape, not global (0.18.0, P1).
The version marker has been per entry since the log's first release — links
write `'v', 1`, concepts `'v', 2` — but the read side checks one constant,
`PAYLOAD_VERSION`, against every row *before* dispatching on `table_name`, so
one number gates two shapes that version independently. That was sound while one
version existed and stopped being sound the day Wave 1 took concepts to v2 and
left links at 1: from then a links row stamped 2 passed the gate and decoded
under v1 field names, and no such row exists only because nothing but this crate
writes links versions. **0.18 does not open the gap; it widens it and makes it
policy.** Bumping the constant to 3 admits links rows stamped 3 as well, and the
part that outlives this release is the precedent: when links eventually
versions, every older binary holding a global ceiling folds the new rows instead
of refusing them, which is precisely the failure the ceiling exists to convert
into an error. The threat model is the one
[D-280](s13-decision-register.md#d-280) already concedes for
`canonical_ts_check!` — the raw writer §4.7 admits — and the log is at least as
writable as a timestamp column. The check therefore moves after the
`table_name` branch and reads per-shape constants, links 1 and concepts 3, so
each shape refuses above its own ceiling. **The cost is small and was measured
rather than assumed:** `PAYLOAD_VERSION` is `pub(crate)`, so no public path
moves and no bless is needed; `DbError::PayloadVersion` already carries `max`,
so the error reports the shape's real ceiling with the table named at the
construction site and the Python binding — which destructures the variant
field-for-field — needs no change; no test references the constant by name; and
the two checks are **duplicated verbatim** between `temporal::replay` and
`temporal::as_of` rather than shared, so the move consolidates a read-side drift
risk, which is [D-056](s13-decision-register.md#d-056)'s single-statement rule
applied to the decode side. `as_of`'s copy is already concepts-only in context —
its query carries `WHERE table_name = 'concepts'` — so only `replay`'s copy
needs the dispatch. **The tightening cannot refuse a row any version of this
crate minted**, because none ever minted a links payload above 1, so it fires
only on rows that were already being silently mis-folded — [D-279]'s cap
discipline reaching the opposite conclusion from the opposite fact. The same
constants feed [D-282](s13-decision-register.md#d-282)'s verify probe strings,
so the read gate and the body check share one source and cannot drift; probing
the links marker is free and a no-op today, and is included so the check is
complete rather than coincidentally sufficient. One literal is left alone
deliberately: the binding's `PayloadVersion` test fixture hard-codes `max: 2` to
raise an error, and it documents nothing about the real ceiling. Rejected:
*bump the global constant and record the gap* (true the day it is written,
unread the day it bites); *hold the two shapes in version lockstep* (versions
exist to move independently, and the promise is unfalsifiable until it is
broken); *bump the links marker to 3 as well* (claims a shape change that did
not happen, costs a third trigger drop/recreate, and ends the property that a
0.18 binary writing only links stays readable by 0.17).

---

## Acceptance gates

1. **Round-trip under archive.** A concept carrying `extra` archives,
   rehydrates, and compares byte-equal — the test D-130 implies and the six
   column lists make necessary.
2. **Cold tolerance.** A pre-0.18 cold file attaches, reads and folds with no
   `extra` column present, and is upgraded only by the archive writer.
3. **Mixed payload fold.** A log holding v1, v2 and v3 concept entries folds to
   the same state the entries describe, with `extra` present only where written.
4. **Snapshot refusal.** A v4 snapshot meeting a v5 build is refused and the
   fold runs from the log.
5. **Stale trigger refusal.** A database whose concept log trigger carries a v2
   body fails `verify` with a message naming the trigger.
6. **Expression index plan, with a negative control.** `EXPLAIN` shows
   `idx_concepts_extra_layer` chosen for a `json_extract(extra, '$.layer')`
   filter — **and** shows it *not* chosen for the `extra ->> '$.layer'` form,
   which is a different expression tree and cannot match the index. Without the
   second half the gate passes while the binding quietly emits the other
   spelling and falls back to the scan this item exists to delete. The exact
   expression form is pinned in the binding and in §4, not left to the caller.
   First expression index in this schema — treat a failure as likely, not
   surprising.
7. **Upsert preservation.** A concept upserted twice, with `extra` set only on
   the first call, still carries it — and the log says so.
8. **Ladder re-run.** §9's concept-write budgets re-measured after P1, since
   `extra` rides every concept write.
9. **Per-shape ceiling.** A links log row stamped v3 — hand-minted, since
   nothing writes one — is refused with an error naming links and a max of 1;
   a concepts row stamped v4 is refused naming concepts and 3. Gate 3 asserts
   only the positive, that mixed versions fold; this is its negative control,
   which is gate 6's lesson applied to the payload gate.

## Open

- The 64 KiB cap on `extra` is provisional. Check it against OKFgraph's real
  frontmatter sizes before it becomes a named constant.
- Whether `extra` should be reachable from `concepts_fts` is not raised here and
  is out of scope for 0.18.
