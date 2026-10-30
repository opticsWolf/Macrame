# Benchmarks: TPC-BiH-style temporal workload

Adapted-workload benchmarking for this ledger. Not a literal TPC-BiH port — a
relational schema plus temporal SQL does not map onto a graph-ledger API — but
the benchmark's query *classes*, implemented natively and run per lineage:

| Class (TPCTC'13) | Coverage here |
|---|---|
| T: timeslice | T1/T2 history-shape contrast, T2 point slices, T4 TOP-N changed, T5 history yardstick, T6 slice-vs-point both directions |
| K: pure-key audit | K1 full history, K2 +range, K4 TOP-N, K5 preceding version, K6 selectivity sweep |
| R: range-timeslice | R1 state change, R3 per-range aggregation |
| Lifecycle (BranchBench) | fork latency, divergent writes, per-lineage reads, fork-chain depth 1–10, zero-write-fork diagnostic |

Spec source: the TPCTC'13 proposal (Kaufmann et al.) and its EDBT'14
evaluation. Both, with the workload's derivation and the discussion these
numbers feed, live in the companion documentation repository beside this one.

## What this is, next to `benches/`

Two criterion harnesses, and they do not overlap.

`benches/budgets.rs` times **the operations this crate bounds**, against the
budgets it states, one machine against itself. It is part of the crate.

This suite times **a workload taxonomy** — T/K/R classes over a generated
bitemporal history, run on the trunk and on a divergent lineage. It is a
separate package on purpose (see `rs-tpcbih/Cargo.toml`), so nothing here is
built by an ordinary `cargo test` or `cargo clippy` at the root.

Neither is a gate. **Performance is seen, not enforced**
([D-055](../docs/architecture/s13-decision-register.md#d-055)): absolute
timings are properties of a machine, and a threshold committed here would be a
number about one box. Regression detection compares a machine against itself —
criterion baselines, or a diff of the result JSON.

## Layout

```text
benchmarks/
  README.md              this file
  tpcbih_style.py        Python harness (the macrame package), v2 hardened
  diagnostics/
    spike_ladder.py      the measurement half of docs/macrame-perf-diagnostics.md
                         (§0b: the edge ladder; §9: the follow-on cycle)
  results/               run JSON: figures + assertion log + meta
  rs-tpcbih/             Rust harness (criterion), same workload classes
    Cargo.toml           its own package, its own workspace, path dep on ../..
    benches/tpcbih.rs
    target/              criterion baselines and estimates (git-ignored)
```

`rs-tpcbih` is not a workspace member and does not ship in the `.crate`
tarball; the rest of `benchmarks/` is named in the root manifest's `exclude`
for the same reason. Neither costs a published crate anything.

## Running

Both harnesses run against **the tree they are sitting in**, which is the
point of them being here rather than beside a checkout.

```sh
# Python: build the in-tree extension first, then run from the repo root.
python scripts/build_python_ext.py
PYTHONPATH=python python benchmarks/tpcbih_style.py --scale 1   # ~1.5k edges
PYTHONPATH=python python benchmarks/tpcbih_style.py --scale 3   # ~4.6k edges
```

Warmup ×3, 5 reps, a `SELECT 1` control per group, assertions before timing.
Results land in `benchmarks/results/` whatever the working directory.

```sh
# Rust (criterion: 1 s warmup, 2 s measurement, 100-sample estimates)
cd benchmarks/rs-tpcbih
MACRAME_TPCBIH_SCALE=1 cargo bench --bench tpcbih
MACRAME_TPCBIH_SCALE=1 cargo bench --bench tpcbih -- --save-baseline main
# ... change code in ../../src ...
MACRAME_TPCBIH_SCALE=1 cargo bench --bench tpcbih -- --baseline main
```

The first build compiles the libSQL amalgamation and takes a while; after that
only this crate and the ledger recompile.

Both harnesses abort on correctness failure: **13 assertions** — interval-model
agreement, pre-divergence `main` == `audit`, reader-vs-replay agreement, key
consistency, non-empty branch diff — must pass before anything is timed. A
benchmark that can time a wrong answer is worse than none.

## Methodology (read before citing a number)

- Medians of repeated samples after warmup, with min/max spread and a
  trivial-operation control per group. Session-to-session noise on ordinary
  hardware runs near 29%: report **shapes and orders of magnitude** — flat
  across sizes, constant across depths — never decimals.
- Absolute budgets are machine properties, not code properties: measured,
  never CI-gated ([D-055](../docs/architecture/s13-decision-register.md#d-055)).
- All published figures are labeled by hardware. Anything in `results/` is
  **PRELIMINARY (non-reference hardware)** until rerun on reference iron
  (Windows 11, NVMe SSD, 32 GB RAM, release build).

## Latest readings (scale 1 → 3, preliminary)

**Measured at 0.20.0, release build, both columns from the same run;** all 13
assertions passed at both scales. See `diagnostics/` for the spike-ladder
harness that found and fixed the fresh-file bulk-import defect, whose
before/after table lives in [D-274](../docs/architecture/s13-decision-register.md#d-274). The
vector-build dissection (plan §9.1, [D-276](../docs/architecture/s13-decision-register.md#d-276))
ran as `examples/vector_build_probe.rs`; its before/after table is in the
register entry.

| Class | Trunk | Branch |
|---|---|---|
| T2 point slice | 0.63–1.73 → 1.95–5.68 ms | 2.37–6.64 → 7.25–19.6 ms |
| T5 history fold | 10.9 → 34.1 ms | 13.3 → 40.9 ms (`reconstruct_on`) |
| T6a valid-slice @ txn point | 11.8 → 35.7 ms | — |
| T6b txn-versions @ valid point | 57.7 → 507 ms (partly OR-chunking) | — |
| K1 key history | 0.26 → 0.62 ms | — |
| R1 state change | 2.28 → 7.29 ms | 7.50 → 22.8 ms |
| R3 agg × 6 instants | 6.33 → 19.7 ms | 22.1 → 66.0 ms |
| Q3 traverse (36 nodes) | 0.42 → 0.44 ms (flat) | 16.8 → 49.3 ms; chain d1–d5 flat, d10 +15% |
| fork | — | 0.145 → 0.159 ms (flat, O(1)) |

Headline findings: fork latency flat across 3× data (empirical O(1));
transaction-dimension work an order pricier than valid-dimension work (T6);
branch cost is ancestry, not divergence (zero-write fork 15.9 ms ≈ divergent
fork 16.8 ms at scale 1); chain reads flat from depth 1 to 5 and 15% dearer
at 10. The branched read *level* sits 3–4× above trunk on slices and
aggregates, and about 40× above it on traversal, at these scales. The Rust
port (`rs-tpcbih`, scale 1) reads the branched paths within ~10% of Python
(slice 2.32–6.04 ms, `reconstruct_on` 12.5 ms, Q3 17.5 ms), so binding
overhead is not in them; on sub-millisecond trunk reads Python is up to 1.7×
slower (Q3 0.24 against 0.42 ms), which is the per-call cost of the binding.

Every column since the last reading moved down, the branch column most: the
previous figures predated [D-272](../docs/architecture/s13-decision-register.md#d-272)
and [D-273](../docs/architecture/s13-decision-register.md#d-273).

## Using these to improve performance

1. Save baselines *before* changing code (commands above).
2. Prime suspects, in evidence order: the branched-traversal plan — read the
   branch lowering's `EXPLAIN QUERY PLAN`, check `idx_lc_lineage_cut` is
   entered with both columns bound, and remember that the last two defects
   found here were both **join order and bound columns rather than a missing
   index** ([D-272](../docs/architecture/s13-decision-register.md#d-272),
   [D-273](../docs/architecture/s13-decision-register.md#d-273)) → snapshot hit
   rates for folds (don't micro-optimise the folds themselves) → harness-side
   costs (T6b OR-chains, Python set-diffs: profile with `py-spy`/`samply`
   before attributing anything to the engine).
3. Re-run, compare against the baseline, keep the 13 assertions green.
4. A plan discovered here is worth pinning in `tests/index_plan_tests.rs`
   before it is optimised, because a measured plan without a pin has a shelf
   life — that is the whole lesson of
   [D-272](../docs/architecture/s13-decision-register.md#d-272).
5. Promote a figure to prose only with reference hardware and repeated
   sessions; until then it stays labeled preliminary.
