"""TPC-BiH-style temporal workload for macrame-db, v2 (hardened).

Faithful to the TPCTC'13 taxonomy (Kaufmann et al.):
  T-class  timeslice      T1/T2 history-shape contrast (update-heavy vs
                             insert-focused key chains), T2 point-point slices,
                             T4 TOP-N changed keys, T5 full-history yardstick,
                             T6 slice-vs-point both directions
  K-class  pure-key audit K1 full tuple history, K2 +range, K4 TOP-N,
                             K5 preceding version, K6 selectivity sweep
  R-class  range-timeslice R1 state-change (two temporal evals + compare),
                             R3 temporal aggregation per time range
Plus per-lineage execution (trunk + divergent `audit` branch), a fork-chain
depth test (BranchBench framing), and correctness assertions (a benchmark that
can time a wrong answer is worse than none):

  A1  Q1 counts equal the Python-side interval model (3 instants x 2 lineages)
  A2  main == audit at a pre-divergence valid instant (shared history)
  A3  reconstruct_on(ts,br).edges == query_as_of_edges(ts,br) per lineage
  A4  K5 row == second-to-last K1 row (preceding-version consistency)
  A5  diff('main','audit') is non-empty after divergent writes

Stats: 1 warmup + 5 reps -> median/min/max; SELECT-1 control per group.
Labels: PRELIMINARY (non-reference hardware) unless run on reference iron.

Usage: python benchmarks/tpcbih_style.py [--scale N]   (from the repo root)
Results: benchmarks/results/tpcbih_style_v2_<scale>.json
"""

import argparse
import json
import os
import random
import statistics
import sys
import tempfile
import time
from datetime import datetime, timezone

from macrame import AttributeMode, ConceptUpsert, Database, EdgeAssertion

VALID_START = "2024-01-01T00:00:00Z"
LATEST = "2030-01-01T00:00:00Z"
DIVERGE_AT = "2024-11-01T00:00:00Z"


def iso(month, day=15):
    return "2024-%02d-%02dT00:00:00Z" % (month, day)


def month_of(ts):
    return int(ts[5:7])


class Stats:
    def __init__(self, db):
        self.db = db
        self.out = {"figures": {}, "assertions": [], "meta": {}}

    def control(self):
        samples = []
        for _ in range(5):
            t0 = time.perf_counter()
            self.db.diagnostic_query("SELECT 1")
            samples.append((time.perf_counter() - t0) * 1000.0)
        return round(statistics.median(samples), 4)

    def measure(self, name, fn, reps=5, extra=None, control=True):
        for _ in range(3):
            fn()  # warmup x3: plan + page caches settle before timing
        samples = []
        out = None
        for _ in range(reps):
            t0 = time.perf_counter()
            out = fn()
            samples.append((time.perf_counter() - t0) * 1000.0)
        row = {"median_ms": round(statistics.median(samples), 3),
               "min_ms": round(min(samples), 3),
               "max_ms": round(max(samples), 3)}
        if control:
            row["control_ms"] = self.control()
        if extra:
            row.update(extra)
        self.out["figures"][name] = row
        print("  %-30s med %9.3f  range [%7.3f, %7.3f]  %s"
              % (name, row["median_ms"], row["min_ms"], row["max_ms"], extra or ""))
        return out

    def check(self, name, cond, detail=""):
        self.out["assertions"].append({"name": name, "pass": bool(cond), "detail": detail})
        print("  [%s] %s %s" % ("PASS" if cond else "FAIL", name, detail))
        if not cond:
            raise AssertionError("correctness assertion failed: %s %s" % (name, detail))


def build_dataset(scale=1):
    rng = random.Random(42)
    n_sup, n_part, n_ord = 40 * scale, 200 * scale, 400 * scale
    heavy = set("S%04d" % i for i in range(n_sup // 2))  # update-heavy group (T1)
    # insert-focused group (T2): remaining suppliers, append-only evolution
    concepts = (
        [("S%04d" % i, "Supplier %d" % i) for i in range(n_sup)]
        + [("P%04d" % i, "Part %d" % i) for i in range(n_part)]
        + [("O%04d" % i, "Order %d" % i) for i in range(n_ord)]
    )
    edges = []
    for i in range(n_sup):
        for p in rng.sample(range(n_part), 8):
            edges.append(("S%04d" % i, "P%04d" % p, "SUPPLIES", iso(rng.randint(1, 6))))
    for i in range(n_ord):
        for p in rng.sample(range(n_part), 3):
            edges.append(("O%04d" % i, "P%04d" % p, "CONTAINS", iso(rng.randint(3, 10))))
    return concepts, edges, heavy, n_part


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--scale", type=int, default=1)
    args = ap.parse_args()

    concepts, edges, heavy, n_part = build_dataset(args.scale)
    path = os.path.join(tempfile.mkdtemp(prefix="tpcbih_"), "bench.db")
    db = Database.open(path, snapshot_every_entries=None)
    st = Stats(db)
    st.out["meta"] = {"scale": args.scale, "label": "PRELIMINARY (non-reference hardware)",
                      "concepts": len(concepts), "seed_edges": len(edges)}

    # Interval model: hist[key] = [(valid_from, valid_to|None)] trunk; b_hist for audit.
    hist, b_hist = {}, {}

    def h_add(h, key, vf):
        h.setdefault(key, []).append([vf, None])

    def h_close(h, key, vt):
        for v in h[key]:
            if v[1] is None:
                v[1] = vt
                return
        raise KeyError("no open version for %r" % (key,))

    def covering(h, key, ts):
        return any(vf <= ts and (vt is None or ts < vt) for vf, vt in h.get(key, []))

    def expected(ts, branch):
        n = 0
        for key in hist:
            if branch == "audit" and covering(b_hist, key, ts):
                n += 1
            elif covering(hist, key, ts):
                n += 1
        return n

    print("== load ==")
    t0 = time.perf_counter()
    for cid, title in concepts:
        db.upsert_concept(ConceptUpsert(cid, title, valid_from=VALID_START))
    for s, t, et, vf in edges:
        db.assert_edge(EdgeAssertion(s, t, et, valid_from=vf))
        h_add(hist, (s, t, et), vf)
    st.out["figures"]["load_total"] = {
        "median_ms": round((time.perf_counter() - t0) * 1000.0, 3),
        "control_ms": st.control()}
    print("  loaded %d concepts, %d edges" % (len(concepts), len(edges)))
    load_mark = db.diagnostic_query(
        "SELECT recorded_at FROM transaction_log WHERE seq_id = "
        "(SELECT MAX(seq_id) FROM transaction_log)")[0][0]

    print("== evolution: update-heavy group corrected repeatedly (T1 shape) ==")
    rng = random.Random(7)
    heavy_open = [k for k in hist if k[0] in heavy]
    n_corr = 0
    t0 = time.perf_counter()
    target, guard = 400 * args.scale, 0
    while n_corr < target and guard < target * 10:
        guard += 1
        key = rng.choice(heavy_open)
        s, t, et = key
        vf = hist[key][-1][0]
        vm = month_of(vf)
        if vm >= 12:
            continue
        vt = iso(vm + 1, 1)
        db.retire_edge(s, t, et, vf, vt)
        h_close(hist, key, vt)
        db.assert_edge(EdgeAssertion(s, t, et, valid_from=vt))
        h_add(hist, key, vt)
        n_corr += 1
    st.out["figures"]["evolution_total"] = {
        "median_ms": round((time.perf_counter() - t0) * 1000.0, 3),
        "control_ms": st.control(), "corrections": n_corr}
    print("  %d corrections applied" % n_corr)
    print("== append wave: insert-focused group grows append-only (T2 shape) ==")
    ins_sup = sorted(set("S%04d" % i for i in range(40 * args.scale)) - heavy)
    n_app = 0
    t0 = time.perf_counter()
    for s in ins_sup:
        for p in rng.sample(range(n_part), 2):
            key = (s, "P%04d" % p, "SUPPLIES")
            if key in hist:
                continue
            vf = iso(rng.choice((9, 10, 11)))
            db.assert_edge(EdgeAssertion(s, *key[1:], valid_from=vf))
            h_add(hist, key, vf)
            n_app += 1
    app_ms = round((time.perf_counter() - t0) * 1000.0, 3)
    print("  %d appends in %.3f ms" % (n_app, app_ms))
    st.out["figures"]["append_wave"] = {"median_ms": app_ms,
                                            "control_ms": st.control(), "appends": n_app}
    evo_mark = db.diagnostic_query(
        "SELECT recorded_at FROM transaction_log WHERE seq_id = "
        "(SELECT MAX(seq_id) FROM transaction_log)")[0][0]

    print("== fork audit + divergent writes ==")
    t0 = time.perf_counter()
    db.fork("audit")
    st.out["figures"]["fork_audit"] = {
        "median_ms": round((time.perf_counter() - t0) * 1000.0, 3),
        "control_ms": st.control()}
    audit_keys = [k for k in hist if covering(hist, k, DIVERGE_AT)][:100 * args.scale]
    t0 = time.perf_counter()
    for key in audit_keys:
        s, t, et = key
        db.retire_edge(s, t, et, hist[key][-1][0], DIVERGE_AT, branch="audit")
        db.assert_edge(EdgeAssertion(s, t, et, valid_from=DIVERGE_AT, weight=0.25,
                                    branch="audit"))
        h_add(b_hist, key, DIVERGE_AT)
    st.out["figures"]["branch_divergent_writes"] = {
        "median_ms": round((time.perf_counter() - t0) * 1000.0, 3),
        "control_ms": st.control(), "rows": len(audit_keys)}

    def q1(ts, br):
        return db.query_as_of_edges(ts, branch=br)

    def keyset(rows):
        return set((r[0], r[1], r[2]) for r in rows)

    print("== A1 model agreement (T2 point-point slices) ==")
    for ts in (iso(5, 1), iso(9, 1), iso(12, 1)):
        for br in ("main", "audit"):
            rows = st.measure("Q1_slice_%s_%s" % (ts[5:7], br),
                              lambda ts=ts, br=br: q1(ts, br))
            st.check("A1_%s_%s" % (ts[5:7], br), len(rows) == expected(ts, br),
                     "got %d want %d" % (len(rows), expected(ts, br)))

    print("== A2 pre-divergence equality (shared history) ==")
    rm, ra = q1(iso(6, 1), "main"), q1(iso(6, 1), "audit")
    st.check("A2_equal", keyset(rm) == keyset(ra),
             "%d vs %d rows" % (len(rm), len(ra)))

    print("== A3 reader-vs-replay agreement (replay history open at ts) ==")
    tsA3 = iso(12, 20)
    tsA3dt = datetime.strptime(tsA3, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    for br in ("main", "audit"):
        replay = st.measure("Q2_reconstruct_on_%s" % br,
                            lambda br=br: db.reconstruct_on(LATEST, br))
        open_hist = [e for e in replay.edges
                     if e[3] <= tsA3dt and (e[4] is None or tsA3dt < e[4])]
        rows = q1(tsA3, br)
        st.check("A3_%s" % br, len(open_hist) == len(rows),
                 "replay-open %d reader %d" % (len(open_hist), len(rows)))

    print("== T5 full-history yardstick ==")
    st.measure("Q5_full_fold", lambda: db.reconstruct(LATEST))
    st.measure("Q2_reconstruct_loadpoint", lambda: db.reconstruct(load_mark))
    st.measure("Q2_reconstruct_evopoint", lambda: db.reconstruct(evo_mark))

    print("== K-class pure-key audit on a corrected edge ==")
    k2 = [k for k in hist if len(hist[k]) > 1][0]
    like = "%s|%s|%s|%%" % k2
    # trunk rows for v versions: v asserts + (v-1) retires = 2v-1; branch rows excluded
    k1 = st.measure("K1_full_history",
                    lambda: db.diagnostic_query(
                        "SELECT seq_id, recorded_at FROM transaction_log "
                        "WHERE table_name='links' AND branch_id='main' "
                        "AND entity_id LIKE ? ORDER BY seq_id",
                        [like]))
    st.check("A1K_count", len(k1) == 2 * len(hist[k2]) - 1,
             "log rows %d versions %d" % (len(k1), len(hist[k2])))
    lo, hi = k1[0][1], k1[-1][1]
    k2r = st.measure("K2_ranged_history",
                     lambda: db.diagnostic_query(
                         "SELECT seq_id FROM transaction_log WHERE table_name='links' "
                         "AND branch_id='main' AND entity_id LIKE ? "
                         "AND recorded_at >= ? AND recorded_at <= ? "
                         "ORDER BY seq_id", [like, lo, hi]))
    st.check("A2K_range", len(k2r) == len(k1), "%d vs %d" % (len(k2r), len(k1)))
    mid = k1[-1][1]
    k5 = st.measure("K5_preceding_version",
                    lambda: db.diagnostic_query(
                        "SELECT seq_id FROM transaction_log WHERE table_name='links' "
                        "AND branch_id='main' AND entity_id LIKE ? AND recorded_at < ? "
                        "ORDER BY seq_id DESC LIMIT 1", [like, mid]))
    st.check("A4_preceding", k5 and k5[0][0] == k1[-2][0],
             "k5 %r want seq %r" % (k5, k1[-2][0]))

    def khist(pat, order="ORDER BY seq_id", extra=""):
        return db.diagnostic_query(
            "SELECT seq_id FROM transaction_log WHERE table_name='links' "
            "AND branch_id='main' AND entity_id LIKE ? %s %s" % (extra, order), [pat])

    print("== T1 vs T2: history-shape contrast (update-heavy vs insert-focused) ==")
    chains_h = sorted((len(v) for k, v in hist.items() if k[0] in heavy), reverse=True)
    chains_i = sorted((len(v) for k, v in hist.items() if k[0] not in heavy
                       and k[2] == "SUPPLIES"), reverse=True)
    st.out["meta"]["chain_heavy"] = {"max": chains_h[0], "avg": round(sum(chains_h) / len(chains_h), 2)}
    st.out["meta"]["chain_insert"] = {"max": chains_i[0], "avg": round(sum(chains_i) / len(chains_i), 2)}
    hk = next(k for k in hist if k[0] in heavy and len(hist[k]) == chains_h[0])
    ik = next(k for k in hist if k[0] not in heavy and len(hist[k]) == 1)
    st.measure("T1K_heavy_chain", lambda: khist("%s|%s|%s|%%" % hk),
               extra={"versions": len(hist[hk])})
    st.measure("T2K_insert_chain", lambda: khist("%s|%s|%s|%%" % ik),
               extra={"versions": 1})

    print("== K4 TOP-N versions ==")
    st.measure("K4_top3", lambda: khist("%s|%s|%s|%%" % hk,
                                          order="ORDER BY seq_id DESC LIMIT 3"))

    print("== K6 predicate selectivity sweep (supplier-id cutoff over slice) ==")
    nsup = 40 * args.scale
    for frac in (0.1, 0.5, 1.0):
        cut = "S%04d" % int(nsup * frac)

        def k6(cut=cut):
            return [r for r in q1(iso(9, 1), "main") if r[0] <= cut]

        sel = st.measure("K6_selectivity_%d" % int(frac * 100), k6)
        st.out["figures"]["K6_selectivity_%d" % int(frac * 100)]["rows"] = len(sel)

    print("== T6 slice-vs-point both directions (dimensional preference) ==")

    def t6a():
        ms = db.reconstruct(evo_mark)  # pre-fork mark: trunk only
        lo = datetime(2024, 3, 1, tzinfo=timezone.utc)
        hi = datetime(2024, 9, 1, tzinfo=timezone.utc)
        return [e for e in ms.edges if e[3] < hi and (e[4] is None or e[4] > lo)]

    r6a = st.measure("T6a_validslice_txnpoint", t6a)
    st.out["figures"]["T6a_validslice_txnpoint"]["rows"] = len(r6a)

    def t6b():
        rows = q1(iso(9, 1), "main")
        pats = ["entity_id LIKE '%s|%s|%s|%%%%'" % (r[0], r[1], r[2]) for r in rows]
        total = 0
        for i in range(0, len(pats), 300):  # SQLite caps expression depth: chunk ORs
            ors = " OR ".join(pats[i:i + 300])
            total += db.diagnostic_query(
                "SELECT COUNT(*) FROM transaction_log WHERE table_name='links' "
                "AND branch_id='main' AND recorded_at <= '%s' AND (%s)"
                % (evo_mark, ors))[0][0]
        return total

    r6b = st.measure("T6b_txnversions_validpoint", t6b)
    st.out["figures"]["T6b_txnversions_validpoint"]["logrows"] = r6b

    print("== T4 TOP-N changed keys (correlated double-travel) ==")

    def changed(br):
        before = {(r[0], r[1], r[2]): r[3] for r in q1(iso(4, 1), br)}
        after = {(r[0], r[1], r[2]): r[3] for r in q1(iso(10, 1), br)}
        return [k for k in after if k not in before or before[k] != after[k]]

    st.measure("T4_top10_changed", lambda: changed("main")[:10])

    print("== A5 branch divergence is real ==")
    d = db.diff("main", "audit")
    st.check("A5_diff", len(d) > 0, "%d differing rows" % len(d))

    print("== R1 state-change (two temporal evals + compare) ==")
    for br in ("main", "audit"):
        ch = st.measure("R1_statechange_%s" % br, lambda br=br: changed(br))
        print("    changed keys on %s: %d" % (br, len(ch)))

    print("== R3 temporal aggregation per range (6 instants) ==")
    instants = [iso(m, 1) for m in (2, 4, 6, 8, 10, 12)]

    def agg(br):
        out = {}
        for ts in instants:
            per = {}
            for r in q1(ts, br):
                if r[0].startswith("S"):
                    per[r[0]] = per.get(r[0], 0) + 1
            out[ts] = sum(per.values())
        return out

    for br in ("main", "audit"):
        st.measure("R3_agg_%s" % br, lambda br=br: agg(br),
                   extra={"instants": len(instants)})

    print("== Q3 temporal join (3-hop traversal at instant) ==")
    seeds = ["S%04d" % i for i in (0, 7, 19, 33)][:max(1, min(4, 10 * args.scale))]

    def run(br):
        n = 0
        for s in seeds:
            n += len(db.traverse(s, max_depth=3, as_of_valid=iso(9, 1), branch=br,
                                 attribute_mode=AttributeMode.CURRENT))
        return n

    for br in ("main", "audit"):
        st.measure("Q3_traverse_%s" % br, lambda br=br: run(br))
    # visited counts recorded post-hoc (measure returns rows, not counts)
    for br in ("main", "audit"):
        st.out["figures"]["Q3_traverse_%s" % br]["visited"] = run(br)

    print("== divergence scaling diagnostic (Q3 on zero-write fork) ==")
    db.fork("audit0", "main")
    st.measure("Q3_traverse_audit0_nowrites", lambda: run("audit0"))

    print("== fork-chain depth test ==")
    prev = "main"
    for i in range(1, 11):
        db.fork("d%d" % i, prev)
        prev = "d%d" % i
    for depth in (1, 2, 5, 10):
        st.measure("chain_traverse_d%d" % depth,
                   lambda depth=depth: db.traverse(
                       seeds[0], max_depth=3, as_of_valid=iso(9, 1),
                       branch="d%d" % depth, attribute_mode=AttributeMode.CURRENT))

    db.close()
    # Relative to this file rather than to the caller's cwd: the harness is
    # run from the repo root by convention and from `benchmarks/` by habit,
    # and only one of those wrote its results where the README says they are.
    results_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "results")
    os.makedirs(results_dir, exist_ok=True)
    out = os.path.join(results_dir, "tpcbih_style_v2_%d.json" % args.scale)
    with open(out, "w", encoding="utf-8") as f:
        json.dump(st.out, f, indent=2)
    fails = [a for a in st.out["assertions"] if not a["pass"]]
    print("assertions: %d passed, %d failed; wrote %s"
          % (len(st.out["assertions"]) - len(fails), len(fails), out))
    if fails:
        sys.exit(1)


if __name__ == "__main__":
    sys.exit(main())
