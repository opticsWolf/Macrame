# Macrame Update Plan v0.19.0 — the blob store

**Status:** planned · **Branch:** `dev/0.19.0` · **Date:** 2026-10-04
**From:** 0.18.0 (schema v21, concepts payload v3, snapshot v5, surface 1,836 items)
**To:** 0.19.0 (**schema v22 — one rung**; payload and snapshot unchanged)
**Source:** [Macrame Update Plan v0.18.0](Macrame%20Update%20Plan%20v0.18.0.md), item P4 and
draft [D-281](Macrame%20Update%20Plan%20v0.18.0.md#d-281-draft); the deferral is recorded in
[releases/v0.18.0](releases/v0.18.0.md).

One item. 0.18 opened the attribute layer; this release gives those attributes
somewhere to point at bulk bytes, under the same rule: the store gets no
reclamation path of its own and uses the archive's.

| Item | Work | Schema |
|---|---|---|
| P4 — blob store | 4 d+ | v21 → v22 |
| P4a — eligibility scan and copy-back | inside P4's estimate, gated separately | none |
| C1 — `MAX_EXTRA_BYTES` against OKFgraph | ~2 h, optional | none |

---

## 0. What D-281 left open, and what this plan settles

D-281 decided the *shape* of reclamation — reachability, not expiry; the
archive, not a refcount; `cold.blobs`, not a delete. It did not decide three
things the implementation cannot proceed without, and the 0.18 plan's own
rejection of *reachability-checked GC* ("needing an index over log payloads to
avoid a full scan") makes the first of them look harder than it is.

1. **What counts as a reference.** `extra` is application JSON; the crate does
   not parse it for meaning. Settled by [D-287](#d-287-draft): the digest's
   lowercase hex, appearing anywhere in a hot payload.
2. **How a session finds the referenced set without a per-blob scan.** Settled
   by D-287 as well: one streaming pass over hot payloads per session, and only
   when there is a candidate to decide.
3. **What happens when a hot entry names a blob that is already cold.**
   Reachable by two ordinary paths. Settled by [D-288](#d-288-draft): the same
   session that moves blobs out copies named ones back.

### Why the hot log is the right set, and the only one needed

`LOG_ARCHIVABLE` never takes the newest entry per `(entity_id, branch_id)`
partition, and since [D-269] never takes an entry a fork point is pinned to. So
the hot log is already *exactly* the set of entries something still reads:
every entity's latest state on every lineage, plus every past state a live
branch is reading through. "Every log entry naming it has gone cold" is
therefore "no hot log payload names it", and nothing beyond the hot log has to
be consulted:

* hot `concepts` and `links` rows are described by their newest log entry, which
  is hot by the rule above;
* rehydration writes no log entry, and the rehydrated concept's newest entry
  was never archived — so rehydration needs no blob step at all;
* snapshots carry `extra`, and so carry references, but a snapshot is a past
  state and past states are what `cold.blobs` exists to keep readable.

### What it does not reclaim, stated up front

**A blob named by any entity's latest state stays hot for as long as the ledger
holds that entity's newest entry** — and that entry is never archivable, retired
concepts included. Retiring a concept does not free its last attachment; only
*replacing* an attachment does, once the replaced version goes cold. This is the
ledger being honest rather than the blob store being weak: the retired
concept's final state is still a fact the hot file answers for. The one path
that releases a latest state is `archive_branch`, for a branch's own entities.

---

## Draft decision-register entries

Drafts, as in 0.18: nothing here enters `architecture/s13-decision-register.md`
until the work lands. D-281 is the number 0.18 reserved; **D-287 and D-288 are
new**, D-286 being the last allocated. Flat numbering, per the 0.18 note that
retired the lettered sub-entries.

<a id="d-281-draft"></a>**D-281** — Blobs are reclaimed by the archive, not by a refcount (0.19.0, P4).
*Carried from the 0.18 plan with three amendments, all marked.* The
content-addressed blob store ships `blob_put` / `blob_get` / `blob_stat` and no
`blob_link`, `blob_unlink` or `blob_gc`; the refcount design is withdrawn
because a versioned `extra` names a blob permanently, and a counter of present
references reaches zero while a past state still depends on the bytes. A blob is
eligible when no hot log entry names it ([D-287](#d-287-draft) says what
*names* means) and it then moves to `cold.blobs`. `blobs` is a **rowid table
with a `UNIQUE` index on `sha256`**, not `WITHOUT ROWID`, so a multi-megabyte
payload is not an overflow chain hanging off the key b-tree. Single-blob cap
**8 MiB**, overridable through `Tuning`, because libsql 0.9.30 exposes blobs
only as `Value::Blob(Vec<u8>)` and every operation materializes the file at
2–3× its size. Blob writes are exempt from `CHUNK_BUDGET` with their own
warn-above-hold threshold; WAL pressure is `Tuning::wal_autocheckpoint`.

**Amendment 1 — an age guard, on Doctrine II's terms.** "No hot entry names it"
is vacuously true of a blob put a moment ago and not yet named, and a caller
always puts the bytes *before* writing the concept that names them. So a blob is
a candidate only when its `put_at < :cutoff`, which is `LINKS_ARCHIVABLE`'s and
`CONCEPTS_ARCHIVABLE`'s own transaction-time clause. A re-put of a digest the
store already holds **refreshes `put_at`** rather than leaving it at first
storage, because the re-put is the caller announcing an imminent reference; it
is the one column a guard lets change ([D-288](#d-288-draft)).

**Amendment 2 — `archive_branch` does not run the blob step.** It has no cutoff,
so it cannot apply amendment 1, and a blob step without an age guard would take
every unnamed fresh blob in the file. It needs none: it moves the lineage's log
entries cold, and the next ordinary `archive` finds their blobs unnamed. This is
the same self-clearing `LINKS_ARCHIVABLE`'s closed-interval arm documents, and
eligibility stays cross-lineage for the reason the 0.18 draft gave.

**Amendment 3 — the address is SHA-256, and now it is argued.** The 0.18 draft
named the column `sha256` without arguing for it, and nothing else in the crate
hashes with any cryptographic function, so there was no existing choice to stay
consistent with. The digest is SHA-256, written as 64 lowercase hex characters.
**The choice is permanent, and that is why it is argued here rather than left
to the implementation:** digests enter versioned log payloads, which never
change, so a later switch would leave every historical reference naming an
address the store no longer computes, with no migration that could reach them.
A permanent identifier should be the most widely computable one, not the
fastest:

* **Applications compute it too.** D-287 puts digests in `extra`, `content`
  and link `properties`, and a caller will want to compute one outside Macrame
  — to check *do I already have this file* before sending 8 MiB. SHA-256 is
  `hashlib.sha256` in Python's standard library, `sha256sum` on the command
  line, and the address most other content-addressed systems use, so a digest
  from elsewhere matches as is and no caller needs a package to agree with the
  store.
* **Speed is not where a put spends its time.** BLAKE3 is several times faster
  in software. But an 8 MiB put also copies the bytes two or three times
  through libsql's `Vec<u8>` and writes them to pages and the WAL, and `sha2`
  uses the CPU's SHA instructions where they exist. Gate 12 times the hash on
  its own, so this is measured rather than assumed; if hashing turns out to
  dominate a put, the register entry says so, and the choice stands anyway for
  the permanence reason above.
* **Same width as the alternative.** 256 bits, collision-resistant, so two
  different blobs cannot share an address by accident or on purpose, and
  D-287's 64-character scan is the same whichever 256-bit hash is chosen.

**The dependency.** `sha2` (RustCrypto, MIT OR Apache-2.0, matching the
crate's own licence) is pure Rust and declares `rust-version = 1.85`, under
this crate's 1.88 floor; the floor is still re-checked with `cargo +1.88.0
check --all-features --all-targets`, the command the `Cargo.toml` comment
records. **Corrected on landing:** it adds **seven** packages to `Cargo.lock`
— `sha2` 0.11.0 and six RustCrypto support crates (`digest` 0.11, `block-buffer`
0.12, `crypto-common` 0.2, `hybrid-array` 0.4, `const-oid` 0.10, `cpufeatures`
0.3). The draft expected some to be shared with libsql's TLS stack; none are.
`crypto-common` and `cpufeatures` were already in the tree, but at the previous
majors (0.1, 0.2), which Cargo builds alongside rather than unifies.

**Why a dependency, against the CRC-32 precedent.** `util/crc32.rs` was written
by hand because one checksum per snapshot, behind zstd and bincode, had no
measurement to justify a crate. Neither half carries over: this hash runs on
every put over up to 8 MiB, where the hardware path matters, and a subtly wrong
CRC is caught by its test vector and costs a refused snapshot, while a subtly
wrong content address silently aliases blobs.

Rejected: *BLAKE3* (faster, and as safe; but a permanent address that every
caller has to install a package to reproduce is the wrong trade for a speed
gain the write path may not notice); *SHA-256 through `ring`* (already compiled
in through libsql's default TLS features, which is exactly why it cannot be
relied on: it disappears the day those features are turned off); *a
hand-written SHA-256, on CRC-32's precedent* (above); *an algorithm-tagged
address such as `sha256:…`* (it would make switching possible, but D-287
deliberately requires no prefix, and a second algorithm, if ever needed, is a
new column and a second scan pattern).

Rejected: *refcount plus `blob_gc`*; *reachability-checked GC outside the
archive* (a second reclamation path, physically deleting outside a session);
*append-only with no reclamation* (an unbounded file deferred to whoever hits
it first); *`archive_branch` running the step with `archived_at` as cutoff*
(the Wave 4.5 two-clock defect the `archive_horizon` comment describes,
committed on purpose).

<a id="d-287-draft"></a>**D-287** — A reference is the digest's text, found by one scan per session (0.19.0, P4).
**What counts:** a blob is named by a log entry when its digest — 64 lowercase
hex characters, exactly what `blob_put` returns — occurs **as a substring
anywhere in the entry's `payload`**: inside `extra`, inside `content`, inside a
link's `properties`, inside a URL or a longer hex run. No key convention, no
`$blobs` array, no prefix. The point is the direction of the error. A scan that
recognizes too much holds a blob that could have gone cold, which costs disk; a
scan that recognizes too little archives a blob a hot entry needs, which costs
an answer. Every narrowing rule — a reserved key, a `blob:` prefix, token
boundaries — buys precision in the harmless direction by adding failure modes in
the harmful one, the first time an application stores the digest some other way.
The only encodings that are **not** references are ones that do not contain the
lowercase hex text at all (uppercase, base64, a truncated prefix), and the API
documentation says so in one sentence. `json(...)` in the log triggers never
escapes a hex character, so what the application wrote is what the scan sees.

**How it is found.** Inside the archive session, after the log has been
archived, the candidate set `C` (hot blobs with `put_at < :cutoff`) is read
first. **If `C` is empty and `cold.blobs` is empty, the step ends there**, so a
ledger that never stored a blob pays one indexed probe and no scan — gate 11
asserts it. Otherwise hot `transaction_log.payload` is streamed once, in Rust;
each maximal run of `[0-9a-f]` of length ≥ 64 contributes every 64-character
window that is a member of `C` (or of the cold digest set, for
[D-288](#d-288-draft)) to the referenced set `R`. Windows rather than tokens,
because a digest embedded in a longer hex run is still the digest; membership
tested per window rather than all windows inserted, so memory is bounded by
`|C|`, not by the log. One pass, O(hot payload bytes), and no matcher crate:
the window test is a `HashSet` lookup.

**Why this is not the index the 0.18 draft rejected.** That rejection was of
reachability-checked GC as a *standalone* operation — run on demand, per blob,
needing an index to avoid scanning the log each time. This is a set
computation run once per session, inside a transaction that already scans the
log for `LOG_ARCHIVABLE`, over a log the session has just made as small as it
will get. The scan is measured as part of the session's hold (gate 12), and if
the measurement says otherwise, the escalation is the write-time `blob_refs`
table rejected below — named now so it is not rediscovered.

Rejected: *a write-time `blob_refs` table filled by the log triggers through
`json_tree`* (exact and incremental, but it puts JSON tree-walking on every
concept and link write that gate 8 of 0.18 just re-measured, and costs a
fourth trigger drop/recreate — the escalation path, not the starting point);
*a reserved key in `extra`* (misses references in `content` and in link
`properties`, which is the harmful direction); *`instr(payload, sha256)` per
candidate in SQL* (O(|C| × hot log), which is the per-blob scan the 0.18 draft
was right to fear).

<a id="d-288-draft"></a>**D-288** — Blobs are immutable, guarded, and copied back when named (0.19.0, P4).
**Immutable.** A content address that can be rewritten is not one. `blobs`
carries a `BEFORE UPDATE OF sha256, size, bytes` guard that aborts
unconditionally — no marker, no session can need it — and the only column an
update may touch is `put_at` (D-281 amendment 1). **Delete is marker-gated**,
exactly as `links` and `concepts` are since D-126: the archive session is the
only place a blob may physically leave the hot file, which is Doctrine V's rule
for every archive participant. `verify` requires both guards and body-probes
the delete guard for the marker, on [D-282]'s reasoning: a guard with the right
name and the wrong body is the failure verification exists to catch.

**Copy-back.** Two ordinary paths leave a hot entry naming a cold blob: an
application writes a digest it remembers without re-putting it, or a session
with a cutoff near *now* runs between `blob_put` and the write that names it.
Neither loses bytes — `blob_get` reads through to `cold.blobs` — but each
breaks the property worth having: *a blob named by a hot entry is hot*. So the
session computes `R` against the cold digests too, and **copies** every cold
blob in `R` back to `blobs`, with `put_at` set to the session's `archived_at`.
Copies, not moves: two rows with one content address cannot disagree, and
deleting from `cold.blobs` would need a cold-side guard for no gain. The cold
copy stays, and a later session that sends the blob cold again finds it already
there (`INSERT OR IGNORE`).

**Read-through.** `blob_get` and `blob_stat` look hot, then — when
`archive_present` — cold, through the same ATTACH/DETACH discipline
`rehydrate` uses, and never write to the cold file. `blob_stat` reports which
side answered. A cold file written before 0.19 has no `blobs` table; the reader
treats that as *absent* (`Ok(None)`), and only the archive writer creates the
table, inside its own transaction — `cold_has_extra`'s rule.

**Outside the snapshot, inside the backup.** A snapshot is a
`MaterializedState`; it carries `extra` and therefore digests, never bytes.
What carries blobs is the file-level backup of the hot file *and* the cold file
— the same sentence D-280 wrote for `kv_store`, with the cold file added.

Rejected: *move back instead of copy* (needs a delete on the cold side, which
has no guards by design); *refusing a write that names an absent digest* (the
crate does not parse payloads at write time, and starting to would be D-287's
rejected trigger walk); *read-through without copy-back* (correct while the cold
file exists, and quietly makes the hot file depend on it for current state,
which no other hot entity does).

---

## 1. Work breakdown

**Rung v21 → v22** (`schema::migrations`, `schema::ddl`):

```sql
CREATE TABLE blobs (
    blob_id  INTEGER PRIMARY KEY,
    sha256   TEXT    NOT NULL UNIQUE
             CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    size     INTEGER NOT NULL CHECK (size = length(bytes)),
    bytes    BLOB    NOT NULL,
    put_at   TEXT    NOT NULL /* canonical_ts_check!(put_at) */
);
CREATE INDEX idx_blobs_put_at ON blobs (put_at);
-- trg_blobs_frozen_update, trg_blobs_guard_delete (marker-gated)
```

The column is named for its algorithm rather than a neutral `digest`, so the
schema says what the address is and a second algorithm, if one is ever needed,
arrives as a second column rather than as a reinterpretation of this one
(D-281 amendment 3). The `sha256` check is on the disk, not only the API,
because D-287's scan matches lowercase hex and a raw writer storing uppercase
would hold a blob the scan can never name. `put_at` is the session's candidate filter, hence the
index. One more `canonical_ts_check!` call site, beside the seven in
`schema::ddl` today.

**`src/blob.rs`** — validation, hashing, `put` / `get` / `stat`,
read-through. **Hashing is a new dependency**, `sha2`; D-281 amendment 3 says
why that crate, why not a hand-written hash, and how the 1.88 floor is
checked. The release note names it. One private function computes the digest
and `blob_put` is its only caller, so there is a single place the address is
decided.
`BlobStat { sha256, size, put_at, location: BlobLocation::{Hot, Cold} }`,
`#[non_exhaustive]`.

**`Database`** — `blob_put(bytes) -> Result<String>`, `blob_get(&str) ->
Result<Option<Vec<u8>>>`, `blob_stat(&str) -> Result<Option<BlobStat>>`. Puts go
through the writer actor; reads use `read_conn`, as `kv_get` does.
`Tuning::max_blob_bytes` (default `8 * 1024 * 1024`); `BLOB_WARN_HOLD` beside
`BULK_ATOMIC_WARN_HOLD`. Errors: `DbError::BlobTooLarge { size, max }`,
`DbError::InvalidDigest(String)`; the binding maps both.

**Archive** — `COLD_SCHEMA` gains `cold.blobs` (same shape, no guards, no
`CHECK` on `size`, which a cold file must not re-validate against a future
rule); `archive_session` gains the blob step after the log delete and before
the horizon row; `ArchiveReport` gains `blobs_archived` and `blobs_restored`
(non-exhaustive, so additive).

**Python** — `blob_put(bytes) -> str`, `blob_get(str) -> bytes | None`,
`blob_stat(str) -> BlobStat | None`; stubs and `tests_py`. No digest helper:
`hashlib.sha256(data).hexdigest()` is the address, and the docstring says so.

**Docs** — §4.10 in `s4-schema.md`; §5 module entry; `quickref.md`; release
note; the three register entries; `public-api.txt` re-blessed.

## 2. Acceptance gates

1. **Round trip.** `blob_put` returns the lowercase hex SHA-256 digest, equal
   to the published NIST test vectors (the empty input included); `blob_get`
   returns the bytes byte-equal; a second put of the same bytes returns the
   same digest, adds no row, and advances `put_at`. In Python, the returned
   digest equals `hashlib.sha256(data).hexdigest()` — the interoperability
   amendment 3 rests on, asserted rather than assumed.
2. **Cap.** `max_blob_bytes + 1` is refused with `BlobTooLarge` naming both
   numbers; a raised `Tuning` value admits it; an empty blob is legal.
3. **The defect D-281 exists to prevent — the negative control.** Concept
   version 1 names `X`, version 2 names `Y`; archive with a cutoff after both.
   `X` is cold, `Y` is hot; `reconstruct` at version 1's instant yields `extra`
   naming `X`, and `blob_get(X)` returns the bytes. A refcount implementation
   passes the second half and fails the first.
4. **Pinned history holds.** As gate 3, with a branch forked between the two
   versions: `X` stays hot, because the fork pins version 1's entry hot
   ([D-269]). Then `archive_branch` the fork and run `archive`: `X` goes cold.
5. **Branch-only references.** A blob named only by a live branch's entry
   survives `archive`; after `archive_branch` plus `archive`, it is cold.
6. **Where references live.** A digest inside link `properties`, inside
   concept `content`, inside a URL string, and embedded in a longer hex run each
   hold the blob. An uppercase digest does not — the documented negative.
7. **Age guard.** With no references anywhere, a blob put after the cutoff
   stays hot; one put before it goes cold; one re-put after the cutoff stays.
8. **Copy-back.** A hot entry naming a cold-only blob: after the next session
   the blob is hot, the cold copy is still present, and `blobs_restored` is 1.
9. **Cold tolerance.** A 0.18 cold file — no `cold.blobs` — attaches;
   `blob_get` of an absent digest is `Ok(None)` and the file's bytes are
   unchanged; the next `archive` creates the table.
10. **Guards.** `DELETE FROM blobs` outside a session is refused; `UPDATE` of
    `bytes` is refused inside one; `UPDATE` of `put_at` is permitted; `verify`
    fails naming `trg_blobs_guard_delete` when its body lacks the marker.
11. **Zero cost when unused.** On a ledger with no blobs, an archive session
    performs no payload scan — asserted by a counter, not by timing.
12. **Hold, measured.** The session's blob step timed at §9's ladder sizes with
    `|C|` of 0, 10 and 10,000, and the blob put/get budgets at 64 KiB, 1 MiB and
    8 MiB added to `benches/budgets.rs`. If the scan dominates the session at
    the top rung, D-287's escalation is taken before release, not after.
    **The hash is timed on its own** at the same three sizes, so a put's budget
    splits into hashing and writing. That number checks D-281 amendment 3's
    claim that speed is not where a put spends its time; the register entry
    records the share either way, and if hashing dominates, it says so
    alongside why SHA-256 stands.
13. **Rung.** v21 → v22 on a populated database; `verify` clean; a v22 file
    handed to a 0.18 binary is refused on schema version — the release-noted
    forward incompatibility.

## 3. Carried from 0.18

- **C1 — `MAX_EXTRA_BYTES` (64 KiB) against OKFgraph's real frontmatter
  sizes.** Still unchecked. Now that bulk bytes have a home, the cap's job is
  only to keep attributes attribute-sized; measure the 99th percentile in the
  OKFgraph corpus and move the constant only if it bites. A one-line change.
- **`extra` in `concepts_fts`** — not raised for 0.19 either.

## Open

- Whether `blob_put` should take a stream-shaped argument now, to be ready for
  a libsql that exposes incremental blob I/O, or keep `&[u8]` and change the
  signature when that driver lands. Leaning `&[u8]`: an API shaped for an I/O
  model the driver cannot perform would promise what the 8 MiB cap exists to
  deny.
- Whether `ArchiveReport` should report bytes moved as well as counts. Cheap to
  add while the step is being written; nobody has asked.

[D-269]: architecture/s13-decision-register.md#d-269
[D-282]: architecture/s13-decision-register.md#d-282
