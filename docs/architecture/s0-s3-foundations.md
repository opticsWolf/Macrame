<!--nav-->
[index](README.md) · [next](s4-schema.md) →
<!--/nav-->

## §0 Doctrine

Before any mechanism, this architecture is defined by eight invariants. Every design decision in this document derives from them; every code review against this codebase should begin by asking which of them a change touches. They are ordered roughly by cost of violation: the first three, if broken, corrupt data; the rest degrade the engineering properties of the system.

<a id="doctrine-i"></a>I. The boundary is sacred. Everything above the libSQL connection is ours — schema generation, query compilation, temporal logic, the API. Everything below it is upstream. We never patch the engine, never fork the C core, never depend on undocumented engine internals. If libSQL lacks a capability, we build around it above the line or carry the gap as an explicit risk. This doctrine is what keeps a two-person project from inheriting a C codebase.

<a id="doctrine-ii"></a>II. Two clocks, never mixed. Every row carries time on two independent axes: valid time (valid_from / valid_to — when a fact held in the world) and transaction time (recorded_at — when the database learned it). No trigger, no default, no code path may ever derive one from the other. valid_to is always supplied explicitly by a caller, or defaulted to clock.now() at the API layer only, where the choice is visible and overridable. Retroactive corrections ("we realize now this expired last week") are a first-class capability, not an accident the schema happens to permit.

<a id="doctrine-iii"></a>III. Assertions are immutable. Rows in links are assertions — statements of belief about an interval — and are never updated in place. Changing belief means inserting a new assertion with a fresh recorded_at. The past is never rewritten; it is only ever superseded. This is what makes the transaction-time axis honest.

<a id="doctrine-iv"></a>IV. The ledger is a table, not the log. Transaction-time reconstruction reads exactly one structure: transaction_log, an append-only table captured by engine triggers. We do not read libSQL's WAL, replication frames, or any CDC facility. Physical logs are truncated and checkpointed by the engine for its own purposes; a ledger whose history can be compacted away beneath it is not a ledger.

<a id="doctrine-v"></a>V. No physical deletion in hot tables. Rows leave the hot database only through the archive path, which runs inside a declared archive session and is verified before anything is removed. An ad-hoc DELETE issued from any other client aborts at the trigger layer. Absence of data must always be explained by the ledger, never caused by a mistake.

<a id="doctrine-vi"></a>VI. Derivative state is disposable. links_current is a materialization — a cache of current belief — and is rebuildable from links at any moment by a single deterministic query. Because it can be rebuilt, it can be trusted: drift is detectable by audit, recoverable by rebuild, and the roundtrip is tested. Any structure added to this system in the future must either be source-of-truth (append-only, immutable) or rebuildable (auditable, disposable). There is no third category.

<a id="doctrine-vii"></a>VII. Embeddings are immutable per version and excluded from the ledger. A vector is a derived artifact of a specific model applied to specific content. It never appears in transaction_log payloads; it lives in per-model tables so that a model migration can never produce a row whose dimension violates its type. If an embedding changes, the concept gets a new version of its vector, in a table named for its model.

<a id="doctrine-viii"></a>VIII. Fidelity is a parameter, never a silent default. Queries that mix time axes say so in their signatures. as_of_valid(ts) means valid time under current belief and returns exactly that; as_of_recorded(ts) and reconstruct(ts) mean belief as of ts and read exactly that; setting both valid and recorded asks what we believed then about what was true then. The gap between the axes — retroactive assertions made after ts — is documented, pinned by tests, and surfaced at the type level. A caller should never receive yesterday's graph with today's text and assume it is history. (Through 0.13.1 a single as_of(ts) carried both clocks and this doctrine was therefore stated and not met; [D-174](s13-decision-register.md#d-174) split it in 0.13.2.)

Amendment 0.4.5 adds no invariants and retires none. Its single subject is the mechanism behind one sentence of [§2](s0-s3-foundations.md#2-system-context) — who holds the pen, and for how long. The Write Actor exists so that III (immutability) and VI (disposable derivatives) are never stressed by a background process holding the write lock for seconds at a time, and so that VIII's discipline extends to the write path itself: even when the database learned something is a fidelity question a caller can answer. It is a mechanism in service of the doctrine, and if it ever ceases to serve them, it is the mechanism that gets changed.

Releases 0.5.0, 0.5.1, and 0.5.2 likewise add no invariants. They are corrective and clarifying releases: the doctrine was sound; the DDL and prose that encode it were not. Every change in 0.5.x makes the normative text match the doctrine it claims to implement.

## §1 Purpose and Scope

Macrame is a domain-specific embedded database layer for a knowledge-ledger application: a system in which concepts are linked by typed, weighted relationships, both concepts and relationships change over time, and the history of those changes is itself a first-class asset. It is delivered as a single Rust crate that an application links directly; the entire database is one file on the local Windows filesystem, opened in-process, with no server, no network protocol, and no external service of any kind.

The crate provides five capabilities that are normally assembled from separate systems, and provides them as one coherent whole: graph storage and traversal over relational edge tables, compiled from a typed builder into recursive CTEs that the application never sees; bitemporal semantics in which every fact carries both the interval during which it held in the world and the moment the database recorded it, with the integrity of both axes enforced by engine triggers rather than application discipline; native vector search via libSQL's F32_BLOB type and DiskANN indexes, including hybrid keyword-plus-semantic retrieval; in-memory graph analytics through petgraph, for algorithms — community detection, strongly connected components, A\* search — that SQL expresses poorly; and point-in-time reconstruction, the ability to materialize the database as it was believed at any past instant, from an append-only log that the crate itself owns.

Two semantic operations anchor the temporal model, and the distinction between them runs through the entire design. as_of_valid(ts) is a valid-time question answered under current belief: it reports what the world looked like at ts given everything we know now, including corrections recorded after ts. reconstruct(ts) is a transaction-time question: it replays the log and reports what the database actually believed at ts, before later corrections arrived. The first is a filtered read of live tables and is cheap; the second is a fold over history and costs what history costs. Both are correct answers — to different questions — and this document treats conflating them as a defect.

Since 0.13.2 a third thing is expressible, and it is the one a bitemporal database is defined by: as_of_recorded(ts) makes the fold reachable from a *traversal* rather than only from reconstruct's whole-state replay, so the two axes can be set on one query ([D-174](s13-decision-register.md#d-174)). Setting both answers *what did we believe at r about what was true at v* — Jensen and Snodgrass's BCDM cell, and the thing the crate could not ask through 0.13.1 because the two axes were reached by two mechanisms that did not compose.

Explicitly out of scope: forking or patching libSQL; multi-process or client-server access; a standalone query language (the API is Rust); GPL-licensed components of any kind; and true continuous valid-time versioning of concept attributes in the live tables — that capability is provided instead by log hydration ([§5.2](s5-modules.md#52-graphbuilderrs--traversal-valid-time-and-attribute-fidelity)) on demand, for reasons examined in [§4.4](s4-schema.md#44-asymmetric-versioning-deliberately).

## §2 System Context
```text

+--------------------------------------------------------------+
|                  Application (Rust, async)                    |
+-----------------------------+--------------------------------+
                              |  typed API — no SQL visible;
                              |  valid_to always explicit or
                              |  API-defaulted
+-----------------------------v--------------------------------+
|                         macrame crate                         |
|                                                               |
|  +----------+ +----------+ +-----------+ +--------+           |
|  | schema/  | | graph/   | | temporal/ | | vector/|           |
|  | DDL gen  | | CTE gen  | | as_of     | | DiskANN|           |
|  | triggers | | vector_  | | replay    | | hybrid |           |
|  | migratns | | filter   | | snapshot  | | RRF    |           |
|  |          | | algorthm | | archive   | |        |           |
|  +----+-----+ +----+-----+ +-----+-----+ +---+----+           |
|       +-------------+------+-------+-----------+              |
|                     |                                         |
|              connection.rs                                    |
|    (pragmas, clock, WRITE ACTOR, priority channels)           |
|  one write connection inside the actor task — the only one;   |
|  read connections outside it, never crossing the actor        |
+-----------------------------+---------------------------------+
                              |  libsql crate — dependency,
                              |  never a fork
+-----------------------------v---------------------------------+
|               libSQL engine (MIT, unmodified)                 |
|   SQLite core · DiskANN · F32_BLOB · JSON1 · window fns      |
|   +-------------------------------------------------------+   |
|   | transaction_log — append-only, trigger-captured        |   |
|   | the sole transaction-time mechanism (Doctrine IV)      |   |
|   +-------------------------------------------------------+   |
+-----------------------------+---------------------------------+
                              |
  macrame_knowledge.db            macrame_knowledge_archive.db
  hot: open intervals,            cold: closed intervals,
  current belief, recent log      superseded log history
```

The horizontal line through the middle of this diagram is the most important boundary in the project. Above it, everything is owned by this crate and written in safe Rust: the schema and its triggers, the compilers that turn builder calls into SQL, the temporal machinery, the API. Below it sits a battle-tested MIT-licensed engine whose internals we treat as opaque. The architecture deliberately exploits libSQL's distinguishing features — the native vector type with its auto-maintained DiskANN index, window functions, JSON functions, ATTACH, the user_version migration hook — while depending on none of its unstable features: no CDC consumption, no replication internals, no experimental pragmas. When libSQL releases a breaking 0.x change, the blast radius is confined to connection.rs and the pinned version in Cargo.toml.

The concurrency model follows the embedded profile, amended in 0.4.5. One process owns the file; within it, one writer and many readers coexist under WAL journaling. As of 0.4.5, the writer is not an entry point but a task: the sole write-capable connection lives inside a dedicated Tokio task (the Write Actor, [§5.1](s5-modules.md#51-connectionrs--the-handle-the-pragmas-and-the-write-actor)), and every other part of the system — UI handlers, analytics workers, the archive scheduler — addresses it through a two-tier priority channel. Write serialization is structural in the strongest sense: no other task holds a connection that can write, so no amount of API misuse can produce a second writer. As of 0.5.0, this guarantee is reinforced at the engine level: the read connection carries PRAGMA query_only = ON, converting the Rust-ownership invariant into a runtime enforcement that survives even a logic error in connection routing. Readers never block on the writer, and never traverse the actor at all. Background writers are bounded by cooperative chunking: no low-priority transaction may hold the lock longer than one chunk, and the chunk sizes are per-path constants solved against a 3 ms duration bound rather than one row range — 90 edges, 70 concepts, 600 annotations, 30 embeddings ([D-058](s13-decision-register.md#d-058)). The worst-case latency of a UI assertion is one chunk commit, not one bulk job. This paragraph said "500–1,000-row" until 0.10.0, which was the 0.4.5 estimate the measurement replaced. This is deliberately less ambitious than a server database, and deliberately sufficient: a desktop knowledge ledger does not need distributed consensus; it needs to never lose a fact — and to never make the user wait for one.

## §3 Crate Layout
```text

macrame/
├── Cargo.toml
├── src/
│   ├── lib.rs                  # public re-exports, prelude
│   ├── error.rs                # DbError (thiserror) — §7
│   ├── connection.rs           # Database handle, pragmas, Write Actor, priority channels, clock injection
│   ├── schema/
│   │   ├── mod.rs
│   │   ├── ddl.rs              # all DDL as reviewed const strings
│   │   ├── migrations.rs       # user_version-driven runner
│   │   └── seed.rs             # optional bootstrap
│   ├── graph/
│   │   ├── mod.rs
│   │   ├── builder.rs          # Traversal builder → CTE; AttributeMode hydration
│   │   ├── edge.rs             # assert / retire / re-assert lifecycle
│   │   ├── vector_filter.rs    # §5.3 — strategies, cost model
│   │   ├── subgraph.rs         # DB → Subgraph loader, byte budget, chunked write-back
│   │   └── algorithms.rs       # dijkstra · astar · scc · k_core · louvain
│   ├── temporal/
│   │   ├── mod.rs
│   │   ├── interval.rs         # Interval, overlap arithmetic
│   │   ├── as_of.rs            # valid-time filters under current belief
│   │   ├── replay.rs           # window-function reconstruction; cold-DB ATTACH (§5.5)
│   │   ├── snapshot.rs         # bincode + zstd snapshots, seq-anchored, sidecar files
│   │   │                       #   (renamed from checkpoint.rs in 0.5.0)
│   │   └── archive.rs          # ATTACH-based cold storage
│   ├── vector/
│   │   ├── mod.rs
│   │   ├── embedding.rs        # Vec ↔ F32_BLOB codec, per-model dims
│   │   ├── model.rs            # §5.9 — ModelName newtype (validated identifier, D-037)
│   │   ├── registry.rs         # §5.9 — register_model, declared_dimension
│   │   ├── search.rs           # §5.9 — top-k, RRF fusion
│   │   └── hybrid.rs           # §5.9 — hybrid vector + FTS, RRF (D-051)
│   ├── integrity/
│   │   ├── mod.rs              # LATEST_BELIEF_PROJECTION — the single definition (D-077)
│   │   ├── audit.rs            # audit_current() — read-side
│   │   ├── rebuild.rs          # rebuild_current() — high-priority command
│   │   └── shadow.rs           # §5.8 — ShadowStep/ShadowOutcome, the chunked
│   │                           #   shadow-swap rebuild (0.6.0, D-082)
│   ├── metrics.rs              # §5.10 — actor hold-time histogram, behind
│   │                           #   --features metrics (0.6.0, D-079)
│   └── util/
│       ├── ids.rs              # ULID generation & validation
│       ├── timestamp.rs        # §5.11 — the canonical form, normalize/parse,
│       │                       #   OPEN_SENTINEL (D-029)
│       ├── limits.rs           # §5.11 — engine ceilings, not tuning choices:
│       │                       #   HYDRATE_CHUNK under SQLITE_MAX_VARIABLE_NUMBER
│       └── clock.rs            # Clock trait; SystemClock (monotonic floor + strict parser),
│                               #   FakeClock (Send + Sync interior)
├── tests/                      # 28 integration targets; the shape, not the list
│   ├── common/                 #   harness.rs (temp-dir fixtures, FakeClock wiring),
│   │                           #   fixtures.rs (the D-088 shape matrix)
│   ├── graph_tests.rs          #   …and temporal_, vector_, integrity_,
│   │                           #   concurrency_, migration_, write_path_, …
│   ├── *_property_tests.rs     #   quarantined behind --features property-tests (R15)
│   └── doc_sync_tests.rs       #   the gates: doc_link_, index_plan_, perf_claim_,
│                               #   fixture_matrix_, packaging_ (D-089, D-139)
├── benches/budgets.rs          # §9, criterion, every group carries control/select_1 (D-090)
├── examples/                   # diagnostics that are not tests — r15_soak, readonly_open_probe
├── bindings/python/src/        # §14 — pyo3 0.29, maturin; the wheel `macrame-db`
│   ├── lib.rs                  #   module init and the exported surface
│   ├── database.rs             #   the handle: frozen pyclass over RwLock<Option<Database>>
│   ├── errors.rs               #   wildcard-free match over DbError (D-099)
│   └── graph.rs, temporal.rs, vector.rs, rows.rs, types.rs, …
├── python/macrame/             # the Python-side package: __init__.py, _macrame.pyi
└── tests_py/                   # pytest suite; probes/ holds the R15 reproducers (D-107)
```

**The `tests/` and `bindings/` entries are shapes rather than inventories**, and deliberately so after 0.10.0 (W4.7): this tree listed five test files for years while the suite grew to twenty-eight, and a list that is wrong is worse than a shape that is right. `python scripts/run_rust_suite.py` enumerates the real set.

Each module owns one concern and one failure mode. graph/builder.rs is the only place SQL strings for traversal are constructed; temporal/replay.rs is the only place the log is folded; integrity/ is the only place links_current is written outside the trigger path; and connection.rs is the only place a write-capable connection exists. This concentration is deliberate: when a class of bug has exactly one address, it can be tested there and nowhere else needs to worry.

