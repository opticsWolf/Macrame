//! The real-reader oracle suite's shared furniture: the axis, the model with
//! its laws, the oracle fns, the pour and its reconcile. P-8's successor file
//! drives it whole; M6-d's archive file drives the same pour, then holds the
//! SAME oracle against the ledger after it has been archived — the cold arm
//! must speak the same law word for word (Amendment 2 §11's M6-d, P-4).
//!
//! Included by `#[path]` from both test files; nothing here is crate code.

use crate::harness::TestHarness;

use macrame::prelude::*;
use macrame::BranchId;

// The valid-time axis
// ---------------------------------------------------------------------------

/// The k-th day of January 2026, canonical and lexicographically ordered.
pub fn v(k: usize) -> String {
    format!("2026-01-{:02}T00:00:00.000000Z", k)
}

/// The axis's far end == the open sentinel; a window ending here is open.
pub const OPEN: &str = "9999-12-31T23:59:59.999999Z";

/// The oracle's pre-history stamp: before the fake clock's very first row.
pub const PRE: &str = "1969-12-31T23:59:59.999998Z";

// ---------------------------------------------------------------------------
// The model: what the pour wrote, as this file itself says the engine wrote it
// ---------------------------------------------------------------------------

/// A classic projection row: one per write of the mint lineage's own history.
/// The supersede's close is its own row under the same act stamp (D-312),
/// which is why two rows can share one `bound`; `seq` is the order the act
/// wrote them, which is the order the fold's newest-row law reads.
pub struct MintRow {
    pub node: &'static str,
    pub id: &'static str,
    pub start: String,
    pub end: String,
    pub bound: String,
    pub title: &'static str,
    pub retired: bool,
    pub seq: usize,
}

/// An overlay version row: the pair a takeover wrote, window included.
pub struct VersionRow {
    pub node: &'static str,
    pub id: &'static str,
    pub start: String,
    pub end: String,
    pub bound: String,
    pub title: &'static str,
}

/// An overlay cut: standing forever (v27's guard refuses rewrites).
pub struct CutRow {
    pub node: &'static str,
    pub id: &'static str,
    pub cut_at: String,
    pub bound: String,
}

/// A fork the pour cut, as the branches table says the lineage resolves.
pub struct Fork {
    pub child: String,
    pub parent: String,
    pub cutoff: String,
}

/// The oracle's world, built as the acts run.
#[derive(Default)]
pub struct Model {
    pub mints: Vec<MintRow>,
    pub versions: Vec<VersionRow>,
    pub cuts: Vec<CutRow>,
    pub forks: Vec<Fork>,
}

/// What one reader answers, in full — enough to compare a whole
/// `ConceptVersion` against the law the pour wrote.
#[derive(Clone, Debug)]
pub struct Answer {
    /// The lineage whose row answers.
    pub bid: String,
    pub title: String,
    pub start: String,
    pub end: String,
    pub stamped: String,
}

/// The coverage the crate applies everywhere: half-open at a named instant,
/// openness at the axis's far end.
pub fn covers(start: &str, end: &str, v: Option<&str>) -> bool {
    match v {
        None => end == OPEN,
        Some(v) => start <= v && v < end,
    }
}

/// The bound rows must sit at or below: the walk's cap per node — the later
/// of the ancestor's fork point and the read's own recorded instant. `None`
/// is unbounded, where the live row is the only candidate there can be.
pub fn cap(cutoff: Option<&str>, r: Option<&str>) -> Option<String> {
    match (cutoff, r) {
        (None, None) => None,
        (None, Some(r)) => Some(r.to_string()),
        (Some(c), None) => Some(c.to_string()),
        (Some(c), Some(r)) => Some(std::cmp::min(c, r).to_string()),
    }
}

impl MintRow {
    fn answer(&self) -> Answer {
        Answer {
            bid: self.node.to_string(),
            title: self.title.to_string(),
            start: self.start.clone(),
            end: self.end.clone(),
            stamped: self.bound.clone(),
        }
    }
}

impl Model {
    /// The walk's path, nearest first, from the branches readback: the viewer
    /// uncut, each ancestor bound at the crossing below it.
    pub fn path(&self, viewer: &str) -> Vec<(String, Option<String>)> {
        let mut out = vec![(viewer.to_string(), None)];
        let mut anchor = viewer;
        while let Some(f) = self.forks.iter().find(|f| f.child == anchor) {
            out.push((f.parent.clone(), Some(f.cutoff.clone())));
            anchor = &f.parent;
        }
        out
    }

    /// The newest write of `(id, node)` at or before the bound — the fold's
    /// one row per `(id, branch)`, the write law's own pick (D-303).
    pub fn newest_mint(&self, id: &str, node: &str, bound: Option<&str>) -> Option<&MintRow> {
        self.mints
            .iter()
            .filter(|m| m.id == id && m.node == node)
            .filter(|m| bound.is_none_or(|b| m.bound.as_str() <= b))
            .max_by_key(|m| m.seq)
    }

    /// The overlay version that answers `(id, node)`, if one covers.
    pub fn version_covering(
        &self,
        node: &str,
        id: &str,
        v: Option<&str>,
        bound: Option<&str>,
    ) -> Option<&VersionRow> {
        self.versions
            .iter()
            .filter(|w| w.node == node && w.id == id)
            .filter(|w| bound.is_none_or(|b| w.bound.as_str() <= b))
            .find(|w| covers(&w.start, &w.end, v))
    }

    /// Whether a cut of `(id, node)` covers at or before the bound.
    pub fn cut_covering(&self, node: &str, id: &str, v: Option<&str>, bound: Option<&str>) -> bool {
        self.cuts
            .iter()
            .filter(|c| c.node == node && c.id == id)
            .filter(|c| bound.is_none_or(|b| c.bound.as_str() <= b))
            .any(|c| match v {
                // A cut covers the present: the far end is inside [cut, ∞).
                None => true,
                Some(v) => c.cut_at.as_str() <= v,
            })
    }

    /// [`concepts_by_id`](macrame::Database::concepts_by_id)'s law: the walk,
    /// then the mint arm — one row per `(id, branch)` decided win-or-absent,
    /// no fall-through past the mint line.
    pub fn by_id(
        &self,
        viewer: &str,
        id: &str,
        v: Option<&str>,
        r: Option<&str>,
    ) -> Option<Answer> {
        let path = self.path(viewer);
        for (node, cutoff) in &path {
            let bound = cap(cutoff.as_deref(), r);
            if let Some(w) = self.version_covering(node, id, v, bound.as_deref()) {
                return Some(Answer {
                    bid: w.node.to_string(),
                    title: w.title.to_string(),
                    start: w.start.clone(),
                    end: w.end.clone(),
                    stamped: w.bound.clone(),
                });
            }
            if self.cut_covering(node, id, v, bound.as_deref()) {
                return None;
            }
        }
        // Where nothing of the overlay decides: the mint line's row. The
        // first path node holding mint rows decides; a row it cannot show
        // (retired, or past its window) is a verdict, not a hole.
        for (node, cutoff) in &path {
            if !self.mints.iter().any(|m| m.node == *node && m.id == id) {
                continue;
            }
            let bound = cap(cutoff.as_deref(), r);
            let row = self.newest_mint(id, node, bound.as_deref())?;
            if row.retired {
                return None;
            }
            return match v {
                Some(v) if !covers(&row.start, &row.end, Some(v)) => None,
                _ => Some(row.answer()),
            };
        }
        None
    }

    /// [`reconstruct_on`](macrame::Database::reconstruct_on)'s law: the walk
    /// at the far end, then the fold's own answer (D-312's projection law).
    pub fn on(&self, viewer: &str, id: &str, r: &str) -> Option<Answer> {
        let path = self.path(viewer);
        for (node, cutoff) in &path {
            let bound = cap(cutoff.as_deref(), Some(r));
            if let Some(w) = self.version_covering(node, id, None, bound.as_deref()) {
                return Some(Answer {
                    bid: w.node.to_string(),
                    title: w.title.to_string(),
                    start: w.start.clone(),
                    end: w.end.clone(),
                    stamped: w.bound.clone(),
                });
            }
            if self.cut_covering(node, id, None, bound.as_deref()) {
                return None;
            }
        }
        for (node, cutoff) in &path {
            if !self.mints.iter().any(|m| m.node == *node && m.id == id) {
                continue;
            }
            let bound = cap(cutoff.as_deref(), Some(r));
            let row = self.newest_mint(id, node, bound.as_deref())?;
            return match row.retired {
                true => None,
                false => Some(row.answer()),
            };
        }
        None
    }

    /// [`reconstruct`](macrame::Database::reconstruct)'s law: the newest
    /// belief per `(id, branch)`, the retired one removing its id.
    pub fn whole_recorded(&self, id: &str, r: &str) -> Option<Answer> {
        self.mints
            .iter()
            .filter(|m| m.id == id)
            .filter(|m| m.bound.as_str() <= r)
            .max_by_key(|m| m.seq)
            .filter(|m| !m.retired)
            .map(|m| m.answer())
    }

    /// [`reconstruct_as_of`](macrame::Database::reconstruct_as_of)'s law: one
    /// current belief per valid start (D-292), the covering start answering,
    /// a retired covering claim removing its id.
    pub fn whole_valid(&self, id: &str, v: &str, bound: Option<&str>) -> Option<Answer> {
        let mut best: Vec<&MintRow> = Vec::new();
        for row in self
            .mints
            .iter()
            .filter(|m| m.id == id)
            .filter(|m| bound.is_none_or(|b| m.bound.as_str() <= b))
        {
            // The newest write per start: replace the incumbent of this
            // start, or claim it for the first time (D-303's within-pair
            // law — "within one start, the row written last").
            match best.iter_mut().find(|s| s.start == row.start) {
                Some(slot) => {
                    if row.seq > slot.seq {
                        *slot = row;
                    }
                }
                None => best.push(row),
            }
        }
        let covering: Vec<&MintRow> = best
            .iter()
            .copied()
            .filter(|s| covers(&s.start, &s.end, Some(v)))
            .collect();
        assert!(
            covering.len() <= 1,
            "two covering starts for {id} at {v}: the pour broke the one-open law"
        );
        covering
            .into_iter()
            .next()
            .filter(|s| !s.retired)
            .map(|s| s.answer())
    }
}

// ---------------------------------------------------------------------------
// The pour
// ---------------------------------------------------------------------------

/// The scenario's titles — one per row, so a title pins which row answered.
pub const T_C1: &str = "c@A trunk";
pub const T_C2: &str = "c@C trunk";
pub const T_CB0: &str = "c@D b0";
pub const T_CB1: &str = "c@F b1";
pub const T_CB2: &str = "c@G b2";
pub const T_C3: &str = "c@K trunk";
pub const T_CDEAD: &str = "c@L retired";
pub const T_D1: &str = "d@B trunk";
pub const T_DCUT: &str = "d@H cut b2";
pub const T_DDEAD: &str = "d@I retired";
pub const T_E1: &str = "e@E b1";

/// The stamp the act just wrote, read back as the store's own bookkeeping.
/// The fake clock issues one µs-monotonic stamp per actor call, so an act's
/// rows share its stamp and the readback is exact.
pub async fn act_stamp(db: &std::sync::Arc<Database>) -> String {
    let mut rows = db
        .read_conn()
        .query("SELECT MAX(recorded_at) FROM transaction_log", ())
        .await
        .expect("the pour reads the act's stamp");
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

/// The pour: every act in order, model written beside each, stamps read back
/// from the log. Each write's rows share one act stamp (D-312's close shares
/// the superseding row's stamp; the W2 pair shares the takeover's).
pub async fn pour(db: &std::sync::Arc<Database>, m: &mut Model) {
    let mut seq = 0usize;

    // Act A — the trunk mints c, open.
    db.upsert_concept(
        ConceptUpsert::new("c", T_C1)
            .valid_from(v(1))
            .valid_to(OPEN),
    )
    .await
    .unwrap();
    let a = act_stamp(db).await;
    m.mints.push(MintRow {
        node: "main",
        id: "c",
        start: v(1),
        end: OPEN.into(),
        bound: a.clone(),
        title: T_C1,
        retired: false,
        seq,
    });
    seq += 1;

    // Act B — the trunk mints d, open.
    db.upsert_concept(
        ConceptUpsert::new("d", T_D1)
            .valid_from(v(3))
            .valid_to(OPEN),
    )
    .await
    .unwrap();
    let b = act_stamp(db).await;
    m.mints.push(MintRow {
        node: "main",
        id: "d",
        start: v(3),
        end: OPEN.into(),
        bound: b.clone(),
        title: T_D1,
        retired: false,
        seq,
    });
    seq += 1;

    // Act C — the trunk supersedes its own c at V7: the close pivots era
    // one's window (`[V1, V7)`), era two opens (`[V7, …)`).
    db.upsert_concept(
        ConceptUpsert::new("c", T_C2)
            .supersede(true)
            .valid_from(v(7))
            .valid_to(OPEN),
    )
    .await
    .unwrap();
    let c = act_stamp(db).await;
    m.mints.push(MintRow {
        node: "main",
        id: "c",
        start: v(1),
        end: v(7),
        bound: c.clone(),
        title: T_C1,
        retired: false,
        seq,
    });
    seq += 1;
    m.mints.push(MintRow {
        node: "main",
        id: "c",
        start: v(7),
        end: OPEN.into(),
        bound: c.clone(),
        title: T_C2,
        retired: false,
        seq,
    });
    seq += 1;

    // Fork b0 from main — after era two, before the branch's own act.
    db.fork(BranchId::new("b0").unwrap(), BranchId::main())
        .await
        .unwrap();

    // Act D — b0 supersedes c at V9, open: the W2 pair (version + cut).
    db.view(BranchId::new("b0").unwrap())
        .upsert_concept(
            ConceptUpsert::new("c", T_CB0)
                .supersede(true)
                .valid_from(v(9))
                .valid_to(OPEN),
        )
        .await
        .unwrap();
    let d = act_stamp(db).await;
    m.versions.push(VersionRow {
        node: "b0",
        id: "c",
        start: v(9),
        end: OPEN.into(),
        bound: d.clone(),
        title: T_CB0,
    });
    m.cuts.push(CutRow {
        node: "b0",
        id: "c",
        cut_at: v(9),
        bound: d.clone(),
    });

    // Fork-at b1 from main at act A's own stamp — the cutoff law's exact.
    db.fork_at(
        BranchId::new("b1").unwrap(),
        BranchId::main(),
        &m.mints[0].bound,
    )
    .await
    .unwrap();

    // Act E — b1 mints e (its own concept; W1's plain upsert on a branch).
    db.upsert_concept(
        ConceptUpsert::new("e", T_E1)
            .valid_from(v(2))
            .valid_to(v(8))
            .on_branch(BranchId::new("b1").unwrap()),
    )
    .await
    .unwrap();
    let e = act_stamp(db).await;
    m.mints.push(MintRow {
        node: "b1",
        id: "e",
        start: v(2),
        end: v(8),
        bound: e.clone(),
        title: T_E1,
        retired: false,
        seq,
    });
    seq += 1;

    // Act F — b1 supersedes c on its window `[V5, V6)`: the takeover's own
    // sentence, cut at V5.
    db.view(BranchId::new("b1").unwrap())
        .upsert_concept(
            ConceptUpsert::new("c", T_CB1)
                .supersede(true)
                .valid_from(v(5))
                .valid_to(v(6)),
        )
        .await
        .unwrap();
    let f = act_stamp(db).await;
    m.versions.push(VersionRow {
        node: "b1",
        id: "c",
        start: v(5),
        end: v(6),
        bound: f.clone(),
        title: T_CB1,
    });
    m.cuts.push(CutRow {
        node: "b1",
        id: "c",
        cut_at: v(5),
        bound: f.clone(),
    });

    // Fork b2 from b0 — after b1's acts, so its own are the youngest.
    db.fork(BranchId::new("b2").unwrap(), BranchId::new("b0").unwrap())
        .await
        .unwrap();

    // Act G — b2 supersedes c at V10, open (its inherited open starts at V9,
    // so the supersede only moves forward).
    db.view(BranchId::new("b2").unwrap())
        .upsert_concept(
            ConceptUpsert::new("c", T_CB2)
                .supersede(true)
                .valid_from(v(10))
                .valid_to(OPEN),
        )
        .await
        .unwrap();
    let g = act_stamp(db).await;
    m.versions.push(VersionRow {
        node: "b2",
        id: "c",
        start: v(10),
        end: OPEN.into(),
        bound: g.clone(),
        title: T_CB2,
    });
    m.cuts.push(CutRow {
        node: "b2",
        id: "c",
        cut_at: v(10),
        bound: g.clone(),
    });

    // Act H — b2 retires its inherited d at V5: the cut-only route.
    db.view(BranchId::new("b2").unwrap())
        .upsert_concept(
            ConceptUpsert::new("d", T_DCUT)
                .retired(true)
                .valid_from(v(5)),
        )
        .await
        .unwrap();
    let h = act_stamp(db).await;
    m.cuts.push(CutRow {
        node: "b2",
        id: "d",
        cut_at: v(5),
        bound: h.clone(),
    });

    // Act I — the trunk retires d at V11: the close pivots era one's open
    // window, the retired row takes it from there.
    db.upsert_concept(
        ConceptUpsert::new("d", T_DDEAD)
            .supersede(true)
            .retired(true)
            .valid_from(v(11)),
    )
    .await
    .unwrap();
    let i = act_stamp(db).await;
    m.mints.push(MintRow {
        node: "main",
        id: "d",
        start: v(3),
        end: v(11),
        bound: i.clone(),
        title: T_D1,
        retired: false,
        seq,
    });
    seq += 1;
    m.mints.push(MintRow {
        node: "main",
        id: "d",
        start: v(11),
        end: OPEN.into(),
        bound: i.clone(),
        title: T_DDEAD,
        retired: true,
        seq,
    });
    seq += 1;

    // Act K — the trunk supersedes c again at V12: era two's window pivots
    // (`[V7, V12)`), era three opens.
    db.upsert_concept(
        ConceptUpsert::new("c", T_C3)
            .supersede(true)
            .valid_from(v(12))
            .valid_to(OPEN),
    )
    .await
    .unwrap();
    let k = act_stamp(db).await;
    m.mints.push(MintRow {
        node: "main",
        id: "c",
        start: v(7),
        end: v(12),
        bound: k.clone(),
        title: T_C2,
        retired: false,
        seq,
    });
    seq += 1;
    m.mints.push(MintRow {
        node: "main",
        id: "c",
        start: v(12),
        end: OPEN.into(),
        bound: k.clone(),
        title: T_C3,
        retired: false,
        seq,
    });
    seq += 1;

    // Act L — the trunk retires c at V13: era three's window pivots to
    // `[V12, V13)`, the retired row takes the far end. No resurrection.
    db.upsert_concept(
        ConceptUpsert::new("c", T_CDEAD)
            .supersede(true)
            .retired(true)
            .valid_from(v(13)),
    )
    .await
    .unwrap();
    let l = act_stamp(db).await;
    m.mints.push(MintRow {
        node: "main",
        id: "c",
        start: v(12),
        end: v(13),
        bound: l.clone(),
        title: T_C3,
        retired: false,
        seq,
    });
    let _ = seq;
    m.mints.push(MintRow {
        node: "main",
        id: "c",
        start: v(13),
        end: OPEN.into(),
        bound: l.clone(),
        title: T_CDEAD,
        retired: true,
        seq: seq + 1,
    });
    let _ = seq;

    // The fork readback: the branches table's rows, each with the cutoff the
    // resolution law uses. b1's is the exact stamp act A issued.
    let mut rows = db
        .read_conn()
        .query(
            "SELECT branch_id, parent_id, forked_at FROM branches \
             WHERE parent_id IS NOT NULL ORDER BY branch_id",
            (),
        )
        .await
        .unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        let child: String = row.get(0).unwrap();
        let parent: String = row.get(1).unwrap();
        let cutoff: String = row.get(2).unwrap();
        m.forks.push(Fork {
            child,
            parent,
            cutoff,
        });
    }
    assert_eq!(m.forks.len(), 3, "the pour carved three lineages");
    let b1 = m.forks.iter().find(|f| f.child == "b1").expect("b1");
    assert_eq!(
        b1.cutoff, m.mints[0].bound,
        "fork_at(b1, main, A) takes A's exact stamp as the cutoff"
    );
}

/// The store's rows against the pour's commands: the overlay tables carry
/// every version and cut the model built (the act's stamp, the poured
/// window, the poured title), and the classic log holds the model's rows in
/// order. This is the fixture sanity the cells then depend on.
pub async fn reconcile(db: &std::sync::Arc<Database>, m: &Model) {
    let mut rows = db
        .read_conn()
        .query(
            "SELECT branch_id, id, valid_from, valid_to, title, recorded_at \
             FROM concept_versions ORDER BY branch_id, valid_from",
            (),
        )
        .await
        .unwrap();
    let mut seen = 0usize;
    while let Some(row) = rows.next().await.unwrap() {
        let bid: String = row.get(0).unwrap();
        let id: String = row.get(1).unwrap();
        let start: String = row.get(2).unwrap();
        let end: String = row.get(3).unwrap();
        let title: String = row.get(4).unwrap();
        let stamped: String = row.get(5).unwrap();
        let w = m
            .versions
            .iter()
            .find(|w| w.node == bid && w.id == id)
            .unwrap_or_else(|| {
                panic!("concept_versions holds {bid}/{id}, which the pour did not write")
            });
        assert_eq!(w.start, start, "bids {bid}/{id}: the pair's start");
        assert_eq!(w.end, end, "bids {bid}/{id}: the pair's end");
        assert_eq!(w.title, title, "bids {bid}/{id}: the pair's title");
        assert_eq!(
            w.bound, stamped,
            "bids {bid}/{id}: the pair's stamp is the act's own"
        );
        seen += 1;
    }
    assert_eq!(seen, m.versions.len(), "every version row the pour wrote");

    let mut rows = db
        .read_conn()
        .query(
            "SELECT branch_id, id, cut_at, recorded_at FROM concept_cuts \
             ORDER BY branch_id, cut_at",
            (),
        )
        .await
        .unwrap();
    let mut seen = 0usize;
    while let Some(row) = rows.next().await.unwrap() {
        let bid: String = row.get(0).unwrap();
        let id: String = row.get(1).unwrap();
        let cut_at: String = row.get(2).unwrap();
        let stamped: String = row.get(3).unwrap();
        let c = m
            .cuts
            .iter()
            .find(|c| c.node == bid && c.id == id)
            .unwrap_or_else(|| {
                panic!("concept_cuts holds {bid}/{id}, which the pour did not write")
            });
        assert_eq!(c.cut_at, cut_at, "bids {bid}/{id}: the cut's instant");
        assert_eq!(
            c.bound, stamped,
            "bids {bid}/{id}: the cut's stamp is the act's own"
        );
        seen += 1;
    }
    assert_eq!(seen, m.cuts.len(), "every cut row the pour wrote");

    let mut rows = db
        .read_conn()
        .query(
            "SELECT entity_id, branch_id, json_extract(payload, '$.valid_from'), \
             json_extract(payload, '$.valid_to'), json_extract(payload, '$.retired'), \
             json_extract(payload, '$.title') FROM transaction_log \
             WHERE table_name = 'concepts' ORDER BY seq_id",
            (),
        )
        .await
        .unwrap();
    let mut got = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        got.push((
            row.get::<String>(0).unwrap(),
            row.get::<String>(1).unwrap(),
            row.get::<String>(2).unwrap(),
            row.get::<String>(3).unwrap(),
            row.get::<i64>(4).unwrap() != 0,
            row.get::<String>(5).unwrap(),
        ));
    }
    let want: Vec<(String, String, String, String, bool, String)> = m
        .mints
        .iter()
        .map(|r| {
            (
                r.id.to_string(),
                r.node.to_string(),
                r.start.clone(),
                r.end.clone(),
                r.retired,
                r.title.to_string(),
            )
        })
        .collect();
    assert_eq!(
        want, got,
        "the classic log holds the pour's rows, in write order"
    );
}

// ---------------------------------------------------------------------------
// The helpers the tests drive
// ---------------------------------------------------------------------------

/// The fixture: open the harness database, pour, reconcile.
pub async fn hold(harness: &TestHarness) -> (std::sync::Arc<Database>, Model) {
    let db: std::sync::Arc<Database> =
        std::sync::Arc::new(Database::open(&harness.db_path).await.unwrap());
    let mut m = Model::default();
    pour(&db, &mut m).await;
    reconcile(&db, &m).await;
    (db, m)
}

/// The four lineages and three ids every grid runs over.
pub const VIEWERS: [&str; 4] = ["main", "b0", "b1", "b2"];
pub const IDS: [&str; 3] = ["c", "d", "e"];

/// Every act stamp the pour wrote plus pre-history: one bound per cell row.
pub fn act_bounds(m: &Model) -> Vec<String> {
    let mut bounds = vec![PRE.to_string()];
    bounds.extend(m.mints.iter().map(|r| r.bound.clone()));
    bounds.extend(m.versions.iter().map(|w| w.bound.clone()));
    bounds.extend(m.cuts.iter().map(|c| c.bound.clone()));
    bounds.sort();
    bounds.dedup();
    bounds
}

/// One `concepts_by_id` cell against the law, full-field.
pub async fn by_id_cell(
    db: &Database,
    m: &Model,
    viewer: &str,
    id: &str,
    at_v: Option<&str>,
    at_r: Option<&str>,
) {
    let mut plan = ReadPlan::new();
    if viewer != "main" {
        plan = plan.on(BranchId::new(viewer).unwrap());
    }
    if let Some(at) = at_v {
        plan = plan.valid_at(at.to_string());
    }
    if let Some(at) = at_r {
        plan = plan.recorded_at(at.to_string());
    }
    let got = db
        .concepts_by_id(&[id.to_string()], plan)
        .await
        .unwrap_or_else(|e| panic!("concepts_by_id({id} @ {viewer}, v={at_v:?}, r={at_r:?}): {e}"));
    let got = got.into_iter().next();
    let want = m.by_id(viewer, id, at_v, at_r);
    let label = format!("concepts_by_id({id} @ {viewer}, v={at_v:?}, r={at_r:?})");
    match (want, got) {
        (Some(w), Some(c)) => {
            assert_eq!(c.title, w.title, "{label}: title");
            assert_eq!(c.branch_id, w.bid, "{label}: the answering lineage");
            assert_eq!(
                c.valid_from.as_deref(),
                Some(w.start.as_str()),
                "{label}: valid_from"
            );
            assert_eq!(
                c.valid_to.as_deref(),
                Some(w.end.as_str()),
                "{label}: valid_to"
            );
            assert_eq!(c.recorded_at, w.stamped, "{label}: recorded_at");
        }
        (None, None) => {}
        (want, got) => panic!("{label}: the oracle says {want:?}, the reader answered {got:?}"),
    }
}

/// One whole-ledger cell against the law, title-or-absent.
pub fn state_cell(got: Option<String>, want: Option<&Answer>, label: String) {
    match (want, got) {
        (Some(w), Some(g)) => assert_eq!(g, w.title, "{label}: title"),
        (None, None) => {}
        (want, got) => panic!(
            "{label}: the oracle says {:?}, the reader answered {:?}",
            want.map(|w| w.title.clone()),
            got
        ),
    }
}

// ---------------------------------------------------------------------------
