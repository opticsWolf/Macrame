Plan · W12.2 → v0.15.0 · green-lit

Branching to 0.15.0
§15.2 says ledger tables just gain a branch_id. Fourteen probe sections say that is true of three tables, needs a rebuild on a fourth, is false for concepts, and hides a hole in the cold file that would return branch belief stamped main.

Status go — seven releases scoped
Head 0.14.1 · schema v11 · D-001…D-213
Evidence examples/branch_identity_probe.rs §1–14
Model Option A
0.15.0 W12 complete + W13 placed
Rung 11 steps
Evidence
probe §1–14
What the engine actually said
Fourteen questions, asked of libSQL rather than reasoned about, for the reason D-078 made expensive. §1–5 establish what the existing schema refuses; §6–9 that the resolution is buildable; §10–14 that the cold file is a second schema this rung has to move.

#	Question	Result	Consequence
1	ADD COLUMN branch_id, four tables, 20,000 rows	metadata only
83–139 µs	§15.2's "a rung and not a rewrite" holds. Cost independent of row count.
2	Second lineage's belief about one concept id	refused
UNIQUE concepts.id	The finding. branch_id on concepts is not sufficient.
3	UNIQUE(id, branch_id) with today's single-column FK	refused at write
foreign key mismatch	The table is created anyway. Schema looks correct, every insert fails — a failure mode that survives review.
4	Composite FK (source_id, branch_id) → concepts(id, branch_id)	forbids CoW	The only shape that expresses the widened uniqueness forbids inheritance.
5	Same edge, two lineages — links / links_current	links ok l_current refused	recorded_at separates lineages in links. links_current needs branch_id in its PK — a rebuild.
6	ADD COLUMN on a table with FTS sync triggers and a delete guard	survives	concepts_fts MATCH still hits. External-content FTS on rowid_pk unperturbed.
7	BEFORE INSERT ahead of ON CONFLICT resolution?	yes	Cross-lineage upsert refused, same-lineage accepted, new id accepted. The guard is dead without this.
8	branch_id immutability via BEFORE UPDATE	enforced	Coexists with trg_concepts_monotonic_ra. No interaction.
9	Lineage as a recursive-CTE predicate subquery	correct
main 1 · b 2 · c 2 · sib 1	Sibling excluded — the leak population. MATERIALIZE ancestry once, then SCAN c. One bound parameter, not a list.
10	Does CREATE TABLE IF NOT EXISTS cold.x upgrade an old shape?	no — and reports success	The hole. The archive session's own setup step leaves a v11 cold file at v11 and says nothing.
11	New column list written into the old shape	refused, named
has no column named branch_id	Fails loudly, not silently. The silent-drop only arrives if someone "fixes" it by omitting the column — which is the trap to name, not the default behaviour.
12	ALTER TABLE cold.… ADD COLUMN inside BEGIN IMMEDIATE	accepted	And the insert in the same transaction sees the new column. The in-place upgrade is available where the archive session already stands.
13	Does ROLLBACK undo DDL on an attached file?	yes
columns and rows both revert	The load-bearing one. The archive transaction's atomicity extends over the cold file's shape change; a failed session leaves the cold file untouched.
14	Commit, then fold across a hot v12 and an upgraded cold	round-trips	Pre-existing cold rows read main by definition of the default, which is correct rather than merely convenient.
Assessment
your amendment
The amendment holds; four things in it don't
The step-8 hole is real, the fix works, and the reasoning behind it is sound at every step the engine can be asked about. Four items need correcting before they reach a register entry, where a wrong citation is worse than no citation.

Claim	Verdict	What to write instead
"ALTER cold.transaction_log and cold.links"	incomplete	Three cold tables, not two. cold.concepts exists (0.9.0, C2) and is the one carrying provenance. Miss it and a rehydrated concept returns stamped 'main' regardless of which lineage minted it — which breaks diff and the branch-scoped archive predicate, the two features the column exists for.
"the archive session already holds the write connection … ALTER inside the transaction"	needs a gate	True for the archive path. The cold file is also attached by the read paths — D-026's fold and rehydrate's SELECT … FROM cold.concepts. Those must tolerate the old shape and never upgrade it: a cold file can sit on read-only media or be shared. Upgrade on write, project a literal on read.
"silently drops lineage … silent wrong answer"	overstated	Probe §11: the write is refused with a named error. The silent path exists only if the column is dropped from the writer's list to make the error go away. Name the actual failure — the register's standard is that a hazard described more dramatically than it behaves gets discounted the next time.
"Uncommitted evidence is D-152 inverted"	mis-cited	D-152 is Rehydrate becoming its own CommandKind and keeping its budget exemption. The principle wanted is D-205's — a review nobody can re-run is a review nobody can check — which 0.14.0 already leaned on for the same reason.
"§0.1's rule (documented semantics precede the API)"	mis-cited	§0.1 is the 0.13/0.14 split — 0.13.0 changes what you can observe, 0.14.0 changes what is guaranteed. The precedent for semantics-before-API is D-160 → D-174, which 0.14.1 already cites and which is the stronger fit anyway.
What fits, and is stronger than you put it
The no-op argument is the right justification and it should lead. At 0.14.4 no API can produce a branch row, so lineage('main') = {'main'} and the predicate passes every row. Precise form: no database this crate's API can produce — a caller who wrote raw SQL against v12 can have branch rows, and the 0.14.2 pinning test deliberately creates one. That exception is the designed flip, not a counter-example.
defect Z is exactly the right citation, and the crate is armed for it. W4.8 put debug_asserts on Subgraph::is_closed at the entry of five algorithms. A lineage-filtered hydration that dropped a node would not be a silent wrong answer — it would be a debug assertion failure, which is worse to ship and better to catch. Either way the trigger removes the question rather than answering it.
C-1's nuance is already written. D-176 states it in place: "a foreign key is not in that position — libSQL reports SQLITE_CONSTRAINT_FOREIGNKEY as a number … this is not a second one." The entry cites rather than re-argues, which is cheaper and harder to contradict later.
F-34's own text sanctions your reading. "Severity: Low today, Medium the moment branching lands … the one item in this document I would cut first if 0.15.0 grows, and it is scheduled where cutting it is still possible." The plan's silence was a real omission, and §0 requires the cut be recorded in §19 with its reason.
What might fail
W13 decided in 0.14.5 contaminates both decisions
reschedule
0.14.5 is where the surface grows — fork(), branches(), the view type, Python parity. A declarative layer changes what that API should look like, so deciding the two together means each is argued partly on the other's behalf. Split them: decide W13 at the start of 0.14.5, before the API shape is fixed; build it at 0.14.7 if taken. The evidence F-34 wants — the countable combinatorial surface — is knowable at 0.14.5's design time, which is exactly when the count of builders taking a branch parameter is decided.

The scope trigger is two seeks, and it is write-path
measure first
Checking source and target symmetrically is two unique-seeks per links insert, not one, on a path that already carries two FK checks and trg_links_single_open's EXISTS. 0.14.0's pinned (4, 2, 5) counts are read-path, so nothing pinned moves — but the write path has its own budget and CHUNK_BUDGET is where a per-row trigger cost lands. Price it at 0.14.6 rather than assuming it free.

0.14.4 changes read semantics and ships no Python
unplaced
The plan puts Python parity at 0.14.5 and 0.14.6. But 0.14.4 changes what a read returns, and that is observable from Python without any new binding. W6's finding is about features; this is a semantic change reaching a second language with no release of its own. Either the Python suite's search assertions are re-verified in 0.14.4, or the release notes say plainly that the binding's semantics moved without its surface.

The five you asked confirmed rather than assumed
Two land as written, one is narrower than feared, one is deliberate and pre-existing, and one is materially larger — large enough that it changes the rung.

#	Item	Answer
1	branches rows mutable	accepted, and widened Correct, and the no-FK decision is what leaves it open. Refuse every UPDATE, not three columns. No column on that table legitimately changes, a whole-row guard needs no maintenance when a column is added, and it removes the "which columns" question from every future reader. Same ABORT_* shape, same classify edit as C-1, one test.
2	Rehydrate is a second mover	confirmed, narrower archive.rs:594 selects nine named columns from cold.concepts and writes them back hot. Concepts only — nothing reads cold.links, so there is no link rehydrate to fix. One column list, symmetric with the writer's.
3	The fold's PARTITION BY	much larger The fold does not splice LATEST_BELIEF_PROJECTION at all — that constant projects links_current, a different thing. Replay has four fold constants (HOT_FOLD, COLD_FOLD, ANCHORED_HOT_FOLD, ANCHORED_COLD_FOLD), each partitioning by (table_name, entity_id). And trg_links_log_insert composes entity_id as source|target|type|valid_from — no recorded_at, no lineage. See below: this is not case (d), it is the central case.
4	Enumerate cold touchpoints	closed set replay.rs:95,133 (the two cold folds) and archive.rs — three CREATE, three writes, the archivability check, rehydrate's read and its delete. Nothing else reads cold.; the §3.2 AtTime path goes through replay, so it is covered by the folds. The two named readers are the complete set.
5	New cold files v12-shaped by the same DDL?	no — separate, and deliberately The cold CREATEs are hand-written in archive.rs, trigger-free and FK-free on purpose, because cold.links cannot satisfy a foreign key into a concepts table that holds only what went cold. So this is D-035's two-descriptions shape already accepted with a reason — not a new trap. Consequence: all three cold CREATEs need branch_id added by hand, and the rung test asserts the cold shape carries every column the hot writer sends.
The finding that changes the rung
Two lineages asserting the same edge produce the same entity_id, because the log trigger composes it from source|target|edge_type|valid_from and stops. All four folds then partition on (table_name, entity_id) and take the highest seq_id — so one lineage's belief is erased at replay, silently.

This is not an edge case to be caught by a test for completeness. It is what happens the first time a branch supersedes an inherited edge, which is the primary thing a branch does. reconstruct() would return the trunk's belief or the branch's depending on write order, with nothing red anywhere.

The fix belongs in v12 even though it is unreachable until fork(): transaction_log gains branch_id in step 3, so all four folds project it and partition on (table_name, entity_id, branch_id), and pre-v12 rows default to 'main'. Changing entity_id's composition in the trigger instead was rejected: it re-keys the log, and old rows would not match new ones. Replay's cardinality becomes per-(entity, lineage), which is a caller contract and belongs in D-217 rather than being discovered at 0.14.5.

Reclassified — and it is what makes the fold fix work
Filed under overlay inheritance, wrong shelf. Walk it at v12 with no overlay anywhere: transaction_log gains branch_id DEFAULT 'main', the log trigger's INSERT does not name the column, and it takes the default. Probe §7 accepts a same-lineage update on a branch-b concept — the guards refuse cross-lineage and immutability, not this — so the branch's own history lands stamped 'main'. A silent restamp on the ledger itself, reachable by raw SQL today and by the API at 0.14.5.

The grep you asked for, run: three production log triggers, all with the same five-column list, none naming branch_id — trg_concepts_log_insert (the insert-side twin, and it exists), trg_concepts_log_update, and trg_links_log_insert. A fourth writer, CONCEPTS_LOG_INSERT_V9 in migrations.rs, is a pinned historical body and must not gain the column: it reconstructs a v9-shaped database, where the column does not exist. Two descriptions, and here the second one is correct.

The coupling that promotes this above "one more item": the fold widening decided last round partitions on transaction_log.branch_id. If every log row reads 'main', that partition discriminates nothing — the fold fix is inert without this one. They are one repair in two files, not two items.

Three consequences already scheduled break on it, all of them quietly: 0.14.6's abandonment predicate ("contiguous by construction" is true only if every writer stamped honestly — a 'main'-stamped branch row survives the abandonment sweep, attributed to trunk); replay, which folds the row into the wrong lineage's history; and diff's ground truth, which mis-attributes the write.

Your three refinements, taken
Python at 0.14.4 verifies nothing as written — correct, and it is my own no-op argument pointed at my own remediation. The pinning fixture gets ported: raw-SQL branch row, assert today's return, comment to flip. Cheap, and it is the only thing that makes the Python release note true.
W13 inherits a W6 question. F-34's framing is language-as-deliverable, so a Rust-only read layer is a binding gap opened in the release that created the feature — W6's exact finding. Python parity for the read layer is decided at 0.14.5's design or the layer is not taken.
Price the trigger with W4.5's alternating arms. Two seeks beside four existing checks is precisely the effect size D-090's session noise certified wrongly once. Alternating arms, not sequential runs.
Decision
D-214
The model: entities don't fork, belief does
D-022's "concepts are entities, not intervals" is the framing: the belief ledger is links + transaction_log, and those branch cleanly. Under A, branch_id on concepts is provenance — which lineage minted it — which is what diff() and the branch-scoped archive predicate need, so the column earns its place without carrying identity.

A — Links branch, concepts don't
taken
Keep UNIQUE(id) and the declared foreign keys. A branch mints new concepts and branches links freely; it cannot retitle, correct, or retire an inherited concept.

Buys
Additive rung. D-036 untouched. Every §15.5 requirement met. Embeddings stay keyed to identity and text, so D-005/D-037's per-model tables, DiskANN and all four search surfaces are branch-invariant.
Reversible
Uniquely so. A → overlay is a new table: additive, permitted after 1.0.
Also
diff is cheap by construction, not by optimisation: divergence is the symmetric difference of own-id row sets, and the ancestor prefix cancels.
B — Overlay table for divergent concepts
deferred, not rejected
Two stores of the current title is D-035's failure shape at table scale: a description of a set can drift from the set; a derivation cannot. Reopens when a use case needs branch-local retirement or correction of an inherited concept — and it arrives with measurements, which is the right order. Not blocked by 1.0.

C — Widen uniqueness, drop the declared FKs
disqualified
Removes the crate's oldest engine-enforced invariant on a promise D-018, D-029 and D-019 have each rejected once, and rebuilds a frozen ledger table to do it. C-1 becomes unanswerable. Probe §3 shows the failure would land at write time, not schema time.

Semantics to state, not discover
The cross-lineage guard is exact-branch equality, not ancestry. A fork cannot update its parent's concepts, only mint new ids. This matches fork-as-new-writer, keeps the trigger non-recursive, and loosening it later is a cheap trigger rung — triggers are not the frozen shape.
Concept merge is structurally a no-op under A — reads are unfiltered, so a branch-minted concept is already globally visible and what merges is links. From 0.14.4 the sentence becomes "a no-op for global-scope concepts", which belongs in the same entry so the two readings never circulate separately.
CHECK (forked_at IS NULL OR forked_at <= created_at) is row-local and expressible. The cross-row invariant — a child's cut lying inside its parent's lived interval — is not, because CHECK cannot subquery. D-034 is its home; say so in the entry so nobody goes looking for it in the schema.
Overlay
option B
the ledger
The overlay, on its merits
Not "should A be reopened" — it should not, and the reasons are below. The question worth answering is what the overlay actually buys, what it actually costs, and what would make it worth paying, written down once so the answer is not re-derived by whoever meets the question next.

What it is
concepts keeps UNIQUE(id) and stays the foreign-key parent. A new table holds only what a branch has diverged:

CREATE TABLE concept_versions (
    id               TEXT NOT NULL REFERENCES concepts(id),
    branch_id        TEXT NOT NULL REFERENCES branches(branch_id),
    title            TEXT NOT NULL,
    content          TEXT NOT NULL DEFAULT '',
    embedding_model  TEXT,
    valid_from       TEXT NOT NULL,
    valid_to         TEXT NOT NULL,
    retired          INTEGER NOT NULL DEFAULT 0,
    recorded_at      TEXT NOT NULL,
    PRIMARY KEY (id, branch_id)
);
A read on branch B resolves the overlay along B's ancestry — nearest ancestor wins — and falls back to concepts. An empty overlay means the branch reads exactly its parent, so the fork stays O(1) and the storage cost is proportional to divergence rather than to history. That is Gancarski's separating logical database versions from physical storage applied to the one table that cannot carry a lineage column.

Strengths
real, not rhetorical
It is the only design that makes belief genuinely divergent on concepts. Under A, retired is one shared flag and a title is one shared string. A branch cannot correct or retire a concept it did not mint. That is a hole in "two divergent lineages of belief", and naming it as deferred does not close it.
The foreign keys survive. concepts.id stays UNIQUE and stays the parent, so probe §3 and §4's failures never arise. The overlay's own FK to concepts(id) is single-column and works.
It is additive to A, not a replacement. concepts.branch_id stays provenance; the overlay adds state beside it. No shipped semantics migrate, no rung rebuilds a frozen table.
It has no deadline — in the schema. A new table is additive periphery under D-036, so no compatibility window closes on it. The machinery is a different claim and the ledger should not overstate it: by then the overlay must be retrofitted into the scope trigger's equality, the reader gate (a second arm for FROM concept_versions), and the surface registry — 0.14.x artifacts that do not exist yet. Later costs somewhat more, not categorically more. That is still the argument for spending the pre-1.0 window elsewhere; it is not the argument that it is free.
Weaknesses
five, four structural
It creates a derivative-vs-source pair where none existed. There is no audit_current for concepts because there is nothing for concepts to drift from. The overlay makes "the current title" a resolution over two tables, and D-030's rule then demands an audit that can fail — new machinery whose absence would be worse than the drift it fails to catch.
It multiplies the remembered predicates, which is the one thing the matrix exists to minimise. Today visible_concept is one predicate over concepts AS c. Under the overlay c stops being a table and becomes a resolution, so every splice site needs a CTE — and traversal hydration becomes a fourth site, the exact one Option A plus the scope trigger was built to avoid needing. And the read-side cost has a write-side twin: "which lineage holds X's current state" stops equalling concepts.branch_id, so every equality laid down across 0.14.x splits into mint-lineage versus effective-lineage — the scope trigger's plain equality, the cross-lineage guard's exact-branch rule, visible_concept's membership test.
The FTS coupling breaks, and both exits are bad. concepts_fts is external-content keyed on concepts.rowid_pk and maintained by triggers on that table. An overlay row has no row there. Either there is a second FTS index and a fusion step — a second description of the same text — or the single-index exit, which is worse than "unfindable" makes it sound: the trunk's entry still matches. The id comes back scored on text the caller cannot see, the matched term may not appear in the title the caller is shown, and ranking is computed on superseded content. Findable-by-the-wrong-thing is quieter than unfindable, which makes it the half that actually hurts.
Embeddings go stale per lineage with no place to say so. D-005/D-037's per-model tables key on concept_id, and the ANN index is shared. A branch that corrects a concept's content retrieves on the trunk's vector. Under A this cannot arise because content cannot diverge; under the overlay it is a correctness gap, and the only clean answer — per-branch embeddings — is the cost D-005 refused.
Struck — this one was the overlay's only by mis-filing. The concept log's lineage-blind stamping is a v12 defect that exists with no overlay anywhere, and the rung now fixes it. Charging it to B double-counted it and inflated the case against a design that should be refused on its real costs. Five weaknesses, not six, and the remaining five stand.
Archive and rehydrate need a fourth shape. cold.concepts is keyed id TEXT PRIMARY KEY and rehydrate selects WHERE id = ?1. Per-lineage concept state makes both ambiguous, so the cold file grows a table and the archivability predicate becomes a cross-lineage question.
Option A	A + overlay
Branch mints new concepts	yes	yes
Branch supersedes inherited edges	yes	yes
Branch retires its own concepts	yes — same lineage, guard permits	yes
Branch corrects or retires an inherited concept	no	yes — the entire delta
Concept-reading surfaces needing a predicate	3 splice sites	4+, each a resolution
Second description of concept state	none	yes — D-035's shape
Keyword search sees branch-corrected text	n/a	only with a second FTS index
Vector search sees branch-corrected text	n/a	no — shared ANN index
Cost after 1.0	—	unchanged — additive
The reopen trigger, stated so it can be recognised
The overlay becomes worth its cost when a caller needs branch-local correction or retirement of a concept the branch did not mint — and not before. §15.5's conversation tree does not: forks add turns, and a branch retires its own turns under A already. The recognisable symptom is a caller working around the gap — minting a near-duplicate concept to carry a corrected title, which is the same information in two ids and exactly the dedup problem the scoped default was chosen to keep visible.

Retirement has its own symptom, and it is not the near-duplicate. The near-duplicate catches correction; retirement shows up as a caller asking to silence an inherited concept in one lineage and being refused — and as its zero-sum converse, a branch retiring a concept it did mint and silencing it for every lineage at once. Either one, heard twice, is the trigger.

Three things must arrive with it and not after: an audit that can fail over the resolution; a decision about the search surfaces; and the monotonicity rule — whether a branch's recorded_at must advance past its own last overlay stamp, the trunk's, or both. Pick wrong and a branch silently rolls its own belief backward, which is D-018's reason arriving a second time. Shipping without those buys divergent state and gives up the property that made A worth choosing.

The answer to "include it?"
No, and the reason is not that it is a bad design. It is a good design whose cost is paid in exactly the currency this plan is short of — remembered predicates, second descriptions, and surfaces that must each be told about lineage — and whose benefit is a capability nothing has asked for. A has the fewest places to be wrong; the overlay adds several, each of them the kind the register has a name for.

What changes is that it is now specified rather than gestured at. D-214 carries the table, the resolution rule, the five weaknesses, the three arrival requirements, and both reopen symptoms, so the next reader inherits a design decision instead of an open question. That is the whole benefit of writing it down and none of the cost of building it.

Corrections
held to code
Five places the first plan and the code disagreed
Assumed	What the code says	Effect
Audit and rebuild are separate sites; atomic and chunked variants	One rebuild (rebuild_current wraps rebuild_within), and both it and audit_current splice the same LATEST_BELIEF_PROJECTION.	smaller One constant. D-035 already paid for this.
"both CTEs must project branch_id"	The load-bearing edit is the PARTITION BY. Without it, latest-belief is chosen across lineages.	sharper Two branches collapse into one row — silent wrong answer, not drift.
The cold fold "must tolerate" a v11 shape	D-026 folds UNION ALL through one identical window query.	harder Arity mismatch. Required, not a nicety.
The predicate is distributed across six surfaces	visible_concept (vector/search.rs:157) is already the single predicate, spliced into search_vector, keyword_search and the pre-filter, bound to alias c.	smaller One definition, three splice sites. D-191 built §15.4's hook.
links keeps its 5-column PK — probably safe	D-025: now() returns max(wall, last_issued + 1µs), floored from MAX(recorded_at) at construction.	confirmed Unreachable, not unlikely. State the mechanism.
Both trigger catches confirmed verbatim: trg_links_single_open's EXISTS has no branch filter, and trg_links_current_sync inserts eight columns with conflict target (source_id, target_id, edge_type, valid_from). And §15.5 re-scanned: its five requirements are fork at every turn, read a leaf's lineage cheaply, abandon most branches, ask what a branch concluded, never corrupt the trunk. None implies branch-local concept state.

The rung
v11 → v12
0.14.2
What v12 contains
One BEGIN IMMEDIATE, stamp inside, per D-032. Name: branch-storage.

CREATE TABLE branches — branch_id TEXT PRIMARY KEY, parent_id TEXT REFERENCES branches(branch_id) (self-FK declarable at CREATE), forked_at and created_at with inline canonical CHECKs per D-029 plus the row-local ordering CHECK. forked_at is recorded_at-domain — the divergence instant §15.3's cutoffs are computed over — NULL for the root. Unconditional delete guard in D-008a's shape: branches are never archived, so no session makes deleting one legal.
Seed the root through one helper shared by the baseline path and the rung, with 'main' spliced from a single constant.
ADD COLUMN branch_id TEXT NOT NULL DEFAULT 'main' on concepts, links, transaction_log. Metadata-only (§1); triggers and FTS survive (§6).
Rebuild links_current with PK (source_id, target_id, edge_type, valid_from, branch_id) — branch_id last, so the autoindex keeps its leading columns and D-059's PK-vs-covering contest is unperturbed. Branch-leading composition is §15.3's measurement call, per F-33. Re-derive via rebuild_within; do not describe.
Widen LATEST_BELIEF_PROJECTION: branch_id in the select list and in the PARTITION BY. One edit makes both the audit and the rebuild branch-correct.
Redefine the three log triggers — trg_concepts_log_insert, trg_concepts_log_update, trg_links_log_insert — so the INSERT column list carries branch_id with value NEW.branch_id. DROP then CREATE, never a re-issue: CREATE TRIGGER IF NOT EXISTS on an existing name keeps the old body, which is the lesson CONCEPTS_GUARD_DELETE_V8's doc comment already records. CONCEPTS_LOG_INSERT_V9 stays untouched and gains a comment saying why — it is a pinned v9 body, and a v9 database has no such column.
Widen all four replay folds — HOT_FOLD, COLD_FOLD, ANCHORED_HOT_FOLD, ANCHORED_COLD_FOLD — to project branch_id and partition by (table_name, entity_id, branch_id). A separate step from the one above because it is a separate mechanism: that projects belief about edges, this folds the log. Without it two lineages' assertions about one edge collapse at replay, silently, the first time a branch supersedes an inherited edge.
Redefine two triggers — trg_links_current_sync (carry NEW.branch_id, extend the conflict target) and trg_links_single_open (scope the EXISTS). Row-level, not view-level: inherited-interval conflicts are §15.4's write-path question.
Two concepts guards — cross-lineage insert (exact-branch equality) and branch_id immutability, each with an ABORT_* constant classified at the one boundary per D-033, and a unit test asserting the constant appears in the trigger body. C-1 closes in the same edit: classify is already open, and the foreign-key arm D-176 carried is one more arm on a path that now has two. Plus a third guard on branches itself: refuse every UPDATE, not a column subset. Nothing on that row legitimately changes, and with no foreign key connecting ledger rows to their lineage record, one UPDATE branches SET branch_id = … orphans every row keyed to the old name with nothing going red. Renaming 'main' orphans the whole ledger in one statement; editing parent_id or forked_at re-derives visibility of stored rows without a new assertion, which is Doctrine III's forbidden move reachable by raw SQL.
The cold file, both directions. This is the amended step and the one with a hole in it before.
Write side — upgrade in place. The archive session already holds the write connection and a BEGIN IMMEDIATE. Where the column is absent, ALTER … ADD COLUMN branch_id TEXT NOT NULL DEFAULT 'main' on all three cold tables: cold.transaction_log, cold.links, and cold.concepts. Probe §12–13: the DDL is accepted inside the transaction and a rollback takes it with it, so a failed session leaves the cold file untouched.
New cold files. The three cold CREATEs are hand-written in archive.rs — trigger-free and FK-free on purpose — so they do not inherit the baseline's columns and each needs branch_id added explicitly.
Both movers, symmetrically. The archive writer carries the column out; rehydrate carries it back in. Its nine-column select from cold.concepts and the hot insert that follows both gain it, or every rehydrated row lands stamped 'main' — probe §11's arity error catches a wrong list, never a short one.
Read side — tolerate, never upgrade. D-026's fold and rehydrate's cold.concepts select project 'main' as a literal when the column is absent. A cold file can be read-only or shared; a read path that mutates it is a new failure class.
Detection is column-presence, not a stamp. Cold files get moved (D-026) and carry no version you can trust.
Consequence: every cold file touched by an archive after 0.14.2 becomes v12-shaped, so the read side's literal arm decays as archives happen and survives only for files never re-attached for a write. D-035's principle extends to lineage: the archive path structurally cannot drop it.
user_version = 12, inside the same transaction.
Deliberately absent
No new indexes. No query filters branch_id until §15.3. They arrive with registry entries and measurements, per W3.5.
No FK on the branch_id columns. D-034 is the home: fork() validates before the payload crosses the channel.
No write-path change. Every INSERT omits branch_id and takes the default — the proof the release is storage-model-only.
No same-lineage update rule. Leaving it schema-permitted means §15.4 tightens without another rung.
The rung test
Populated v11 fixture (W1.2) plus the labelled empty case (W2.4).
user_version == 12; branches holds exactly the root; every ledger row reads 'main'.
links_current after == latest-belief projection of links before, row for row, modulo the new column.
EXPLAIN QUERY PLAN on the core traversal, pinned (W3.5) — and its operation counts, which 0.14.0 made a gate rather than a print.
audit_current() == 0 on the migrated populated database.
Guards: cross-lineage upsert aborts through the upsert path, with a comment naming probe §7 as the engine behaviour it depends on; branch_id update aborts; new id under a non-main branch succeeds; a main upsert still works.
Raw-SQL assert into links under a non-main branch lands in links_current under that branch — proves both trigger redefinitions at once.
Cold, four cases: a v11 cold file folds without error; a branch row archived into a v11 cold file upgrades it and round-trips with lineage intact; a never-re-attached v11 file still reads through the literal arm; and — the one the other three cannot catch, because they are all single-lineage — a hot branch-B row and a cold main row for the same edge, folded, asserting two beliefs rather than a seq_id winner.
Honest stamping, both operations: a raw-SQL concept minted under 'b' and then updated same-lineage produces two log rows, both carrying 'b'; the same for an edge. This is the test that keeps 0.14.6's abandonment predicate honest two releases before it exists.
branches: every UPDATE aborts, including a no-op one; DELETE aborts outside an archive session and there is no session in which it is legal.
The cold shape carries every column the hot writer sends — asserted, not assumed, since the two descriptions are deliberately separate.
Expect red, and update deliberately
compat_contract_tests' table_info pins go red on four tables — the change detector working, and that test already proved ADD COLUMN-with-triggers possible, so the rung is that test promoted to production. The public API gate stays at 1,313 items; if it moves, something leaked out of a storage-only release.

Probe
dependency map
What the rung depends on, and what is only evidence
An example is not a gate. The probe lands with 0.14.2 as D-214's evidence — D-205's rule, since a register entry citing a file that does not exist is the inverse of a re-runnable review — but only three of its findings are load-bearing, and those need something that fails if the engine changes.

§	Finding	Depended on?	Where it lives after 0.14.2
1	ADD COLUMN metadata-only	no	Example only. Nothing breaks if it becomes O(rows) — the upgrade just costs more.
3, 4	FK shapes under widened uniqueness	no	Example only — evidence against alternatives not taken, cited by D-214.
6	Triggers and FTS survive ADD COLUMN	yes	Rung-test guard cases and the compat pins.
7	BEFORE INSERT precedes ON CONFLICT	critically	The guard is dead without it, and silently: if libSQL ever reorders, the abort disappears and the foreign upsert succeeds. The rung test's upsert-path abort covers it; the work is naming the dependency in the comment.
9	Lineage as a predicate subquery	yes, later	0.14.3's measurement fixture and 0.14.4's matrix-test predicate.
13	ROLLBACK undoes DDL on an attached file	critically	Step 8's atomicity claim. If it stopped holding, a failed archive would leave a half-upgraded cold file. Rung test's cold cases cover it.
Sequencing
the reorder
Visibility precedes fork(), and costs nothing
The dependency runs opposite to the intuitive order, and the justification is stronger than "it is owed".

On every database this crate's API can produce at 0.14.4, only main exists — so lineage('main') = {'main'} and the predicate passes every row.
The predicate lands at the one moment it is observationally silent, and fork() then ships into a surface whose default was never wrong in a released version. Ship it the other way round and changing a read default afterwards is a semantic break on a shipped surface — which is what D-160 → D-174 spent a release doing properly. The designed exception is the 0.14.2 pinning test, which creates a branch row by raw SQL precisely so the flip is loud.

What does not have to precede fork() is the scope column: lineage-scoped-by-default already excludes siblings and descendants, which is the entire leak population. scope only adds deliberate global minting, so it lands with the code that reads it.

Two interim semantics to document, not fix
Lineage includes ancestors' post-fork mints. A branch sees things main minted after the fork. That is shared-vocabulary semantics and the right default, but it is a decision to record. A strict fork-point cut needs a creation instant on concepts, which the table does not carry — creation lives in transaction_log. Do not add one unless the loose semantics measurably hurts.
Between 0.14.5 and 0.14.6, search visibility and edge visibility disagree for branch-minted concepts linked across lineages: the trunk's search will not show a branch's concept, but a trunk edge to it hydrates fine. Not closable in code — the information to close it (scope) does not exist yet — and acceptable because it is documented interim semantics resolved one release later, not a silent leak. It goes in 0.14.5's notes and comes out of the carried limits when the trigger ships.
Plan
0.14.2 → 0.15.0
Seven releases and the acceptance
0.14.2
W12.2 · schema v12
The storage model
The nine-step rung, its test, and the probe checked in with its dependency map in the header. C-1 closes here, in the same classify edit. Plus the two cheap items from the search analysis while the ground is open: the leak probe (raw-SQL spike branch, all four surfaces, matrix recorded) and the leak-pinning test asserting the sibling concept is returned today, commented to flip at the predicate.

D-214 model
D-215 links PK
D-216 derivative carve-out
D-217 cold shape
no API change
0.14.3
W12.3
Ancestry resolution, and the measurement that picks the strategy
The ancestry walk built once, consumed twice — edge reads and concept visibility. §15.3's deliverable: depth-3 traversal on chains of 1, 10 and 100 branches against the same fixture unbranched. The fixture needs post-fork writes on main: a chain over a frozen trunk measures the walk's depth cost and never the cutoff's selectivity, which is the cost that matters. The branch_id index arrives here with its registry entry and measured justification. Strategy (1) confirmed or escalated to (2) on the numbers, (3) rejected on evidence.

F-33 discipline
W3.5 index gate
0.14.4
W12.4
The visibility predicate
The lineage clause enters visible_concept — one definition, three splice sites, one new bound parameter. Default scoped, global() the named opt-out, per D-155's silent-vs-visible rule. The matrix test extends F-31's fixture by one axis: retired × valid-time × lineage, every surface against every cell. The 0.14.2 pin goes red and is updated deliberately. Over-fetch recall measured; predicate placed before hybrid fusion. No index — a named deferral, because on every database that exists here the filter discriminates nothing. Python's pinning fixture ported, not its search assertions — single-lineage fixtures make scoped and global identical, so only a raw-SQL branch row verifies anything.

And the reader gate. A test walks the SQL-bearing sources and requires every FROM concepts and JOIN concepts to be either inside visible_concept's composition or named in a registered visibility-surface list with a reason. D-038's name-checking, applied to source rather than to the register. It does not make the invariant structural — nothing available here can — but it converts the failure mode from a reader was written without the predicate and nobody noticed into a reader was added and the build said so. The register grows by one line when the answer is "this one is deliberately unscoped".

Two entries go in with the gate rather than being discovered by it: rehydrate and the cold folds read cold.concepts as movers, not as caller-facing visibility surfaces — they carry rows between files and must see every row, scoped or not. Exempt with a reason, never silently outside the walk. And the gate's own comment records that the overlay, if it lands, grows it a second arm for concept_versions — the gate sits on the ledger's side of that deferral too.

D-155 default rule
semantics move, pre-API
reader gate
0.14.5
W12.5 · W13 decided
The API, both languages — and F-34 answered
Database::fork(name, from) -> BranchId, branches(), and the branch view as a separate Clone type over Arc<Database> + BranchId — never a Database with a field, per D-203: a view must not carry the right to close(). Write-path policy under D-034. Merge and cross-branch traversal recorded as refused and tested. Python parity in the same release, per W6.

W13's decision is taken at this release's design, before the API shape is fixed — that is when the count of builders taking a branch parameter is decided, which is F-34's own evidence. Two sanctioned exits, and deciding nothing is not one of them.

surface grows — first move since the freeze
F-34 → build or §19
0.14.6
W12.6 · schema v13
diff, abandonment, and the scope column
diff(a, b), cheap by construction. The branch-aware archive arm — an abandoned branch's rows are contiguous by construction, the cheapest archive predicate in the crate, and the half of §15.5 an API-only cut would have shipped without. scope lands as its own rung with the code that reads it, per W5.1, and with the write-side trigger that closes the merge channel — priced first, since it is two seeks per links insert on a path that already carries four. Python parity.

Parked here, and worth stating so it is not found by accident: a cold file carries branch-stamped rows but no branches rows. It records that a row belonged to lineage b and nothing about what b was — no parent, no fork point. Attached to a database that has forgotten b, those rows are stamped with a name that resolves to nothing, and ancestry cannot place them. Harmless while archive is same-database; it is the abandonment arm in this very release that makes forgetting a branch a normal operation. Either the cold file carries the lineage rows it references, or unresolvable stamps are a defined and tested read outcome. Decided here, not later.

schema v13
write-path cost
0.14.7
W13 · conditional
The declarative read layer, if 0.14.5 took it
A thin declarative surface over the builders, closing F-34 while the combinatorial count is fresh. Its own release so §17 reviews a closed surface. If 0.14.5 cut it instead, this release does not exist and §19 gains the entry with its reason — §0's rule, which applies to cuts as to reversals.

exists or §19 does
0.15.0
§17 acceptance
Read as evidence, not ticked
§17's ten items plus W12's carries. C-2…C-4 closed or re-carried with stated reasons (C-1 closed at 0.14.2). The surface review against 0.14.0, cheap now that D-205's baseline exists and D-212's method is written down. F-34 placed either way. Then main, tagged.

D-212's method
Forks
answered
The four, closed
1 · 0.15.0 means W12 complete
and W13 placed
The decisive reason is the one the first draft underweighted: §15.5 lists "abandon most branches" as a requirement. An API-only cut ships forking without its exit, and branches that can be created but never abandoned accumulate in the hot file forever. The use case minus its lifecycle is not the use case. Two supporting reasons: a §17 surface review against a surface about to grow gets redone, and an acceptance section reading a wave in progress is what 0.13.x's revision row was corrected away from.

2 · Two rungs planned, three budgeted
deferrable
v13's shape is orthogonal to §15.3's outcome — scope is concepts-side audience, a hybrid materialisation is links_current-side periphery, D-036-rebuildable and post-1.0-addable. The only freeze-window-bound items in the plan are the additive columns on frozen tables, and both are already inside it. Nothing before 0.14.3's measurement constrains anything after it; the ladder composing steps is D-036's own rationale. Let the measurement add v14 if it must.

3 · The trigger ships with scope
and is priced first
It does more than close the merge channel: it keeps traversal isolation structural. Traversal hydration is a fourth splice site the predicate cannot be given for free — an edge whose target hydrates to nothing is defect Z's shape returning, and W4.8's debug_asserts on Subgraph::is_closed would turn it into an assertion failure rather than a silent one. The guard removes the question instead of answering it: no foreign edge can point at a lineage-scoped concept, so hydration never meets one. D-018/D-019's argument once more. Two seeks, not one — measure before assuming free.

4 · The probe lands with 0.14.2
with its dependency map
Fourteen sections, merged and green. A register entry citing a file that does not exist is D-205's principle inverted. The header carries which findings the rung depends on — §7 and §13 critically — so a future engine change has something that fails rather than something that quietly stops applying.

Limits
carried
What this plan does not fix
Perfect isolation is not available and the plan should stop implying it is. There is no row-level security in this engine and no parameterized view; a predicate spliced into a query string is enforced exactly as far as every query goes through the splicer. A branch is a coherence boundary — one lineage's belief kept whole — and not a security boundary, and the two want opposite remedies. Coherence wants in-process lineage predicates — this plan. Confidentiality wants file and process separation: a database file per tenant, OS permissions, no shared process. The single-file embedded shape makes that second remedy cheap if it is ever asked for, which is why "not a security boundary" here means a different mechanism entirely and not security arrives later in this plan. Sorted by what can be done about them:

Permanent — the ceiling, not a backlog
A raw-SQL caller bypasses visibility entirely. Triggers fire on writes; reads are unguarded. This is the same status retired = 0 has held since F-31 — lineage does not add exposure, it adds a second thing behind the same door.
Cold files never re-attached for a write stay v11-shaped indefinitely, read through the literal arm. There is no pass that finds them, because nothing knows where they are.
Over-fetch recall degrades where foreign-lineage content is dense near a query. Measurable, boundable, not removable without per-lineage indexes.
Scheduled — dated, with the release that pays
A branch cannot retire, correct, or retitle an inherited concept. Not a defect; the deliberate shape of A. The refusal error is the documentation, and the overlay is specified with its reopen trigger rather than left as an open question.
search_filtered stays safe by composition, not by decision until its candidate path is asserted in the matrix at 0.14.4. F-31's phrase, and F-31's exposure.
Cold files are not self-describing — decided at 0.14.6, beside the arm that makes forgetting a branch normal.
Fixable now — and taken
A new concept reader written without the predicate. Today nothing notices; the gate at 0.14.4 makes it a build failure or a registered exception. This is where the honest ceiling actually pays: given that bypass cannot be prevented, the thing worth buying is that accidental bypass cannot be quiet.
Silent belief erasure at replay, from four folds partitioning without lineage — fixed in the v12 rung, before anything can reach it.
branches rows editable in place, re-deriving visibility of stored rows with no new assertion — every UPDATE refused at 0.14.2.
None of this is a reason to revisit A. The overlay multiplies the surfaces that must remember lineage — the exact quantity the gate exists to bound — and C removes the structural guarantees that make the remaining exposure a single known door. A is the arrangement with the fewest places to be wrong, which is the property worth optimising when the ceiling is fixed.