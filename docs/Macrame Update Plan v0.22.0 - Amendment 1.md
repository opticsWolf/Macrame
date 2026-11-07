# Macrame Update Plan v0.22.0 — Amendment 1: the gaps doc, and what §3.5 corrects in it

**Status:** 2026-11-07 · amends [Macrame Update Plan v0.22.0.md](Macrame%20Update%20Plan%20v0.22.0.md) §3.5 and §4.3
**Answers:** "where is the gaps doc, and which lines does §3.5 correct"

## 1. Where the gaps doc is

"Macrame gaps for Silk tasks and decisions" (header: *2. Nov. 2026 · @W*) is **not a file in any
repository**. It is a document in the Silk author's own editor, and reached Macrame only as a paste
into the planning session of 2026-11-02, the one that wrote the plan. No copy was ever written to
disk, and it is not in okfgraph.
It is not under version control on the Macrame side. Macrame cannot edit it, so it cannot be
corrected with a commit. **The corrections are this document.** The Silk author applies them to
their copy (§4).

Its author says the doc was written from the published 0.21.0 stubs and wrapper, without access to
the Macrame repo or the draft. That is why it disagrees with the draft in places the draft had
already decided.

## 2. The two passages §3.5 corrects, verbatim

Both sit in the doc's "Proposals" section.

**M5 — Fork at an instant, or after a concept**, third bullet:

> • after resolves to the recorded_at of that concept's latest version. That is how a plan
> branches together with a conversation cut after a decision.

**M4 — Change feed**, third bullet:

> • An archived range should report a gap (Change.gap=True) rather than skip silently, so a
> viewer knows to reload.

## 3. The corrections

**C1 (M5, `after=`).** `fork(..., after=concept_id)` resolves to the concept's **mint**, meaning its
earliest log row on its own lineage, not its latest version's `recorded_at`. If it used the latest
stamp, the fork would silently inherit every write to the parent between the concept's mint and its
latest version. Those are writes the caller never chose to branch from.

A caller that does want "after the latest version" passes `at=` with that version's `recorded_at`,
which `concepts_by_id` returns. Silk's use case still works under the mint rule. A decision concept
is minted when the decision is made, and the mint is the cut.

Decided in the draft's D-291, carried into **D-307**.

**C2 (M4, gaps).** An archived range is **refused**, not flagged. `changes(after_seq, …)` raises
`ChangeFeedGapError` (fields `after_seq`, `removed`) when rows after the cursor have been archived.
The draft's reason: "a flag a consumer can ignore is how a silent miss ships."

The viewer's protocol:

1. Catch `ChangeFeedGapError`.
2. Reload its view from a full read.
3. Store `head_seq()` as its new cursor.
4. Resume.

Decided in the draft's D-295, carried into **D-311**.

## 4. Smaller corrections the Silk author should apply in the same pass

The plan did not list these. They are all facts about the shipped or ported surface, not design
choices:

| Gaps doc says | Correct | Where it is decided |
|---|---|---|
| Summary: "schema v24" for the port | **v24 → v25**. v24 was spent by 0.21.0's state rule. | plan §2 |
| M0: branch writes "Since 0.14.9" | **Since 0.14.8** | D-225 |
| M1b: an inherited open version on a branch "is closed by a shadow row… without reversing D-214" | **Not in 0.22.0.** Superseding an inherited concept stays `CrossLineageError`. A shadow row for a concept *is* M6, which needs its own decision. | plan §4.1 |
| M2: registering a kind as functional "should be refused" if duplicates are open | **Accepted.** The next offending write is refused. Registration does not scan. | plan §4.3 |
| M2: `register_edge_kind(kind, *, functional)` | `register_edge_kind(kind, functional=False)`, positional with a default (the draft's signature) | plan §5 |
| M2: `set_pointer(source, kind, …)` / `pointer(source, kind, …)` | parameter name `edge_type`. Same position. | draft D-293 → D-309 |
| M4: `head_seq(*, branch=None)` | `head_seq()` takes no branch. The sequence is database-wide. `changes(after_seq, *, branch=None, tables=…, limit=1000)` filters by branch. | draft D-295 → D-311 |
| M4: `Change` fields `seq_id, recorded_at, table, key, op` | `(seq_id, branch_id, table, entity_id, operation, recorded_at)` | D-311 |
| M7: `concepts_by_id(ids, *, branch, content=True) -> list[NodeData]` | `concepts_by_id(ids, *, branch=None, as_of_valid=None, as_of_recorded=None) -> list[ConceptVersion]` | draft D-294 → D-310 |

The gaps doc and the plan agree on `pointer(..., as_of_valid=)`, which the plan adds (§4.3). They
also agree on `expect=` (M3 → D-313) and on `supersede=` for owned concepts (M1b → D-312).

## 5. What this changes in the plan

- §3.5: no change in substance. "Correct the gaps doc" now means **hand §2–§4 of this amendment to
  the Silk author**. Nothing in the Macrame build waits on it.
- §8: add an okfgraph thought under `macrame-0.22.0-conversation-port` recording C1 and C2 as
  superseded asks. That way a later reading of the gaps doc does not reopen them.
