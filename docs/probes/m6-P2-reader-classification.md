# M6-P, probe P-2 — the `concepts` reader census, classified

For [Amendment 2, §8](../Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md), probe **P-2**:
classify every `concepts` read under §5.4 cost 1, so §11's M6-b estimate prices actual sites and
the K2 gate ("a test asserts the count of content readers, so a new reader fails it until
classified") has its blessed list.

Run on `dev/0.22.0-revA` at `35256c6` with:

```text
grep -rnEi "(FROM|JOIN|INTO|UPDATE)\s+concepts($|[^_a-zA-Z])" src/ | (excluding concepts_fts, concepts log, concept_disorder)
→ 27 hits, 12 files
```

The amendment's "27 sites in 12 files" reproduces exactly with the INTO/UPDATE-inclusive pattern;
the loose grep the bounds paragraph reported was the same number via slightly different
auspices. One of the 27 is a rustdoc line (`builder.rs:11`, the `AttributeMode::Current` doc),
one is a test fixture (`archive.rs:2163`, inside the archive module's own tests), and the rest
are genuine storage statements.

## Classes

- **content** — reads concept *beliefs* (text, validity, retirement, archivability, existence of a
  live belief). Under the overlay these need the §5.3 resolution: reads on a lineage consult
  `concepts` ∪ `concept_versions` with fork-cutoff-path filtering. These are the readers K2 pins.
- **identity** — reads the *mint row's* provenance by id (which lineage holds it, its stash for an
  error message, a rowid contract). The mint row answers exactly; unchanged at v26.
- **write machinery** — not readers: the upsert path, D-312's open-belief lookup and close, delete
  and restore paths. §7.2's "parametrized on the target table rather than copied."
- **guard / fixture** — DDL triggers and in-file test fixtures. Untouched.

## The census

| # | Site | What it reads | Class | What D-314 does |
|---|---|---|---|---|
| 1 | `src/graph/builder.rs:11` | `AttributeMode::Current` semantics (rustdoc) | content | Current mode resolves (§5.3) once versions exist |
| 2 | `src/graph/builder.rs:636` | CURRENT walk: `CROSS JOIN concepts c ON c.id = w.node_id WHERE c.retired = 0` | content | the walk's node join resolves |
| 3 | `src/graph/builder.rs:840` | `build_sql_at_recorded` variant, same shape | content | resolves at a recorded cutoff (already cut-aware for edges) |
| 4 | `src/graph/concept_lineage.rs:188` | D-310's fold CTE: `FROM concepts c JOIN cut_cv…` | content | the resolution engine itself: fold FROM `concepts` ∪ `concept_versions` |
| 5 | `src/graph/concept_lineage.rs:371` | live-id membership: `SELECT id FROM concepts WHERE retired = 0 AND id IN` | content | membership widens to a lineage's visible beliefs (versions included) |
| 6 | `src/graph/subgraph.rs:1265` | node payload hydrate (title/… /extra ∈ list) | content | hydrate resolves |
| 7 | `src/temporal/as_of.rs:454` | payload hydrate for as-of/current reads | content | hydrate resolves |
| 8 | `src/graph/vector_filter.rs:525` | embeddings → `JOIN concepts AS c ON c.id = e.concept_id` | content | filters resolve; P-3 pairs this with version embeddings |
| 9 | `src/temporal/archive.rs:509` | archivable census via `CONCEPTS_ARCHIVABLE` | content | predicate gains the version-reachability arm (P-4) |
| 10 | `src/temporal/archive.rs:1214` | archivable count, same predicate | content | same arm |
| 11 | `src/temporal/archive.rs:1257` | `DELETE FROM concepts WHERE {CONCEPTS_ARCHIVABLE}` | content* | the *predicate* changes; the delete's by-id mechanics stay (cost 5) |
| 12 | `src/temporal/archive.rs:1437` | branch-local existence guard (`c.branch_id = :branch AND EXISTS links…`) | content | predicate arm (P-4) |
| 13 | `src/temporal/archive.rs:1686` | branch archivable count | content | predicate arm |
| 14 | `src/temporal/archive.rs:1712` | branch archive's concept set for log cleanup | content | predicate arm |
| 15 | `src/util/clock.rs:111` | `MAX(recorded_at) … FROM concepts` — the cadence probe | content | extends to `∪ concept_versions`, else version stamps stay invisible to the ceiling |
| 16 | `src/branch.rs:669` | ancestry explorer's hold lookup (`SELECT … FROM concepts c WHERE c.id = ?1`) | identity | provenance question — the mint row answers; grows richer under D-315, not required at v26 |
| 17 | `src/error.rs:1588` | `ContextSource`: mint lineage for diagnostics | identity | mint answers |
| 18 | `src/error.rs:1626` | `does id exist at all` existence probe | identity | mint answers |
| 19 | `src/error.rs:1640` | recorded stamp for error context | identity | mint answers |
| 20 | `src/temporal/archive.rs:2025` | rowid-span verification (`COUNT(*) WHERE rowid_pk = ?1`) | identity | the VACUUM contract P-1 pinned; untouched |
| 21 | `src/connection.rs:5902` | `UPSERT_CONCEPT` (`ON CONFLICT(id) DO UPDATE`) | write | W1's statement; routing happens before it |
| 22 | `src/connection.rs:7057` | `OPEN_CONCEPT_BELIEF` (D-312's lookup) | write | W2 parametrizes on the target table |
| 23 | `src/connection.rs:7085` | `CLOSE_SUPERSEDED_CONCEPT` (log row + mint read) | write | W2, likewise |
| 24 | `src/temporal/archive.rs:1722` | delete by branch territory (`DELETE … WHERE branch_id = :branch`) | write | untouched: deletes a lineage's own rows |
| 25 | `src/temporal/archive.rs:2037` | rehydrate restore INSERT (rowid-preserving) | write | restore includes the notion of versions (P-4) |
| 26 | `src/temporal/archive.rs:2063` | rehydrate restore INSERT (fresh-row arm) | write | same |
| 27 | `src/schema/migrations.rs:1734` | rung rebuild reads-all-then-drop | machinery | v26's rung follows D-036's rebuild shape; nothing overlays it |
| — | `src/schema/ddl.rs:586` | `trg_concepts_branch_immutable` | guard | §5.1: stays |
| — | `src/temporal/archive.rs:2163` | in-file test fixture INSERT | fixture | untouched |

\* the delete's *predicate* is a content read; its write mechanics are not. Counted once, under
content (site 11), so the K2 test forms a wedge from the predicate. New rows in this census for
that reason: delete-statements with a `CONCEPTS_ARCHIVABLE` suffix land under content and
delete-statements under branch territory under write, as above.

## What it costs

- **content = 15** (header rows 1–15 above), **identity = 5**, **write machinery = 7**,
  **guard/fixture = 2** (+ the rustdoc re-class at row 1 counted under content).
- The amendment's cost count grows by the two *extends* (rows 5 and 15): membership and the clock
  silo. §11's M6-b estimate does not move from this; M6-b's actual reader work is rows 2–8 and the
  shared hydrate, plus the archive predicate arm in M6-d.

## The K2 gate, to write with M6-a

The content-class list pins two things the D-224 pattern wants:

1. the census count is asserted (27 + this doc's classes), so a new concept read fails the test
   until it names its class in its rustdoc;
2. each content row's rustdoc gains `over-reader:` tag pointing here; the classification doc is
   stable because the register entry D-314 will cite it — *"what the reader census said on the
   day the decision landed"*.


## M6-a's recount, and M6-b's conversion (0.22.0, on `dev/0.22.0-revA`)

The rung changed the count: `concept_lineage.rs`'s DFS, the rung's rebuild steps in
`migrations.rs`, `error.rs` and M6-a's routing reads in `connection.rs`, and the as-of rustdoc
bring the census to **34 sites in 12 files** at `dev/0.22.0-revA` after M6-a (probe commit
`bccdc9a`); the probe's 27 was the day of the original sweep. The K2 gate
(`tests/k2_reader_census_tests.rs`) pins the 34/12 shape and re-fails on any new unclassified
`concepts` row.

M6-b converts the content rows behind the switch (Amendment 2 §5.4 cost 1's add-divider; the
gate is `graph::concept_lineage::overlay_engaged` — a forked ledger with something in the
overlay):

- rows 2–3 (the traversal's node joins) resolve through `filter_visible`, which now threads the
  traversal's own `as_of_valid` so `AT_TIME` narrowing runs §5.3's walk at the instant rather
  than narrowing current-first;
- row 4 (the D-310 fold) stays byte-identical as the walk's fall-through arm
  (`legacy_rows_on`); the walk (`overlay_scan`) answers `concept_versions` and `concept_cuts`
  per the resolution — a version wins over a cut where both cover, a cut stops the walk, and at
  the axis's far end "covering" means the version is open and every cut covers the present;
- row 5 (`live_ids`, the named tier) keeps D-214's law; ids the walk answered *absent* are
  barred from the named tier by the callers, on the branch's-own-cards rule;
- row 6 (the subgraph loader) and row 7 (the hydrate) are the same reader family through
  `rows_on`, whose merge is decisions-first, fold-second;
- row 8 (the traversal's vector filter) resolves its candidates through the same walk before
  either strategy scores; the strategies' own predicates stand down on engaged stores
  (`PreFilterCTE`; the post-filter's pool sources the lineage's D-310 fold via
  `search_vector_on`), and drops work like the M6-c stale-hit gate will;
- row 15 (the clock silo) extends `MAX(recorded_at)` over `concept_versions` ∪ `concept_cuts`.

Still open in this family: the archive predicate arm (row 9–14), which the amendment
prices into M6-d.

## M6-b's continuation recount (0.22.0, the post-merge review): 37 sites in 12 files

The write-side correction moved the census again — `connection.rs` from 6 to 9, all three
new rows **write machinery**:

- the route's P-7 vista now reads the id's *creation* stamp from the log (the projection's
  `recorded_at` is the last edit's, which read post-fork edits as invisibility); its fallback
  predicate on `concepts` is the legacy row of that read;
- the effective-state read (`effective_state_at`) is a hybrid like the fold's: a projection
  row recorded at the writer's cap, or the pair's newest log row otherwise — one `concepts`
  table arm, one payload arm, one pairing subquery; and the interval-shaped half of the G-1
  law (`refuse_path_interval_w2`) is the same hybrid over the overlay's open windows, plus
  the write's interval upper bound.

The ledger folds themselves (the commit that carried the resolution) added no census rows —
their rows read `transaction_log` and the overlay tables, not `concepts`.
