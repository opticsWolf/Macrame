"""Spike ladder — the measurement half of docs/macrame-perf-diagnostics.md.

Implements the diagnostics plan's section 1 (per-chunk hold curves, metrics,
EXPLAIN QUERY PLAN, snapshot accounting) and section 3 (caller-visible knob
sweeps), on the discipline section 0 states:

- fresh DB per run, one open per DB (R15);
- medians of >=3 sessions, control query per session;
- storage state (page_count, freelist_count, WAL bytes, snapshot bytes)
  recorded with every timing;
- per-chunk holds as the primary signal: growing holds scale with n, flat
  holds are fixed per-chunk overhead.

No timings here are gates (D-055). Output is a table plus JSON in
benchmarks/results/diagnostics/.

Usage (from the repo root, with the in-tree extension built):
    PYTHONPATH=python python benchmarks/diagnostics/spike_ladder.py [--which ...]
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import statistics
import sys
import tempfile
import time
import uuid
from pathlib import Path

from macrame import ConceptUpsert, Database, EdgeAssertion

ROOT = Path(__file__).resolve().parent.parent.parent
RESULTS = ROOT / "benchmarks" / "results" / "diagnostics"

WAL_SENTINEL = "9999-12-31T23:59:59.999999Z"
STAMP = "2025-01-01T00:00:00Z"

# unique-key pools: every edge between two distinct concepts, one kind.
CONCEPT_POOL_SIZES = {"edges": 0, "concepts": 0}


def iso(dt: str) -> str:
    return dt


def make_concepts(n: int) -> list[ConceptUpsert]:
    return [ConceptUpsert(id=f"c{i}", title=f"concept {i}", valid_from=STAMP) for i in range(n)]


def make_edges(n: int, concepts: int, hub: bool = False) -> list[EdgeAssertion]:
    """Edges with unique keys. `hub` forms hubs (same source fan-out); the
    default is near-chain (i <> j) to keep overlap-guard candidates tiny."""
    import random

    rng = random.Random(20250910)
    edges: list[EdgeAssertion] = []
    seen: set[tuple[int, int]] = set()
    i = 0
    while len(edges) < n:
        if hub:
            s, t = rng.randrange(concepts), rng.randrange(concepts)
        else:
            s = i % concepts
            t = (s + 1 + rng.randrange(3)) % concepts
            i += 1
        if s == t or (s, t) in seen:
            continue
        seen.add((s, t))
        edges.append(
            EdgeAssertion(
                source=f"c{s}",
                target=f"c{t}",
                edge_type="RELATES",
                valid_from=STAMP,
                valid_to=WAL_SENTINEL,
                weight=1.0,
            )
        )
    return edges


class Session:
    """One fresh DB, one open, one ladder run."""

    def __init__(self, snapshot_every: int | None = 10_000, poll: float = 1.0):
        # Not a TemporaryDirectory: its finalizer deletes while the engine may
        # still hold the file handle, and Windows refuses. Explicit dir + a
        # tolerant cleanup after close() instead.
        self.tmp_name = os.path.join(tempfile.gettempdir(), f"macrame-ladder-{uuid.uuid4().hex[:8]}")
        os.makedirs(self.tmp_name, exist_ok=True)
        self.tmp = self
        self.path = Path(self.tmp_name) / "ladder.db"
        self.snapshot_every = snapshot_every
        self.poll = poll

    def cleanup(self) -> None:
        shutil.rmtree(self.tmp_name, ignore_errors=True)

    def open(self) -> Database:
        db = Database.open(
            str(self.path),
            snapshot_every_entries=self.snapshot_every,
            snapshot_poll_seconds=self.poll,
        )
        return db

    def control_ms(self, db: Database) -> float:
        samples = []
        for _ in range(5):
            t0 = time.perf_counter()
            db.diagnostic_query("SELECT 1")
            samples.append((time.perf_counter() - t0) * 1000.0)
        return round(statistics.median(samples), 4)

    def storage(self) -> dict:
        pages = free = 0
        try:
            rows = self._diag("PRAGMA page_count")
            pages = rows[0][0] if rows else 0
            rows = self._diag("PRAGMA freelist_count")
            free = rows[0][0] if rows else 0
        except Exception:
            pass
        wal = 0
        wal_path = Path(self.tmp_name) / "ladder.db-wal"
        if wal_path.exists():
            wal = wal_path.stat().st_size
        snap_dir = Path(self.tmp_name) / "snapshots"
        snap = sum(f.stat().st_size for f in snap_dir.rglob("*") if f.is_file()) if snap_dir.exists() else 0
        return {"pages": pages, "freelist": free, "wal_bytes": wal, "snapshot_bytes": snap}

    def _diag(self, sql: str):
        # diagnostic_query is a Database method; called only with db attached
        return self._last_db.diagnostic_query(sql)

    def seed(self, db: Database, concepts: int, progress=None) -> float:
        """Concepts first (FK targets). Returns wall seconds."""
        t0 = time.perf_counter()
        db.write_concepts(make_concepts(concepts), progress=progress)
        return time.perf_counter() - t0


def run_ladder(session: Session, db: Database, edges: list[EdgeAssertion], label: str) -> dict:
    """One bulk_import call with per-chunk hold capture."""
    holds: list[float] = []
    rows_per_chunk: list[int] = []
    last = {"written": 0}

    def progress(p: dict) -> None:
        if p["written"] > last["written"]:
            holds.append(p["held_ms"])
            rows_per_chunk.append(p["rows"])
            last["written"] = p["written"]

    t0 = time.perf_counter()
    db.bulk_import(edges, progress=progress)
    wall = time.perf_counter() - t0
    return {
        "label": label,
        "edges": len(edges),
        "wall_s": round(wall, 3),
        "hold_sum_ms": round(sum(holds), 1),
        "chunks": len(holds),
        "rows_per_chunk_first_last": [rows_per_chunk[0] if rows_per_chunk else 0,
                                      rows_per_chunk[-1] if rows_per_chunk else 0],
        "holds_first5": [round(h, 2) for h in holds[:5]],
        "holds_last5": [round(h, 2) for h in holds[-5:]],
    }


def medians(runs: list[dict]) -> dict:
    out = {}
    for key in ("wall_s", "hold_sum_ms"):
        vals = [r[key] for r in runs]
        out[key] = round(statistics.median(vals), 3)
        out[key + "_min"] = round(min(vals), 3)
        out[key + "_max"] = round(max(vals), 3)
    return out


def bench_edges(sessions: int, sizes: list[int], snapshot_every: int | None) -> dict:
    """Section 1.1: the edge bulk ladder. Fresh DB + seeded concepts each run."""
    out: dict[str, list] = {}
    for n in sizes:
        runs = []
        for s in range(sessions):
            sess = Session(snapshot_every=snapshot_every)
            db = sess.open()
            sess._last_db = db
            db.write_concepts(make_concepts(max(n // 2, 1000)))
            r = run_ladder(sess, db, make_edges(n, max(n // 2, 1000)), f"edges-{n}")
            r["seed_s"] = None
            r["control_ms"] = sess.control_ms(db)
            r["storage_after"] = sess.storage()
            r["violations_after"] = [str(v) for v in db.metrics().violations()][:5]
            runs.append(r)
            db.close()
            sess.tmp.cleanup()
        med = medians(runs)
        per_row = round(med["wall_s"] * 1000.0 / n, 4)
        out[str(n)] = {"median": med, "per_row_ms": per_row, "runs": runs}
        print(f"edges n={n:>6}  wall={med['wall_s']}s  holds={med['hold_sum_ms']}ms"
              f"  per-row={per_row}ms  control={runs[0]['control_ms']}ms", flush=True)
    return out


def bench_call_sizes(sessions: int, total: int, snapshot_every: int | None) -> dict:
    """Section 3a: one total, swept across call sizes."""
    out = {}
    for parts in (1, 2, 4, 8, 16):
        size = total // parts
        runs = []
        for s in range(sessions):
            sess = Session(snapshot_every=snapshot_every)
            db = sess.open()
            sess._last_db = db
            db.write_concepts(make_concepts(total // 2))
            t0 = time.perf_counter()
            edges = make_edges(total, total // 2)
            holds, rows_chunk, last = [], [], {"written": 0}

            def progress(p: dict) -> None:
                if p["written"] > last["written"]:
                    holds.append(p["held_ms"])
                    rows_chunk.append(p["rows"])
                    last["written"] = p["written"]

            wall_total = 0.0
            for k in range(parts):
                t0 = time.perf_counter()
                db.bulk_import(edges[k * size:(k + 1) * size], progress=progress)
                wall_total += time.perf_counter() - t0
            run = {
                "label": f"{parts}x{size}",
                "wall_s": round(wall_total, 3),
                "hold_sum_ms": round(sum(holds), 1),
                "chunks": len(holds),
                "holds_first5": [round(h, 2) for h in holds[:5]],
                "holds_last5": [round(h, 2) for h in holds[-5:]],
                "control_ms": sess.control_ms(db),
                "storage_after": sess.storage(),
            }
            runs.append(run)
            db.close()
            sess.tmp.cleanup()
        med = medians(runs)
        out[f"{parts}x{size}"] = {"median": med, "runs": runs}
        print(f"call-size {parts:>2}x{size:<5} wall={med['wall_s']}s holds={med['hold_sum_ms']}ms", flush=True)
    return out


def bench_vectors(sessions: int, n: int, dims: list[int]) -> dict:
    """Section 1.1 vectors: 5k upserts at dim 64/256, per-chunk holds."""
    import random

    rng = random.Random(7)
    out = {}
    for dim in dims:
        runs = []
        for s in range(sessions):
            sess = Session()
            db = sess.open()
            sess._last_db = db
            # Vectors carry a foreign key into concepts; seed one concept per
            # vector, because the vector key set is 1:1 with the concept ids.
            db.write_concepts(make_concepts(n))
            model = f"probe_{dim}"
            db.register_model(model, dim)
            vecs = [(f"c{i}", [rng.uniform(-1, 1) for _ in range(dim)]) for i in range(n)]

            holds, last = [], {"written": 0}

            def progress(p: dict) -> None:
                if p["written"] > last["written"]:
                    holds.append(p["held_ms"])
                    last["written"] = p["written"]

            t0 = time.perf_counter()
            db.upsert_embeddings(model, vecs, progress=progress)
            wall = time.perf_counter() - t0
            run = {
                "label": f"vectors-d{dim}",
                "n": n,
                "wall_s": round(wall, 3),
                "hold_sum_ms": round(sum(holds), 1),
                "chunks": len(holds),
                "holds_first5": [round(h, 2) for h in holds[:5]],
                "holds_last5": [round(h, 2) for h in holds[-5:]],
                "control_ms": sess.control_ms(db),
                "storage_after": sess.storage(),
            }
            runs.append(run)
            db.close()
            sess.tmp.cleanup()
        med = medians(runs)
        out[f"dim{dim}"] = {"median": med, "runs": runs}
        print(f"vectors dim={dim} n={n} wall={med['wall_s']}s holds={med['hold_sum_ms']}ms", flush=True)
    return out


def bench_concepts(sessions: int, n: int) -> dict:
    """Contrast arm: the concept bulk path at the same scale."""
    runs = []
    for s in range(sessions):
        sess = Session()
        db = sess.open()
        sess._last_db = db
        holds, last = [], {"written": 0}

        def progress(p: dict) -> None:
            if p["written"] > last["written"]:
                holds.append(p["held_ms"])
                last["written"] = p["written"]

        t0 = time.perf_counter()
        db.write_concepts(make_concepts(n), progress=progress)
        wall = time.perf_counter() - t0
        run = {
            "label": f"concepts-{n}",
            "wall_s": round(wall, 3),
            "hold_sum_ms": round(sum(holds), 1),
            "chunks": len(holds),
            "holds_first5": [round(h, 2) for h in holds[:5]],
            "holds_last5": [round(h, 2) for h in holds[-5:]],
            "control_ms": sess.control_ms(db),
        }
        runs.append(run)
        db.close()
        sess.tmp.cleanup()
    med = medians(runs)
    print(f"concepts n={n} wall={med['wall_s']}s holds={med['hold_sum_ms']}ms", flush=True)
    return {"median": med, "runs": runs}


def explain_plans(n: int) -> dict:
    """Section 1.3: EXPLAIN QUERY PLAN on the hot bulk statements,
    against a populated database (n edges), after ANALYZE and before it."""
    sess = Session()
    db = sess.open()
    sess._last_db = db
    concepts = max(n // 2, 1000)
    db.write_concepts(make_concepts(concepts))
    db.bulk_import(make_edges(n, concepts))

    guard_trunk = ("SELECT l.valid_from, l.valid_to FROM links_current l "
                   "WHERE l.source_id = ?1 AND l.target_id = ?2 "
                   "AND l.edge_type = ?3 AND l.valid_from <> ?4")
    guard_forked = ("SELECT l.valid_from, l.valid_to FROM links_current l "
                    "WHERE +l.branch_id = ?5 AND l.source_id = ?1 "
                    "AND l.target_id = ?2 AND l.edge_type = ?3 "
                    "AND l.valid_from <> ?4")
    insert_link = ("INSERT INTO links (source_id, target_id, edge_type, valid_from, "
                   "valid_to, weight, properties, recorded_at, branch_id) "
                   "VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")
    single_open = ("SELECT 1 FROM links_current WHERE source_id = ?1 AND target_id = ?2 "
                   "AND edge_type = ?3 AND branch_id = ?4 "
                   "AND valid_from <> ?5 AND valid_to = '" + WAL_SENTINEL + "'")
    sync_upsert = ("INSERT INTO links_current (source_id, target_id, edge_type, valid_from, "
                   "valid_to, weight, properties, recorded_at, branch_id) "
                   "VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) "
                   "ON CONFLICT(source_id, target_id, edge_type, valid_from, branch_id) DO UPDATE SET "
                   "valid_to = excluded.valid_to, weight = excluded.weight, "
                   "properties = excluded.properties, recorded_at = excluded.recorded_at "
                   "WHERE excluded.recorded_at > links_current.recorded_at")

    targets = {
        "guard_trunk": guard_trunk,
        "guard_forked": guard_forked,
        "insert_link": insert_link,
        "trigger_single_open": single_open,
        "trigger_current_sync": sync_upsert,
        "traverse_sample": ("SELECT target_id FROM links_current WHERE source_id = ? AND valid_to = '"
                            + WAL_SENTINEL + "'"),
    }
    plans: dict[str, dict] = {"raw": {}, "analysed": {}}
    params = ["c5", "c6", "RELATES", STAMP, "main", WAL_SENTINEL]
    for name, sql in targets.items():
        try:
            rows = db.diagnostic_query("EXPLAIN QUERY PLAN " + sql, params)
            plans["raw"][name] = [r[3] if len(r) > 3 else str(r) for r in rows]
        except Exception as e:
            plans["raw"][name] = [f"error: {e}"]
    db.analyze()
    for name, sql in targets.items():
        try:
            rows = db.diagnostic_query("EXPLAIN QUERY PLAN " + sql, params)
            plans["analysed"][name] = [r[3] if len(r) > 3 else str(r) for r in rows]
        except Exception as e:
            plans["analysed"][name] = [f"error: {e}"]

    # statement-level timing: how long does one guard execution take at this size
    timings = {}
    for name in ("guard_trunk", "guard_forked"):
        sql = targets[name]
        samples = []
        for _ in range(200):
            t0 = time.perf_counter()
            try:
                db.diagnostic_query(sql, params)
            except Exception:
                pass
            samples.append((time.perf_counter() - t0) * 1000.0)
        timings[name] = round(statistics.median(samples), 4)

    db.close()
    sess.tmp.cleanup()
    return {"plans": plans, "guard_ms_at_" + str(n): timings}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--which", default="all",
                    choices=["all", "edges", "sweep", "vectors", "concepts", "explain"])
    ap.add_argument("--sessions", type=int, default=3)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    RESULTS.mkdir(parents=True, exist_ok=True)
    out: dict = {"date": time.strftime("%Y-%m-%dT%H:%M:%S"), "hardware": "windows-dev-box",
                 "python": sys.version.split()[0]}
    t_all = time.perf_counter()

    if args.which in ("all", "edges"):
        # Python None = no cadence; the shipped default is every 10,000 entries.
        out["edge_ladder_default_cadence"] = bench_edges(args.sessions, [2000, 4000, 8000, 16000], 10_000)
        out["edge_ladder_no_cadence"] = bench_edges(args.sessions, [2000, 4000, 8000, 16000], None)
    if args.which in ("all", "sweep"):
        out["call_size_sweep"] = bench_call_sizes(args.sessions, 16000, None)
    if args.which in ("all", "vectors"):
        out["vector_ladder"] = bench_vectors(args.sessions, 5000, [64, 256])
    if args.which in ("all", "concepts"):
        out["concept_ladder"] = bench_concepts(args.sessions, 20000)
    if args.which in ("all", "explain"):
        out["explain"] = explain_plans(16000)

    out["total_s"] = round(time.perf_counter() - t_all, 1)
    path = args.out or str(RESULTS / f"spike_ladder_{time.strftime('%Y%m%d_%H%M%S')}.json")
    with open(path, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=1, default=str)
    print(f"\nresults -> {path}  ({out['total_s']}s total)", flush=True)


if __name__ == "__main__":
    main()
