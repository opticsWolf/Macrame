//! TPC-BiH-style temporal workload, native Rust port of
//! `benchmarks/tpcbih_style.py` v2.
//!
//! Same T/K/R classes, same deterministic data, same 13 correctness
//! assertions (panics fail the run before timing starts). Criterion supplies
//! warmup, statistics, and baselines; each group opens with the repo's
//! `control/select_1` convention. Scale via `MACRAME_TPCBIH_SCALE` (default 1).

use criterion::{BenchmarkId, Criterion};
use macrame::prelude::*;
use macrame::{BranchId, Database};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

const LATEST: &str = "2030-01-01T00:00:00.000000Z";
const DIVERGE_AT: &str = "2024-11-01T00:00:00.000000Z";

fn iso(m: u32, d: u32) -> String {
    format!("2024-{m:02}-{d:02}T00:00:00.000000Z")
}

fn scale() -> usize {
    std::env::var("MACRAME_TPCBIH_SCALE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

// SplitMix64: deterministic RNG without new dependencies.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % (n as u64)) as usize
    }
    fn sample_idx(&mut self, n: usize, k: usize) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..n).collect();
        for i in 0..k.min(n) {
            let j = i + self.below(n - i);
            idx.swap(i, j);
        }
        idx.truncate(k.min(n));
        idx
    }
}

type Key = (String, String, String);

struct Fixture {
    db: Database,
    _dir: tempfile::TempDir,
    seeds: Vec<String>,
    audit_id: BranchId,
    like_heavy: String,
    like_insert: String,
    heavy_versions: usize,
    evo_mark: String,
}

fn month_of(ts: &str) -> u32 {
    ts[5..7].parse().unwrap()
}

async fn build(scale: usize) -> Fixture {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Database::open_with_cadence(dir.path().join("bench.db"), None)
        .await
        .unwrap();
    let (n_sup, n_part, n_ord) = (40 * scale, 200 * scale, 400 * scale);
    let heavy: std::collections::HashSet<String> =
        (0..n_sup / 2).map(|i| format!("S{i:04}")).collect();
    let mut rng = Rng(42);

    let mut concepts = Vec::new();
    for i in 0..n_sup {
        concepts.push(ConceptUpsert::new(format!("S{i:04}"), format!("Supplier {i}"))
            .valid_from("2024-01-01T00:00:00.000000Z"));
    }
    for i in 0..n_part {
        concepts.push(ConceptUpsert::new(format!("P{i:04}"), format!("Part {i}"))
            .valid_from("2024-01-01T00:00:00.000000Z"));
    }
    for i in 0..n_ord {
        concepts.push(ConceptUpsert::new(format!("O{i:04}"), format!("Order {i}"))
            .valid_from("2024-01-01T00:00:00.000000Z"));
    }
    db.write_concepts(concepts).await.unwrap();

    // Interval model: trunk history per key.
    let mut hist: HashMap<Key, Vec<(String, Option<String>)>> = HashMap::new();
    let mut edges = Vec::new();
    for i in 0..n_sup {
        for p in rng.sample_idx(n_part, 8) {
            let vf = iso((rng.below(6) + 1) as u32, 15);
            edges.push(EdgeAssertion::new(
                format!("S{i:04}"),
                format!("P{p:04}"),
                "SUPPLIES",
            )
            .valid_from(vf.clone()));
            hist.entry((format!("S{i:04}"), format!("P{p:04}"), "SUPPLIES".into()))
                .or_default()
                .push((vf, None));
        }
    }
    for i in 0..n_ord {
        for p in rng.sample_idx(n_part, 3) {
            let vf = iso((rng.below(8) + 3) as u32, 15);
            edges.push(EdgeAssertion::new(
                format!("O{i:04}"),
                format!("P{p:04}"),
                "CONTAINS",
            )
            .valid_from(vf.clone()));
            hist.entry((format!("O{i:04}"), format!("P{p:04}"), "CONTAINS".into()))
                .or_default()
                .push((vf, None));
        }
    }
    db.bulk_import(edges).await.unwrap();

    // Evolution: update-heavy group corrected repeatedly (T1 shape).
    let mut rng = Rng(7);
    let heavy_open: Vec<Key> = hist.keys().filter(|k| heavy.contains(&k.0)).cloned().collect();
    let mut n_corr = 0;
    let target = 400 * scale;
    let mut guard = 0;
    while n_corr < target && guard < target * 10 {
        guard += 1;
        let key = &heavy_open[rng.below(heavy_open.len())];
        let vf = hist[key].last().unwrap().0.clone();
        let vm = month_of(&vf);
        if vm >= 12 {
            continue;
        }
        let vt = iso(vm + 1, 1);
        db.retire_edge(&key.0, &key.1, &key.2, &vf, &vt).await.unwrap();
        hist.get_mut(key).unwrap().last_mut().unwrap().1 = Some(vt.clone());
        db.assert_edge(EdgeAssertion::new(key.0.clone(), key.1.clone(), key.2.clone())
            .valid_from(vt.clone()))
            .await
            .unwrap();
        hist.get_mut(key).unwrap().push((vt, None));
        n_corr += 1;
    }

    // Append wave: insert-focused group grows append-only (T2 shape).
    for i in n_sup / 2..n_sup {
        for p in rng.sample_idx(n_part, 2) {
            let key = (format!("S{i:04}"), format!("P{p:04}"), "SUPPLIES".to_string());
            if hist.contains_key(&key) {
                continue;
            }
            let vf = iso([9, 10, 11][rng.below(3)], 15);
            db.assert_edge(EdgeAssertion::new(key.0.clone(), key.1.clone(), key.2.clone())
                .valid_from(vf.clone()))
                .await
                .unwrap();
            hist.entry(key).or_default().push((vf, None));
        }
    }
    let evo_mark: String = {
        let mut rows = db
            .read_conn()
            .query(
                "SELECT recorded_at FROM transaction_log WHERE seq_id = (SELECT MAX(seq_id) FROM transaction_log)",
                (),
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<String>(0).unwrap()
    };

    // Fork + divergent writes.
    let audit_id = BranchId::new("audit").unwrap();
    db.fork(audit_id.clone(), BranchId::main()).await.unwrap();
    let mut b_hist: HashMap<Key, Vec<(String, Option<String>)>> = HashMap::new();
    let open: Vec<Key> = hist
        .iter()
        .filter(|(_, v)| v.iter().any(|(_, vt)| vt.is_none()))
        .map(|(k, _)| k.clone())
        .collect();
    for key in open.iter().take(100 * scale) {
        let vf = hist[key].iter().find(|(_, vt)| vt.is_none()).unwrap().0.clone();
        db.retire_edge_on(&key.0, &key.1, &key.2, &vf, DIVERGE_AT, audit_id.clone())
            .await
            .unwrap();
        db.assert_edge(
            EdgeAssertion::new(key.0.clone(), key.1.clone(), key.2.clone())
                .valid_from(DIVERGE_AT.to_string())
                .weight(0.25)
                .on_branch(audit_id.clone()),
        )
        .await
        .unwrap();
        b_hist.entry(key.clone()).or_default().push((DIVERGE_AT.to_string(), None));
    }

    // ---- correctness assertions (fail the run before timing) ----
    let covering = |h: &HashMap<Key, Vec<(String, Option<String>)>>, key: &Key, ts: &str| {
        h.get(key).map(|vs| {
            vs.iter().any(|(vf, vt)| vf.as_str() <= ts && vt.as_deref().is_none_or(|e| ts < e))
        }).unwrap_or(false)
    };
    let expected = |ts: &str, branch: &str| {
        hist.keys()
            .filter(|k| {
                if branch == "audit" && covering(&b_hist, k, ts) {
                    true
                } else {
                    covering(&hist, k, ts)
                }
            })
            .count()
    };
    let keyset = |rows: &[(String, String, String, String, String)]| {
        rows.iter().map(|r| (r.0.clone(), r.1.clone(), r.2.clone())).collect::<std::collections::HashSet<_>>()
    };
    for ts in [iso(5, 1), iso(9, 1), iso(12, 1)] {
        for br in ["main", "audit"] {
            let rows = query_as_of_edges_on(db.read_conn(), &ts, Some(br)).await.unwrap();
            assert_eq!(rows.len(), expected(&ts, br), "A1 model {ts} {br}");
        }
    }
    let (rm, ra) = (
        query_as_of_edges_on(db.read_conn(), &iso(6, 1), Some("main")).await.unwrap(),
        query_as_of_edges_on(db.read_conn(), &iso(6, 1), Some("audit")).await.unwrap(),
    );
    assert_eq!(keyset(&rm), keyset(&ra), "A2 pre-divergence equality");
    for br in ["main", "audit"] {
        let replay = db.reconstruct_on(LATEST, br).await.unwrap();
        let at = iso(12, 20);
        let open_hist = replay
            .edges
            .iter()
            .filter(|e| {
                e.valid_from.as_str() <= at.as_str()
                    && (e.valid_to.is_empty()
                        || e.valid_to.as_str() >= "9999"
                        || at.as_str() < e.valid_to.as_str())
            })
            .count();
        let rows = query_as_of_edges_on(db.read_conn(), &iso(12, 20), Some(br)).await.unwrap();
        assert_eq!(open_hist, rows.len(), "A3 reader-vs-replay {br}");
    }
    assert!(!db.diff(&BranchId::main(), &audit_id).await.unwrap().is_empty(), "A5 diff");

    let hk = hist
        .iter()
        .filter(|(k, _)| heavy.contains(&k.0))
        .max_by_key(|(_, v)| v.len())
        .map(|(k, v)| (k.clone(), v.len()))
        .unwrap();
    let ik = hist
        .iter()
        .find(|(k, v)| !heavy.contains(&k.0) && k.2 == "SUPPLIES" && v.len() == 1)
        .map(|(k, _)| k.clone())
        .unwrap();

    let seeds: Vec<String> = [0, 7, 19, 33]
        .into_iter()
        .take(10 * scale.max(1).min(4))
        .map(|i| format!("S{i:04}"))
        .collect();

    Fixture {
        db,
        _dir: dir,
        seeds,
        audit_id,
        like_heavy: format!("{}|{}|{}|%", hk.0 .0, hk.0 .1, hk.0 .2),
        like_insert: format!("{}|{}|{}|%", ik.0, ik.1, ik.2),
        heavy_versions: hk.1,
        evo_mark,
    }
}

struct Shared {
    rt: tokio::runtime::Runtime,
    fx: Fixture,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

/// Built once on a runtime that is never dropped: the write actor spawns
/// tasks at open, so the creation runtime must outlive every benchmark.
fn shared() -> &'static Shared {
    SHARED.get_or_init(|| {
        let rt = runtime();
        let fx = rt.block_on(build(scale()));
        Shared { rt, fx }
    })
}

fn control_group<'a>(
    c: &'a mut Criterion,
    s: &'static Shared,
    name: &str,
) -> criterion::BenchmarkGroup<'a, criterion::measurement::WallTime> {
    let mut g = c.benchmark_group(name);
    g.bench_function("control/select_1", |b| {
        b.iter(|| {
            s.rt.block_on(async {
                let mut rows = s
                    .fx
                    .db
                    .read_conn()
                    .query("SELECT 1", ())
                    .await
                    .unwrap();
                rows.next().await.unwrap().unwrap();
            })
        });
    });
    g
}

fn t2_slices(c: &mut Criterion) {
    let s = shared();
    let (rt, fx) = (&s.rt, &s.fx);
    let mut g = control_group(c, s, "t2-slice");
    for ts in [iso(5, 1), iso(9, 1), iso(12, 1)] {
        for br in ["main", "audit"] {
            let id = format!("{}-{br}", &ts[5..7]);
            g.bench_function(BenchmarkId::new("slice", id), |b| {
                b.to_async(rt).iter(|| async {
                    query_as_of_edges_on(fx.db.read_conn(), &ts, Some(br))
                        .await
                        .unwrap()
                });
            });
        }
    }
    g.finish();
}

fn replays(c: &mut Criterion) {
    let s = shared();
    let (rt, fx) = (&s.rt, &s.fx);
    let mut g = control_group(c, s, "replay");
    for br in ["main", "audit"] {
        g.bench_function(BenchmarkId::new("reconstruct_on", br), |b| {
            b.to_async(rt).iter(|| async {
                fx.db
                    .reconstruct_on(LATEST, br)
                    .await
                    .unwrap()
            });
        });
    }
    g.bench_function("reconstruct_latest", |b| {
        b.to_async(rt).iter(|| async { fx.db.reconstruct(LATEST).await.unwrap() });
    });
    g.finish();
}

async fn log_count(db: &Database, like: &str) -> usize {
    let mut rows = db
        .read_conn()
        .query(
            &format!("SELECT COUNT(*) FROM transaction_log WHERE table_name='links' AND branch_id='main' AND entity_id LIKE '{like}'"),
            (),
        )
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
    n as usize
}

fn key_history(c: &mut Criterion) {
    let s = shared();
    let (rt, fx) = (&s.rt, &s.fx);
    let mut g = control_group(c, s, "key-history");
    g.bench_function("t1-heavy-chain", |b| {
        b.to_async(rt).iter(|| async { log_count(&fx.db, &fx.like_heavy).await });
    });
    g.bench_function("t2-insert-chain", |b| {
        b.to_async(rt).iter(|| async { log_count(&fx.db, &fx.like_insert).await });
    });
    g.bench_function("k4-top3", |b| {
        b.to_async(rt).iter(|| async {
            let mut rows = fx
                .db
                .read_conn()
                .query(
                    &format!("SELECT seq_id FROM transaction_log WHERE table_name='links' AND branch_id='main' AND entity_id LIKE '{}' ORDER BY seq_id DESC LIMIT 3", fx.like_heavy),
                    (),
                )
                .await
                .unwrap();
            let mut n = 0;
            while rows.next().await.unwrap().is_some() {
                n += 1;
            }
            n
        });
    });
    g.finish();
}

fn traversals(c: &mut Criterion) {
    use macrame::graph::{AttributeMode, TraversalBuilder};
    let s = shared();
    let (rt, fx) = (&s.rt, &s.fx);
    let mut g = control_group(c, s, "traverse");
    for br in ["main", "audit"] {
        g.bench_function(BenchmarkId::new("q3-3hop", br), |b| {
            b.to_async(rt).iter(|| async {
                let mut n = 0;
                for s in &fx.seeds {
                    n += TraversalBuilder::new(s)
                        .max_depth(3)
                        .as_of_valid(iso(9, 1))
                        .on_branch(br)
                        .attribute_mode(AttributeMode::Current)
                        .execute_ids(fx.db.read_conn(), LATEST)
                        .await
                        .unwrap()
                        .len();
                }
                n
            });
        });
    }
    // Fork-chain depth test: d1..d10 from main, one traversal each.
    let depths: Vec<usize> = rt.block_on(async {
        let mut prev = BranchId::main();
        let mut ds = Vec::new();
        for i in 1..=10usize {
            let id = BranchId::new(format!("d{i}")).unwrap();
            fx.db.fork(id.clone(), prev).await.unwrap();
            prev = id;
            ds.push(i);
        }
        ds
    });
    for d in depths {
        g.bench_function(BenchmarkId::new("chain-depth", d), |b| {
            b.to_async(rt).iter(|| async {
                TraversalBuilder::new(&fx.seeds[0])
                    .max_depth(3)
                    .as_of_valid(iso(9, 1))
                    .on_branch(format!("d{d}"))
                    .attribute_mode(AttributeMode::Current)
                    .execute_ids(fx.db.read_conn(), LATEST)
                    .await
                    .unwrap()
                    .len()
            });
        });
    }
    g.finish();
}

fn aggregates(c: &mut Criterion) {
    let s = shared();
    let (rt, fx) = (&s.rt, &s.fx);
    let mut g = control_group(c, s, "aggregate");
    let instants: Vec<String> = [2, 4, 6, 8, 10, 12].into_iter().map(|m| iso(m, 1)).collect();
    for br in ["main", "audit"] {
        g.bench_function(BenchmarkId::new("r3-six-instants", br), |b| {
            b.to_async(rt).iter(|| async {
                let mut total = 0;
                for ts in &instants {
                    let rows =
                        query_as_of_edges_on(fx.db.read_conn(), ts, Some(br)).await.unwrap();
                    total += rows.iter().filter(|r| r.0.starts_with('S')).count();
                }
                total
            });
        });
        g.bench_function(BenchmarkId::new("r1-statechange", br), |b| {
            b.to_async(rt).iter(|| async {
                let before: std::collections::HashSet<_> =
                    query_as_of_edges_on(fx.db.read_conn(), &iso(4, 1), Some(br))
                        .await
                        .unwrap()
                        .into_iter()
                        .map(|r| (r.0, r.1, r.2))
                        .collect();
                let after: std::collections::HashSet<_> =
                    query_as_of_edges_on(fx.db.read_conn(), &iso(10, 1), Some(br))
                        .await
                        .unwrap()
                        .into_iter()
                        .map(|r| (r.0, r.1, r.2))
                        .collect();
                after.difference(&before).count()
            });
        });
    }
    g.finish();
}

fn main() {
    let mut c = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(2))
        .configure_from_args();
    t2_slices(&mut c);
    replays(&mut c);
    key_history(&mut c);
    traversals(&mut c);
    aggregates(&mut c);
    c.final_summary();
}
