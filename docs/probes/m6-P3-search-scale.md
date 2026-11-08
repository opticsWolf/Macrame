# M6-P, probe P-3 — a branch search at production scale, and the fallback decided

For [Amendment 2, §11](../Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md),
probe **P-3**: *"plan and time a branch search with 1,000 versions over 100,000
concepts; the stale-hit red test"* — the question being **whether the
version-aware filter is cheap enough**, or whether the FTS shape *"falls back to
a per-lineage FTS rebuild if the filter scans."* Also K7's storage fear:
*"the FTS and embedding storage roughly doubles for heavily versioned stores."*

The stale-hit red test the row asks for is M6-c's own:
`a_branch_search_for_the_trunks_superseded_text_does_not_return_the_concept`
(`tests/m6c_search_tests.rs`), landed with M6-c at `3bc0283`. This probe is the
row's measurement half: `examples/m6_p3_search_scale_probe.rs`, run on
`dev/0.22.0-revA` past M6-d (`0e2defa`), release profile, medians of 7 samples
per arm after a warm-up, one database for the whole run.

## The ledger

100,000 trunk concepts (`write_concepts` in 2,000-chunks, the release bench's
seeding shape) with FTS text (~100 `topic N` words of the corpus each) and
2-dimensional embeddings on a cone (`register_model("m6_p3", 2)` +
`upsert_embeddings`); one fork `b0`; **1,000 supersede versions** spread every
7th id through the corpus, whose version text deliberately carries none of the
trunk's query words. `upsert_version_embeddings` fills the version pool for the
same pair keys. So a `corpus` search on the branch sees every versioned id
appear among the trunk arm's bm25 hits **and** get dropped by the registration
(the stale-hit red test's shape at scale), while `topic 41` sees almost none
(the light arm).

## The numbers (release, medians of 7)

| arm                                                | `100k / 1k versions` | `100k / 100 versions` |
| -------------------------------------------------- | -------------------: | --------------------: |
| kw trunk, whole-file, light word (`topic 41`)      |            3.2–3.5 ms |             3.2–3.5 ms |
| kw trunk, whole-file, widest word (`corpus`, all 100k match) | 72–77 ms |             73–77 ms |
| kw branch, light arm (`topic 41`)                  |           9.7–10.7 ms |            9.7–10.0 ms |
| kw branch, worst arm (`corpus`: every versioned id stale) |    191–204 ms |          184–198 ms |
| vec trunk (whole-file, 100k pool)                  |            1.0–1.3 ms |            1.3–1.4 ms |
| vec branch (union + version pool + drop)           |            7.9–23 ms |            4.2–9.2 ms |
| control (`SELECT 1`)                               |              1–3 µs |               1 µs |

Branch-minus-trunk deltas, the filter's own bill:

- **light keyword arm: +6.4–7.3 ms**, flat across 100 → 1,000 versions. The
  union (the trunk statement again, its CTE-visible clause with the overlay's
  per-row checks, plus the version pool's own bm25) and a near-empty
  registration drop. This is the version-aware filter's standing cost.
- **worst keyword arm: +112–129 ms**, and **the same size whether 100 or 1,000
  versions hold the subject**. The drop itself is light (the light arm proves
  per-hit resolution is µs-scale); what prices the widest word is the fetch
  loop's *escalation*: each step re-runs the trunk arm at a grown `LIMIT`, and
  the trunk arm still `MATCH`es the whole 100k corpus and sorts the whole
  bm25 answer before the `LIMIT` sheds it. Over a ~1 % stale rate, the root
  arm's re-runs — not the drop — are the cost.
- **vector arm: +2.8–7 ms at 100 versions; 6.6–13 ms most runs at 1,000**
  (one 23 ms outlier). Scales with the version pool's size, its own scoring
  plus the same drop.

## Decision

**The version-aware filter is cheap enough. The per-lineage FTS rebuild stays
off the FTS shape.** Recorded:

1. The filter's standing cost — the doubled union plus the registration — is a
   flat ~7 ms on a whole-file keyword arm that costs 3 ms under no lineage, and
   a similar order on the vector arm, whose whole-file base is 1 ms. No corpus
   scan hides beneath it: the version pool is matched on `concept_versions_fts`
   (1,000 rows), the trunk arm's index is untouched, and the drop touches only
   the hits the Match returns.
2. The wide-word worst case (~200 ms) is priced by the **escalation** — the
   fetch loop's whole-corpus re-runs under grown `LIMIT`s — and would stay
   grounds for a per-lineage rebuild *only if the rebuild were cheaper to keep
   than the escalation is to run*. It is not: a per-lineage FTS rebuild
   multiplies storage per forked lineage and breaks the union shape the
   readers' resolution law stands on, to speed a worst case that is still one
   100k-row bm25 sort repeated a handful of times. Declined for 0.22.0.
3. The escalation, not the filter, is the one future row if branch search on
   very wide words ever needs polish: a rank-cursor escalation (fetch the next
   slice at rank-threshold instead of re-running the match at a grown limit)
   would cut the worst arm toward its base. Not taken in 0.22.0; the reader's
   law is unchanged either way — the drop rides whatever fetch the loop runs.
4. **Storage (K7): ~1.2–1.5 KB per version** at this shape (the version's FTS
   row, log mirrors, version pool, cut rows where pairs close). At C9's hoped
   share — versions a small fraction — 1,000 versions cost 1.2 % of a 105 MB
   store; the K7 doubling materializes only at ~100 % version share, where the
   store's own mirror projection is what doubles. The amendment's paragraph
   holds as written; C9 is what makes it irrelevant in practice.

## Repo notes

- `M6_P3_NODES` / `M6_P3_VERSIONS` env knobs reproduce the second
  configuration without a second artifact (defaults 100,000 / 1,000).
- The probe's version text carries **none** of the trunk's query words, so the
  worst arm's escalation is top-of-form: every taken-over id the trunk arm
  returns is a stale hit the registration must drop (K1's bug shape at scale).
- One vocabulary point the numbers sharpened: **the drop filter and the
  escalation are different costs**. Amendment 2 §11's fallback sentence says
  "if the filter scans"; at this scale the filter does not scan — the
  escalation re-runs the corpus. The two answers are recorded separately above.
