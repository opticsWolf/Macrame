# Macrame Update Plan v0.22.0 — Amendment 2 (PROPOSED, revision 2)

> **Status: PROPOSED, revision 2, 2026-11-07.** Revision 1 was written for a second review. This
> revision applies that review (§1). Nothing here is decided, nothing is committed, no code exists,
> and the register has not moved. If the review accepts it, the next cycle promotes it as **two**
> register entries, candidate **D-314** (the overlay) and candidate **D-315** (adoption across
> lineages), on a branch after 0.22.0's tail (§13 Q7). 0.22.0's remaining work is untouched by this
> file.
>
> This revision **assumes a way forward exists**. It designs the surface as if the probes in §8
> pass, and it says at each point which probe could still overturn that.
>
> **Review outcome (2026-11-07).** §13 Q3 is **accepted**: adoption is not §15.4's merge — the
> adopting lineage has no belief about the id, the minting lineage is never read, and the caller
> asserts content it takes from the merge commit's own tree — with one condition: **P-7 must run
> before any D-315 build**, since its verdict decides whether a batch must write the adoption
> before its edges. Q1, Q2 and Q5 proceed as recommended: the M6 cycle is branch
> `dev/0.22.0-revA`, D-314 in full, D-315 in the same cycle. There, P-1 (the rung sketch is
> additive; both concept guards still refuse direct cross-lineage writes after it exists) and
> P-2 (the census reproduces: 27 sites in 12 files — 15 content, 5 identity, 7 write machinery,
> 2 guard/fixture — with two *extends*: membership and the clock ceiling) are on record. The
> register entries still land with the cycle's docs phase (M6-f), and nothing here moves while
> 0.22.0's tail is unfinished.

## 0. What this amendment is, and is not

[Milestone M6](Macrame%20Update%20Plan%20v0.22.0.md) is marked **not in 0.22.0** with the note
"*retitle on a branch*". The plan's §7 open question 2 defers it: *"Reverses D-214's rule. Not in
0.22.0 … ask Silk whether retitling on a branch matters before planning it."*
[Amendment 1](Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%201.md) adds, in its §4 table: *"A
shadow row for a concept **is** M6, which needs its own decision."*

That framing stopped being adequate when CodeRadar's P5a.4 branch ingestion shipped. CodeRadar hit
the same wall from the other side and wrote the failure down:

> *"The entities live on the mint lineage permanently — until the D-214 overlay exists, the trunk
> can never mint those ids. Operational consequence: ingest branches before merging them, and
> expect branch-born files to read on their lineage."*
> — CodeRadar `docs/temporal-p5-commit-ingestion.md:361-364` (`68e7428`, v0.14.0, branch `dev`)

This amendment is the planning document for the overlay that D-214 deferred. It starts from the
design D-214 already recorded (§2), states two decisions rather than one, and leaves both to the
review.

**What it is not:**
- a release plan, a commit or a register entry;
- an argument that M6 belongs in 0.22.0 (it does not);
- a rewrite of D-214's or D-260's entries. The register is append-only, so any "Amended by" line
  lands only after the review.

## 1. What the review changed (revision 1 → revision 2)

| # | Review finding | Disposition in this revision |
|---|---|---|
| F1 | Revision 1 ignored the overlay already designed in `Macrame Road to 1.0.md` §15.2 (`concept_versions`, five costs, two reopen symptoms). Its `UNIQUE(id, branch_id)` on `concepts` is the option D-214 lists as *"measured — every insert fails"*. | **Applied.** The design now starts from §15.2's `concept_versions` (§5). `concepts` keeps one row per id, so the foreign keys from `links`, the embedding tables and `analytics_annotations` are untouched. Probe P-1a (re-run the identity probe) is dropped: D-214 already measured it. |
| F2 | The "cheapest before 1.0" deadline (old §4.1, Q5) misread `ddl.rs:391-400`, which is about v8's `rowid_pk`. §15.2 says `concept_versions` is additive under D-036 at any version. | **Applied.** No schema deadline is claimed. What does grow is the reader count (cost 1); §13 Q5 is reframed around that. |
| F3 | R2 (the post-merge trunk minting branch-born ids) is the operation §15.4 and §16 refuse as merge. It is CodeRadar's U-4. With R1 and R2 together, the narrowed `CrossLineageError` of old Q4 could never fire. | **Applied, with an argument.** R2 becomes its own decision, **D-315 adoption**. §6 argues why it is not §15.4's merge, and the review must accept or reject that argument by name. Old Q4 is re-derived (§13 Q4). |
| F4 | Three of §15.2's five costs were missing: search, archive, and the resolution rule. Search was the very silent-wrong-answer risk revision 1 ranked most serious. | **Applied.** Each of the five costs has a row in §5.4, and a sixth was found while doing it: the per-model embedding tables, which are keyed by concept id (`ddl.rs:795`). |
| F5 | R1 reintroduced implicit supersede: a plain upsert on a branch would silently cut an inherited open belief, where D-312 requires `supersede=True` on one lineage. | **Applied.** Cutting an inherited open belief requires `supersede=True`; a plain upsert stays `ConceptSingleOpen` (§5.2). The backwards case is defined and refused. |
| F6 | Citation errors. | **Applied:** the Amendment 1 link; the headline quote's location; *"inherited"* not *"merged"*; no invented S1 provenance; §-numbers; the §11 placeholder test name; Q3's D-124 (the R15 retraction, not a guard precedent). |
| F7 | Missing context: CodeRadar's U-5 is D-312 (now built), U-4 is merge, and §15.2 names a second consumer (the conversation-tree harness). | **Applied** (§4). |
| F8 | The 6.5–8.5 d estimate excluded search, embeddings, archive and the cold readers, and counted two decisions as one. | **Re-estimated** (§11). |

## 2. Where this starts: what is already decided

- **Option A** (D-214, `Road` §15.2) has one row per concept, carrying the lineage that minted it.
  `branch_id` on `concepts` is *provenance, never identity*. D-214 measured the two obvious
  widenings and rejected both:
  - `UNIQUE(id, branch_id)`: every insert fails, because `links.source_id` and `links.target_id`
    are single-column foreign keys and need a unique parent;
  - a composite foreign key: it forbids the copy-on-write that makes a fork O(1).
- **The overlay** (`Road` §15.2, line 3015) is a separate `concept_versions (id, branch_id, …)`
  resolved along ancestry. It buys exactly one thing: *correcting or retiring a concept the branch
  did not mint*. Its costs, in §15.2's own numbering:
  1. readers of `concepts` become readers of a resolution;
  2. every writer must tell the mint lineage from the effective lineage;
  3. search finds neither the branch's text nor its absence;
  4. a resolution rule that stays monotonic under later writes to an ancestor;
  5. archive's predicate and the cold readers gain a lineage arm.
- **The overlay's reopen triggers.** Two symptoms, quoted from §15.2:
  - a caller is told "fork the concept instead" more than occasionally;
  - the retirement symptom: a branch cannot retire an inherited concept *at its own cost*.

  Both have now fired. CodeRadar's inherited-file divergence (C6) is the first. Its merge hollow,
  read from the trunk's side, is the second's converse.
- **Merge is refused** (`Road` §15.4, §16): *"Merging two belief lineages requires deciding which
  assertion supersedes which on the caller's behalf, and Doctrine III has no neutral answer to
  that. It is its own decision."*
- **The guard comment** on `CREATE_CONCEPTS_GUARD_LINEAGE` (`src/schema/ddl.rs:577-580`):

  > *"A branch that may restate its parent's concepts is the overlay design, and the overlay is
  > deferred with its reopen trigger named (D-214) — a guard that quietly permitted the ancestry
  > case would ship half of it with none of the machinery that makes it correct."*

  This revision does not relax that guard (§5.1).

## 3. Three different proposals hide inside the word "M6"

The gaps doc's M1b line (*"the **inherited** open version is closed by a shadow row … without
reversing D-214"*, quoted in Amendment 1's §4 table) can be read three ways.

| Shape | Mechanism | As-of honesty | Verdict |
|---|---|---|---|
| **S1**: read-time override | A read prefers the branch's row wherever one exists. No ledger event, no stamp, no cut. | **Broken.** As-of reads before the edit already show the new text; no recorded-time provenance; the change feed and replay cannot see it. | **Refused.** What a read returns must never get ahead of what the ledger was told. |
| **S2**: lineage versions (the overlay) | A lineage holds its own versions of an id, at its own stamps, in `concept_versions`. Nearer versions win per valid instant (§5.3). The mint row is never mutated. | Honest. The branch shows the inherited text in `[inherited_start, cut)` and its own from `cut` on. | **Proposed as D-314** (§5). |
| **S3**: cross-lineage relations | Edges on branches behind the same relaxation | n/a | **Out of scope**, deferred under D-225. CodeRadar agrees by construction (`ingest.rs:838-840`). Named so the halves are never re-merged silently. |

The gaps doc's "shadow row" is defensible only as S2. D-314 delivers it: a branch supersede of an
inherited concept is a cut plus a version under one stamp (§5.2).

## 4. The consumer case

### 4.1 CodeRadar

Read off the CodeRadar tree (`D:\User\Documents\Python\CodeRadar`, `dev`, `68e7428`). C2–C4 were
re-checked in this revision; the other anchors are carried over from revision 1.

| # | What was found | Anchor |
|---|---|---|
| C1 | Lineage-per-git-branch ingestion is shipped: `fork_branch(branch, parent)` at the merge-base, recorded once and mapped in `kv`; cursors per lineage | `docs/temporal-p5-commit-ingestion.md` §P5a.4 |
| C2 | Three-way file resolution on a branch. Branch-minted files mint entity concepts on the lineage. Inherited files diverge through `write_branch_content_at`, *"bytes only — the entities stay trunk rows the branch reads through fallback"*. Ids a sibling holds get bytes, concepts skipped and reported. | `core_indexer/src/graph/ingest.rs:835-870` |
| C3 | The **merge hollow**: a trunk ingest meeting a branch-born id skips its concepts and reports `cross-lineage-id:{minted}` | `ingest.rs:470-500`, `:562`, `:1217` |
| C4 | The workaround, in prose: *"ingest branches before merging them"* | `temporal-p5-commit-ingestion.md:355-364` |
| C5 | Sweep and retire already round-trip the mint lineage, so stale sweep discipline cannot block a future overlay | §P5a.4 as-built, final bullet |
| C6 | **Inherited-file divergence carries no structure.** A branch commit changing a trunk-born file moves the bytes, but the branch's symbol graph still shows the trunk's concepts. | C2's first arm; `py_agent/src/coderadar/ops.py:1181`ff. |
| C7 | The consumer's read surface already speaks lineages (`ops.branches()`, `as_of(…, branch=)`, per-lineage `verify_history`), so it needs no read-API change | `ops.py:1161-1180`, §P5a.5 |
| C8 | Relations and annotations are decided: edges across lineages are deferred; annotations are payload references and never inherit edge machinery | `docs/annotations-comments-proposal.md:61-78`; `ingest.rs:838-840` |
| C9 | Volumes: this repo's history is 354 commits / 1,696 files in 72 s (~0.2 s per commit) | §P5a.3 |

**CodeRadar's upstream requests, mapped** (`docs/temporal-p5b-and-beyond.md:313-326`):

| Request | What it asks | Where it lands |
|---|---|---|
| U-3 | fork with a valid-axis cut (the merge-base commit time) | **Not this amendment.** D-307's `at=` is a recorded-axis cut. The valid-axis cut is a separate question for the next cycle. |
| U-4 | *"let a lineage assert concepts first minted on a lineage it merges from, under the same id, with provenance recorded"* | **D-315 adoption** (§6). CodeRadar notes that it *"needs a Macrame decision before any design (Q-15)"* (`:46`). |
| U-5 | supersede in one call | **Discharged** by D-312 (M1b) on `dev/0.22.0`. CodeRadar should be told; its F2 two-flush window closes when 0.22.0 ships. |

**The two gaps M6 closes:**

1. **The post-merge trunk is hollow** (C3/C4, CodeRadar F4). After a merge, the trunk reads the
   merged files' bytes but no symbol concepts. Every default read, `as_of(branch="main")`, sees
   hollow structure for exactly the files the merge was about. CodeRadar records that this is the
   common case, not an edge case: under its D-6 c, every branch with commits of its own is
   ingested before it merges. Closed by D-315.
2. **An inherited file has no branch structure** (C6). At commit X on branch Y, a file Y changed
   shows Y's bytes but the trunk's concepts. Closed by D-314.

**What CodeRadar does not ask for:** retroactive history, read-time override, backwards edits,
per-lineage blobs (its INV-4/F-5 decided against those), or edges across lineages (C8).

### 4.2 Silk, and the conversation harness

`Road` §15.2 names the overlay's one capability Option A lacks: *it is the only design in which a
conversation-tree harness can revise a turn on an alternative continuation*. That is Silk's case:
retitling a task or revising a decision on a branch. Silk's alternative still holds as application
practice: keep task text immutable and move edits onto pointers and new concepts. With CodeRadar's
evidence on the table, though, the overlay no longer rests on retitle ergonomics alone.

## 5. D-314: the overlay, defined

### 5.1 Storage

```text
concept_versions (
    id            TEXT NOT NULL,      -- no FK: the id's mint row may be on a lineage this row cannot see (D-315)
    branch_id     TEXT NOT NULL REFERENCES branches(branch_id),
    valid_from    TEXT NOT NULL,
    valid_to      TEXT NOT NULL,      -- OPEN sentinel as concepts
    kind          TEXT NOT NULL CHECK (kind IN ('version', 'cut')),
    title, content, extra, embedding_model,   -- NULL iff kind = 'cut'
    recorded_at   TEXT NOT NULL,
    PRIMARY KEY (id, branch_id, valid_from)
) WITHOUT ROWID
```

- **`concepts` is unchanged**, one row per id: the mint, with its provenance. Its foreign-key
  children (`links.source_id` and `links.target_id`, the per-model embedding tables at
  `ddl.rs:795`, `analytics_annotations` at `ddl.rs:1139`) keep a unique parent. Nothing D-214
  measured is re-opened.
- **`trg_concepts_cross_lineage` is not relaxed.** A write of an id this lineage did not mint never
  reaches `concepts`; the write actor routes it to `concept_versions`. So the guard comment's
  condition holds literally: no guard quietly permits the ancestry case. The machinery, not the
  guard, decides.
- **The log is already shaped for it.** `transaction_log` rows carry the writer's `branch_id`, and
  the replay fold partitions by `(table_name, entity_id, branch_id)`
  (`src/temporal/replay.rs:569-619`). A version is logged as a `concept_versions` row, under the
  writer's lineage and stamp.
- **A `'cut'` row** is the concept analogue of D-225's shadow retirement for edges. From
  `valid_from` on, it hides every farther lineage's belief for this id and shows nothing. It answers
  the retirement symptom: a branch can say "on my lineage this is no longer believed" at its own
  cost.
- **The schema rung** is v25 → v26 and additive: one table, its triggers, its index. No foreign-key
  suspension (D-117) and no trigger replacement. That answers old Q3, which this revision drops.

### 5.2 Write rules

**W1, an owned id.** If the writer's lineage minted the id, the write is exactly today's:
`concepts`, D-291, D-303, D-312.

**W2, an inherited id.** The id is minted on an ancestor and visible at the fork cutoff.

- **`supersede=True`** (D-312's contract, generalized). If the effective belief the writer sees
  (§5.3) is open and starts before `valid_from`, write in one transaction under one stamp:
  - a `'cut'` row at `valid_from`, then
  - a `'version'` row at `valid_from`.

  In practice the two are one row, since a version starting at `t` already hides the farther belief
  from `t` on. The cut is written separately only for a bare retirement.
- **No supersede, and the effective belief is open** → `ConceptSingleOpen`, as on one lineage. A
  lineage never gains an implicit supersede that its trunk does not have.
- **No supersede, and the effective belief is closed or absent at `valid_from`** → a `'version'`
  row, the same as an ordinary later upsert on one lineage.
- **Backwards** (`valid_from` before the effective open belief's start) → `ConceptSingleOpen`.
  Supersede only moves forward, as D-312 decided.
- **`retire_concept(id, at, branch=)`**, new: writes a bare `'cut'` row. On an owned id it is the
  existing close.
- **After the first version**, later writes of that id on this lineage are W2 against the lineage's
  own versions. D-291's single-open interval and D-303's state rule apply within
  `concept_versions` per `(id, branch_id)`, with D-303's skipped-backfill machinery reused rather
  than copied.

**W3, an id not visible on this lineage.** The mint is on a sibling, or on a lineage that is not an
ancestor. This stays `CrossLineageError` under D-314 alone. D-315 is the decision that widens it
(§6).

### 5.3 The resolution rule (§15.2 cost 4, stated before anything is built)

For a reader on lineage `L` at recorded cutoff `r` and valid instant `v`:

1. **Path.** `path(L) = L, parent(L), …, main`. Each ancestor `P` contributes only the rows
   recorded at or before the fork cutoff on that path. This is today's rule (D-223), unchanged.
2. **Per lineage.** Each lineage on the path resolves its own belief for the id at `v` exactly as
   today, using D-303's rule within the lineage. The mint lineage resolves from `concepts` and the
   log; every other lineage resolves from `concept_versions` and the log.
3. **Nearest wins, per valid instant.** The belief at `v` comes from the nearest lineage on
   `path(L)` that has a row covering `v`, a `'version'` or a `'cut'`. A `'cut'` resolves to
   *absent*.

**Monotonic.** A later write to an ancestor is either after the fork cutoff, and so invisible to
`L`, or a backfill before it, which is resolved inside that ancestor by D-303 and still loses to
`L`'s own rows wherever `L` has them. No reading on `L` changes because a farther lineage wrote,
except where `L` has no row of its own. That is what "nearest version on the path to the root"
requires.

**Single-lineage databases.** With `concept_versions` empty, which includes every database that
never forked, every reader runs today's statement. This is gate 9's byte-identical condition, the
same switch D-220 uses (`branches` has more than one row **and** `concept_versions` is non-empty).

### 5.4 The costs, one row each

| §15.2 cost | What D-314 does | Probe |
|---|---|---|
| 1. Readers of `concepts` | Today there are 27 `FROM`/`JOIN concepts` sites in 12 files under `src/`. Each is classified: identity-only (the mint row answers it, no change), content (resolution needed), or guard/DDL (none). Only content readers change, behind the single-lineage switch. | P-2 |
| 2. Mint vs effective lineage on writes | W1/W2/W3 (§5.2). The decision is made once, in the write actor, before the lock, where D-225's `lineage_shape` already runs. | P-2 |
| 3. Search | A new `concept_versions_fts` (external content, keyed by the version's `(id, branch_id, valid_from)` through a rowid-bearing shadow). A branch search unions the trunk FTS and the version FTS, then drops every hit whose id resolves (§5.3) to a different version or to absent. **Red test:** a branch search for the trunk's old text does not return the concept. | P-3 |
| 3b. Embeddings (found in this revision) | `{model}_embeddings` is keyed `concept_id PRIMARY KEY`, so a branch version's text would be searched with the trunk's vector. Versions get `{model}_version_embeddings (id, branch_id, valid_from, embedding)`. A branch vector search applies the same drop-if-superseded filter. Doctrine VII is unchanged: an embedding is derived from content, and a version is content. | P-3 |
| 4. Resolution rule | §5.3, with a property test (P-8). | P-8 |
| 5. Archive and the cold readers | An id is archivable only if no version on any live lineage is reachable. Each cold reader in §15.2's table gains its lineage arm: `rehydrate` returns the mint plus its versions; the cold replay fold applies §5.3, using the `branches` rows archive keeps; `audit` distinguishes an unreachable version from an absent one; `archive_hint` answers per lineage. | P-4 |
| — Change feed and diff | A version is a log row with its writer's `branch_id` (D-311's filter needs no change). `diff(a, b)` compares resolved beliefs, so a branch version shows as a concept change on that branch. | P-5 |
| — Verification | `verify_concept_intervals` runs per `(id, branch_id)` across `concepts` ∪ `concept_versions`, plus a resolution check: no lineage resolves two beliefs at one instant. | P-8 |

### 5.5 Invariants and red lines

**Invariants** (each becomes a red test, §12):

- **I1.** Rows on lineage A are never written by a write on lineage B, in `concepts`,
  `concept_versions` or the log. `trg_concepts_branch_immutable` stays.
- **I2.** `concepts` holds one row per id, and every foreign key into it is unchanged.
- **I3.** D-225's overlap rule, *"what a lineage may not overlap is what that lineage can see"*,
  holds for concepts under §5.3's resolution.
- **I4.** D-312's contract holds on every lineage: supersede is explicit, forward-only, one stamp.
- **I5.** D-308's batch, D-309's pointers, D-311's feed and D-313's `expect=` keep their shapes. Only
  the set of ids a lineage may write widens.

**Red lines** (each is a refusal, not a gap):

- No S1: no read shows a lineage's text at an instant before the version that carries it.
- No mutation across lineages.
- No per-lineage blobs and no per-lineage key-value data.
- No relation machinery. Edges stay D-225's. M6 must not turn into the edges overlay.

## 6. D-315: adoption across lineages (CodeRadar U-4)

**The rule.** A lineage may write a version of an id whose mint it cannot see (W3), as an ordinary
first version in `concept_versions` on its own lineage, at its own stamps. Nothing is copied from
the minting lineage. Nothing on any lineage is closed, superseded or cut. The id's provenance (its
`concepts` row) still names the minting lineage. Reads on the adopting lineage resolve by §5.3;
before the adopting stamp they find nothing, so pre-merge as-of reads on the trunk stay empty.

**Why this is not §15.4's merge, the argument the review must accept or reject by name.** §15.4
refuses merge because the engine would have to *choose a superseder on the caller's behalf*
between two lineages' assertions. Adoption chooses nothing:

- the adopting lineage had **no belief** about the id (W3 is exactly "not visible"), so nothing is
  superseded;
- the content is **asserted by the caller**, here from the merge commit's own tree, not resolved
  from the other lineage's rows;
- the other lineage is **not read** by the write.

What adoption does share with merge is the id. It relies on the principle D-214
codes into `concepts`, worded in §15.4 as *"concepts are shared vocabulary across the whole
ledger"*: two lineages that cannot see each other may each believe something about one id. If the review holds that reusing an id *is* a merge, D-315 is refused, U-4
stays blocked, and CodeRadar keeps the `cross-lineage-id` skip. D-314 stands either way.

**Edges to adopted ids.** An edge carries one `branch_id`, and `links` references `concepts(id)`,
which already exists for an adopted id. A trunk edge to an adopted id is therefore legal at the
storage layer today. Whether any write-path check refuses an edge whose endpoint has no belief
visible on its lineage is unverified (P-7). If one does, adoption must come before the edges in the
same batch (D-308 orders concepts first).

**`CrossLineageError` after D-315.** With W1, W2 and adoption, every (lineage, id) concept write is
routable, so the public API cannot raise it. The trigger keeps raising it for a write that reaches
`concepts` directly, such as `raw()` or an outside writer (D-074's class). See §13 Q4.

## 7. What Macrame would change (inventory for costing)

1. **Schema** (`src/schema/ddl.rs`, `migrations.rs`): the v26 rung (`concept_versions`, its FTS,
   per-model version-embedding tables created alongside the model's table, the immutability and
   single-open triggers per `(id, branch_id)`); the v11 and v24 fixtures; the quickref ladder.
2. **Write actor** (`src/connection.rs`): W1/W2/W3 routing in front of `upsert_concept`,
   `write_concepts`, `write_batch`'s concept phase and supersede; `retire_concept(…, branch=)`; the
   guard and D-303's backfill machinery parametrized on the target table rather than copied.
3. **Readers** (`src/graph/*`, `src/temporal/*`, `src/vector/*`): the content readers from P-2's
   classification behind the single-lineage switch, including `concept_lineage.rs` (D-310's held
   vs named becomes held, named or versioned), `subgraph.rs`, `vector_filter.rs`, `as_of.rs`, the
   replay fold, and hybrid search.
4. **Archive** (`src/temporal/archive.rs`): the predicate's second arm and the four cold readers.
5. **Verification**: `verify_concept_intervals`, `final_concept_intervals`,
   `reject_concept_overlaps_within`, all scoped per `(id, branch_id)`.
6. **Surface**: `supersede` and `retire_concept` on `BranchView`; `ConceptVersion` gains its
   resolving lineage (a field, so additive). Bless `public-api.txt`, the Appendix D count, Python
   stubs, parity tables and exceptions.
7. **Docs**: register entries D-314 and D-315; "Amended by" lines on D-214, D-225, D-260, D-303 and
   D-310; `Road` §15.2 (the overlay taken) and §15.4 (adoption distinguished from merge); s4 and s5
   sections; release notes.

## 8. Probes (decisions by measurement)

Each probe is small, gates the phase after it, and is written red-first.

| # | Probe | Settles | Could overturn |
|---|---|---|---|
| P-1 | Build the v26 rung on a populated v25 file; prove that `concepts`' foreign-key children, the FTS and the embedding tables are byte-identical, and that the cross-lineage trigger still fires for direct writes | §5.1 is additive | Only a libSQL surprise |
| P-2 | Classify all 27 `concepts` reader sites (identity / content / guard) and the write paths | cost 1 and 2 size; §11's estimate | the estimate, not the design |
| P-3 | FTS and vector filter: plan and time a branch search with 1,000 versions over 100,000 concepts; the stale-hit red test | cost 3 and 3b; whether a version-aware filter is cheap enough | the FTS shape: fall back to a per-lineage FTS rebuild if the filter scans |
| P-4 | Archive with versions on two live lineages; rehydrate and the cold fold | cost 5 | archive could require `concept_versions` to archive with its mint |
| P-5 | Change feed and diff, with one id versioned on two lineages | no noise rows; the direction of a diff | — |
| P-6 | Python: `CrossLineageError` consumers (M1b's 6 tests, the branch upsert paths) under W2/W3 | which tests change | — |
| P-7 | An edge on lineage L to an id with no belief visible on L | whether adoption needs batch ordering | D-315's edge story |
| P-8 | Property test: random fork trees, random versions, cuts and ancestor backfills; §5.3 by brute force vs the SQL resolution; monotonic under later ancestor writes | cost 4; I3 | the resolution rule itself |
| P-9 | CodeRadar pricing: count `cross-lineage-id` skips on its own store; time the adoption pass at the merge commits against the 72 s / 354-commit baseline | D-315's write volume | — |

## 9. Risks

| # | Risk | Seriousness | Mitigation |
|---|---|---|---|
| K1 | **Search returns the trunk's text on a branch** (cost 3), a silent wrong answer | High | The stale-hit red test (§12, 7) gates D-314; no version write ships before the FTS filter |
| K2 | **A reader missed in P-2** keeps reading `concepts` and smooths over a branch version (S1 by accident) | High | P-2 classifies every site; a test asserts the count of content readers, so a new reader fails it until classified (the D-224 pattern) |
| K3 | **The resolution rule is wrong** at some fork-tree shape | High | P-8's brute-force property test, before any reader is converted |
| K4 | **Adoption is merge after all** | Decision risk | §6 names the argument; D-315 is separable, and D-314 stands without it |
| K5 | M6 is taken as licence for the edges overlay | Medium | S3 stays deferred; CodeRadar enforces the split (C8) |
| K6 | **Reader count grows before the overlay lands** (§15.2's "blast radius") | Medium | Take the cycle soon after 0.22.0; until then, any new concept reader names its overlay arm in its rustdoc |
| K7 | **The FTS and embedding storage roughly doubles** for heavily versioned stores | Low–medium | P-3 measures it; versions are a small fraction (C9) |
| K8 | **Existing CodeRadar stores stay hollow** until re-ingested at their merge commits | Medium, operator-visible | P-9 prices it; CodeRadar's migration op (P5a.5) carries the step |
| K9 | Surface churn | Low | Standing gates: `api_growth`, `public-api.txt`, `binding_parity`, `doc_sync` |

## 10. What CodeRadar changes, if the review says go

1. **Adoption at merges** (D-315, U-4): the `cross-lineage-id` skip becomes an adoption path,
   writing entity concepts for branch-born ids at the merge commit's stamps, one flush per commit.
   The skip reason remains for ids that arrive with no branch mapping (a rebase whose lineage was
   never ingested).
2. **Branch structure for inherited files** (D-314): `write_branch_content_at` gains an entity arm
   that writes versions with `supersede=True` for inherited files the branch commit changes, and
   cuts for deleted ones (replacing tombstones). This replaces "bytes only".
3. **U-5**: take D-312 when 0.22.0 ships. F2's two-flush window closes.
4. **Migration op** (P5a.5): a post-merge re-ingest step for existing stores; `verify_history
   --branch` keeps its semantics.
5. **Docs**: the three "until the D-214 overlay exists" sites become "since schema v26"; F4 and U-4
   are marked delivered or refused according to D-315.
6. **Version pairing**: the adoption and version paths require schema v26 or later. On older stores
   CodeRadar keeps today's skip.

## 11. Effort (review input, not commitments)

| Phase | Content | Estimate |
|---|---|---|
| M6-P | P-1, P-2, P-8 (rung, reader classification, resolution property test) | 1.5 d |
| M6-a | D-314 write side: v26 rung, W1/W2 routing, supersede and `retire_concept` on a branch, guard and D-303 machinery parametrized | 2–2.5 d |
| M6-b | Content readers behind the switch: fold, `as_of`, traversal, `concept_lineage`, subgraph | 2–3 d |
| M6-c | Search and embeddings (P-3), with the stale-hit gate | 1.5–2 d |
| M6-d | Archive and the cold readers (P-4), verification, feed and diff (P-5) | 1.5–2 d |
| M6-e | D-315 adoption (W3), P-7 | 0.5–1 d |
| M6-f | Docs, surface, Python parity, register D-314/D-315 and the amended-by lines | 1–1.5 d |
| M6-g | CodeRadar side (§10), end to end, P-9 pricing | 2–2.5 d |

**Total ≈ 12–16 d.** About half of that is resolution proofs, search and archive, the costs
revision 1 left out. M6-e depends only on M6-a and can be cut if the review refuses D-315.

## 12. Verification stories (red-first names)

1. `a_branch_supersede_of_an_inherited_concept_shows_the_old_text_before_the_cut_and_its_own_after`
   (per reader: `reconstruct`, `reconstruct_as_of`, `concepts_by_id`, traversal `CURRENT` and
   `AT_TIME`)
2. `a_branch_supersede_of_an_inherited_concept_leaves_the_mint_lineage_byte_identical`
3. `a_plain_upsert_over_an_inherited_open_belief_is_refused_as_single_open`
4. `a_backwards_branch_supersede_is_refused`
5. `a_branch_retires_an_inherited_concept_at_its_own_cost` (the §15.2 retirement symptom, closed)
6. `a_later_ancestor_backfill_does_not_change_a_branch_reading_the_branch_versioned`
   (monotonicity)
7. `a_branch_search_for_the_trunks_superseded_text_does_not_return_the_concept`
   (search; the vector twin follows the same pattern)
8. `an_archive_keeps_a_concept_whose_branch_version_is_still_reachable`
9. `the_interval_ledger_verifies_clean_with_one_id_versioned_on_two_lineages`
10. `a_direct_write_to_concepts_across_lineages_is_still_refused_by_the_trigger`
11. `a_lineage_adopts_an_id_it_cannot_see_without_reading_or_touching_the_minting_lineage`
    (D-315)
12. `a_trunk_as_of_before_the_adoption_stamp_finds_nothing` (D-315)
13. `a_never_forked_database_runs_every_concept_reader_unchanged` (gate 9, plan pins
    byte-identical)
14. CodeRadar: after a merge story, a v26 store reports zero `cross-lineage-id`, and trunk symbol
    coverage includes the merged files.

## 13. Questions for the review (each with a recommendation)

| # | Question | Recommendation |
|---|---|---|
| Q1 | Redefine M6 as "the D-214 overlay (S2) as `Road` §15.2 designed it, S1 refused, S3 deferred", plus a separate adoption decision | **Yes.** Correct the M6 row and §7 Q2 in the plan, and Amendment 1's §4 row, to point here. |
| Q2 | First cycle scope | **D-314 in full** (search and archive included; a partial overlay is K1/K2). D-315 in the same cycle only if Q3 accepts it, since it is ~1 d on top. |
| Q3 | Is adoption (§6) distinct from §15.4's merge? | **Yes.** No superseder is chosen, no other lineage is read, and the caller asserts. Record the argument in D-315 and amend §15.4 with the distinction, so "merge" keeps meaning the thing it refuses. |
| Q4 | `CrossLineageError` after D-314/D-315 | **Keep the variant**, with its meaning narrowed to "a direct write to `concepts` across lineages", the D-074 class. It is no longer reachable through the API once D-315 lands; say so in its rustdoc rather than removing a variant callers match on. Under D-314 alone it still covers W3. |
| Q5 | Timing | **The cycle right after 0.22.0.** The schema has no deadline (it is additive, F2), but every new content reader added before the overlay is another one to convert (K6). |
| Q6 | Ask Silk first? | **In parallel, not as a gate.** CodeRadar's evidence carries the case; Silk's immutable-text practice stays the recommended application pattern either way. |
| Q7 | Where this lives | Accept the file as **Amendment 2**. On acceptance, promote it to D-314 and D-315 on `dev/0.23.0` (or `dev/m6-overlay`) as a docs-only planning commit, not inside 0.22.0's tail. |
| Q8 (new) | U-3, the valid-axis fork cut | **Separate.** It concerns the fork, not concept identity, and needs its own small amendment. Note it here so CodeRadar's U-list is fully answered. |

## 14. Bounds of this exercise

- No code in either repo; no register, plan or CodeRadar doc edited.
- No probes run. §8 is the list, not results. The reader count (27 sites in 12 files) is a grep of
  `src/` on `dev/0.22.0` at `731d334`, not a classification.
- CodeRadar anchors C2–C4 and U-3/U-4/U-5 were re-read for this revision. C1 and C5–C9 are carried
  over from revision 1 unchecked.
- The `concept_versions` column list in §5.1 is a sketch for costing. P-1 and P-3 settle its
  final shape, including how the FTS gets a rowid.
- okfgraph gets a summary thought only after the review, so the graph records the decision, not
  the proposal.

— end of Amendment 2 (proposed, revision 2) —
