//! What the write actor knows about its own latency (T1.4, D-079).
//!
//! # Why this exists
//!
//! [`crate::CHUNK_BUDGET`] is 3 ms and the crate has, until now, had exactly one
//! way to find out whether that bound holds: run `benches/budgets.rs` on a
//! synthetic fixture. That is a statement about a laptop, not about a database
//! in use. D-059 already established that the bound does **not** hold on a large
//! file, by a factor of 15, and it took a benchmark rewrite to notice — because
//! nothing in the running system was counting.
//!
//! Tier 1's other three items are all "make the tail bounded". None of them can
//! be validated in the field without something that measures the tail, which is
//! why this is a precondition for them rather than a nice-to-have.
//!
//! # What is recorded, and what is deliberately not
//!
//! Four things, all of them per **actor turn** — one command, start to finish:
//!
//! - **queue depth** on both channels, sampled *before* the turn begins;
//! - **hold duration**, bucketed, per command kind;
//! - **holds over budget**, counted separately per kind;
//! - **the longest hold since open**, with the kind that caused it.
//!
//! The hold is the whole turn, not the `execute` call's SQL. That is the
//! quantity the budget is about: the SQLite write lock is not preemptible, so an
//! interactive assertion arriving mid-turn waits for the turn, whatever the turn
//! spent its time on.
//!
//! There is no per-command timestamp trail and no sampling of individual slow
//! commands. That would be a tracing problem, and `tracing` is already a
//! dependency — spans belong there. This module answers one question ("is the
//! bound holding, and if not, which kind breaks it") in fixed memory, with no
//! allocation on the actor's path.
//!
//! # The feature gate
//!
//! Behind `metrics`, off by default. With the feature off, [`ActorMetrics`] is a
//! zero-sized type whose methods compile away and [`HoldTimer::start`] does not
//! read the clock — so the actor loop has **one** shape either way. That
//! matters more than the nanoseconds: a `#[cfg]` in the loop body is how the
//! instrumented and uninstrumented paths drift until only one of them is the one
//! that runs.

use std::time::Duration;

/// The command kinds the actor can spend a turn on.
///
/// One flat enum across both channels rather than one per channel. The question
/// this exists to answer is "which command broke the budget", and a reader
/// looking at a 400 ms hold does not first want to know which queue it came off.
/// Priority is a property of scheduling; kind is a property of cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum CommandKind {
    AssertEdge,
    RetireEdge,
    UpsertConcept,
    WriteBulkAtomic,
    RebuildCurrent,
    RegisterModel,
    Shutdown,
    BulkImportChunk,
    WriteConceptsChunk,
    WriteAnalyticsChunk,
    UpsertEmbeddingChunk,
    Archive,
    RebuildFts,
    /// One step of a chunked shadow rebuild (T1.2). Its own kind rather than
    /// folded into `RebuildCurrent`, because the two have opposite latency
    /// profiles and the whole point of the chunked path is that its turns are
    /// short — averaging them together would hide exactly the improvement.
    ShadowRebuild,
}

impl CommandKind {
    /// Every kind, in declaration order. Indexing into the per-kind arrays is by
    /// position in this slice, so the two must not drift — which is why the
    /// arrays are sized from `ALL.len()` rather than from a hand-written count.
    pub const ALL: &'static [CommandKind] = &[
        CommandKind::AssertEdge,
        CommandKind::RetireEdge,
        CommandKind::UpsertConcept,
        CommandKind::WriteBulkAtomic,
        CommandKind::RebuildCurrent,
        CommandKind::RegisterModel,
        CommandKind::Shutdown,
        CommandKind::BulkImportChunk,
        CommandKind::WriteConceptsChunk,
        CommandKind::WriteAnalyticsChunk,
        CommandKind::UpsertEmbeddingChunk,
        CommandKind::Archive,
        CommandKind::RebuildFts,
        CommandKind::ShadowRebuild,
    ];

    pub const COUNT: usize = CommandKind::ALL.len();

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            CommandKind::AssertEdge => "assert_edge",
            CommandKind::RetireEdge => "retire_edge",
            CommandKind::UpsertConcept => "upsert_concept",
            CommandKind::WriteBulkAtomic => "write_bulk_atomic",
            CommandKind::RebuildCurrent => "rebuild_current",
            CommandKind::RegisterModel => "register_model",
            CommandKind::Shutdown => "shutdown",
            CommandKind::BulkImportChunk => "bulk_import_chunk",
            CommandKind::WriteConceptsChunk => "write_concepts_chunk",
            CommandKind::WriteAnalyticsChunk => "write_analytics_chunk",
            CommandKind::UpsertEmbeddingChunk => "upsert_embedding_chunk",
            CommandKind::Archive => "archive",
            CommandKind::RebuildFts => "rebuild_fts",
            CommandKind::ShadowRebuild => "shadow_rebuild",
        }
    }

    /// Whether this kind is exempt from [`crate::CHUNK_BUDGET`] by contract.
    ///
    /// The three exemptions are the table in `CHUNK_BUDGET`'s own rustdoc, and
    /// they are carried here so a dashboard can separate "the budget is being
    /// broken" from "the budget does not apply and never claimed to". Counting
    /// an `archive` as a budget violation would make the violation count useless
    /// on any database that archives.
    ///
    /// [`CommandKind::ShadowRebuild`] is deliberately **not** exempt. Its fill
    /// chunks are meant to fit the budget and its swap turn is not going to —
    /// the swap rebuilds three indexes under the lock, which is the residual
    /// cost T1.2 could not remove. Both facts are worth seeing, and exempting
    /// the kind would hide the first to excuse the second.
    pub const fn exempt_from_budget(self) -> bool {
        matches!(
            self,
            CommandKind::WriteBulkAtomic | CommandKind::Archive | CommandKind::RebuildCurrent
        )
    }
}

impl std::fmt::Display for CommandKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Upper bounds of the hold-duration histogram, in microseconds.
///
/// `3_000` is [`crate::CHUNK_BUDGET`] exactly, so the bucket boundary and the
/// bound are the same number and a reader does not have to interpolate to answer
/// "what fraction of turns fit". The tail runs to 1 s because D-059's measured
/// worst case was 45 ms and `rebuild_current` at 40K rows is 318 ms (D-077) —
/// a range this has to cover without saturating.
///
/// Anything above the last bound lands in the overflow bucket, which is why
/// [`KindSnapshot::buckets`] is one longer than this slice.
pub const BUCKET_BOUNDS_MICROS: &[u64] = &[
    100, 300, 1_000, 3_000, 10_000, 30_000, 100_000, 300_000, 1_000_000,
];

/// Number of histogram buckets, including the overflow bucket.
pub const BUCKET_COUNT: usize = BUCKET_BOUNDS_MICROS.len() + 1;

#[allow(dead_code)] // used by `imp` under `metrics`, and by the tests always
fn bucket_of(micros: u64) -> usize {
    // Linear scan over nine bounds. A binary search here would be slower in
    // practice and this runs once per actor turn, against a turn measured in
    // microseconds at best.
    BUCKET_BOUNDS_MICROS
        .iter()
        .position(|&bound| micros <= bound)
        .unwrap_or(BUCKET_BOUNDS_MICROS.len())
}

/// Times one actor turn.
///
/// With the `metrics` feature off this reads no clock and holds nothing: the
/// call site is identical, the cost is not paid. That is the whole reason it is
/// a type rather than a bare `Instant::now()` in the loop.
pub struct HoldTimer {
    #[cfg(feature = "metrics")]
    start: std::time::Instant,
}

impl HoldTimer {
    #[inline]
    pub fn start() -> Self {
        Self {
            #[cfg(feature = "metrics")]
            start: std::time::Instant::now(),
        }
    }

    #[inline]
    pub fn elapsed(&self) -> Duration {
        #[cfg(feature = "metrics")]
        {
            self.start.elapsed()
        }
        #[cfg(not(feature = "metrics"))]
        {
            Duration::ZERO
        }
    }
}

// ---------------------------------------------------------------------------
// Instrumented implementation
// ---------------------------------------------------------------------------

#[cfg(feature = "metrics")]
mod imp {
    use super::{bucket_of, CommandKind, BUCKET_COUNT};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// One kind's counters. All `Relaxed`: these are statistics, and ordering
    /// them against each other would buy a consistency no reader needs and cost
    /// fences on the write path.
    #[derive(Debug, Default)]
    struct Kind {
        turns: AtomicU64,
        total_micros: AtomicU64,
        over_budget: AtomicU64,
        /// This kind's own high-water mark, in µs.
        ///
        /// Not redundant with the global `longest`. That one names a single
        /// command, so on any real database it names whichever kind is slowest
        /// overall — and the question "did windowing shrink the archive's worst
        /// hold" cannot be answered by a counter that a bulk import wins. No
        /// packing needed here: the kind is the array index.
        longest_micros: AtomicU64,
        buckets: [AtomicU64; BUCKET_COUNT],
    }

    /// Live counters, shared between the actor and the handle.
    ///
    /// Fixed size, no allocation, no lock. The actor updates; anyone may read.
    #[derive(Debug, Default)]
    pub struct ActorMetrics {
        kinds: [Kind; CommandKind::COUNT],
        /// Packed `micros << 8 | kind`, so the longest hold and the kind that
        /// caused it are read and written as **one** value. Two atomics would
        /// let a reader see a duration from one turn beside a kind from
        /// another — a rare wrong answer to exactly the question this field
        /// exists to answer. 2^56 µs is over two thousand years.
        ///
        /// **The duration must occupy the high bits.** The update is a
        /// `fetch_max` on the packed word, so whichever field is packed high is
        /// the one being compared. The first version of this had the kind up
        /// there, which made the "longest hold" the hold with the largest
        /// *enum index* — a 3 ms `write_concepts_chunk` beat a 10 ms
        /// `rebuild_current` because its variant is declared later. It was
        /// `actor_metrics_tests` that caught it, not the unit tests, because
        /// nothing in the arithmetic is wrong: the packing is only incorrect in
        /// the presence of the atomic operation it exists to serve.
        longest: AtomicU64,
        /// Loop iterations, which is **not** the number of turns taken.
        ///
        /// The depth sample happens at the top of the loop, before `select!`
        /// blocks — so an idle actor has already counted the iteration for a
        /// command that has not arrived. That is right for depth (the sample is
        /// "what was queued when I went looking") and wrong for turns, which is
        /// why [`MetricsSnapshot::turns`] is the sum of the per-kind counters
        /// instead. Conflating the two made `turns` permanently one too high and
        /// disagree with its own breakdown.
        depth_samples: AtomicU64,
        high_depth_sum: AtomicU64,
        high_depth_max: AtomicU64,
        low_depth_sum: AtomicU64,
        low_depth_max: AtomicU64,
    }

    const MICROS_SHIFT: u32 = 8;
    const KIND_MASK: u64 = (1 << MICROS_SHIFT) - 1;

    impl ActorMetrics {
        pub fn new() -> Self {
            Self::default()
        }

        /// Sample both queue depths. Called before the turn, not after: after
        /// the turn the queue reflects what arrived *during* it, which is a
        /// different and much less useful quantity.
        #[inline]
        pub fn record_turn(&self, high_depth: usize, low_depth: usize) {
            self.depth_samples.fetch_add(1, Ordering::Relaxed);
            for (sum, max, depth) in [
                (&self.high_depth_sum, &self.high_depth_max, high_depth as u64),
                (&self.low_depth_sum, &self.low_depth_max, low_depth as u64),
            ] {
                sum.fetch_add(depth, Ordering::Relaxed);
                max.fetch_max(depth, Ordering::Relaxed);
            }
        }

        #[inline]
        pub fn record_hold(&self, kind: CommandKind, held: Duration) {
            let micros = held.as_micros().min(super::MICROS_CEILING as u128) as u64;
            let k = &self.kinds[kind.index()];
            k.turns.fetch_add(1, Ordering::Relaxed);
            k.total_micros.fetch_add(micros, Ordering::Relaxed);
            k.buckets[bucket_of(micros)].fetch_add(1, Ordering::Relaxed);
            k.longest_micros.fetch_max(micros, Ordering::Relaxed);
            if !kind.exempt_from_budget() && held > crate::CHUNK_BUDGET {
                k.over_budget.fetch_add(1, Ordering::Relaxed);
            }
            self.longest
                .fetch_max((micros << MICROS_SHIFT) | kind.index() as u64, Ordering::Relaxed);
        }

        /// A consistent-enough picture for a dashboard.
        ///
        /// Not a torn-read-free snapshot, and it does not pretend to be: the
        /// actor keeps running while this walks the array, so two kinds may be
        /// read one turn apart. Locking the actor to produce a report would make
        /// the observer a source of the latency it is measuring.
        pub fn snapshot(&self) -> super::MetricsSnapshot {
            let samples = self.depth_samples.load(Ordering::Relaxed);
            let mean = |sum: &AtomicU64| {
                if samples == 0 {
                    0.0
                } else {
                    sum.load(Ordering::Relaxed) as f64 / samples as f64
                }
            };

            let packed = self.longest.load(Ordering::Relaxed);
            let longest_micros = packed >> MICROS_SHIFT;
            let longest = (longest_micros > 0)
                .then(|| {
                    let idx = (packed & KIND_MASK) as usize;
                    CommandKind::ALL
                        .get(idx)
                        .map(|&kind| (kind, Duration::from_micros(longest_micros)))
                })
                .flatten();

            let kinds: Vec<_> = CommandKind::ALL
                .iter()
                .map(|&kind| {
                    let k = &self.kinds[kind.index()];
                    let turns = k.turns.load(Ordering::Relaxed);
                    let total = k.total_micros.load(Ordering::Relaxed);
                    super::KindSnapshot {
                        kind,
                        turns,
                        over_budget: k.over_budget.load(Ordering::Relaxed),
                        mean: total
                            .checked_div(turns)
                            .map_or(Duration::ZERO, Duration::from_micros),
                        longest: Duration::from_micros(
                            k.longest_micros.load(Ordering::Relaxed),
                        ),
                        buckets: std::array::from_fn(|i| k.buckets[i].load(Ordering::Relaxed)),
                    }
                })
                .collect();

            super::MetricsSnapshot {
                // Summed, not counted separately — see `depth_samples`.
                turns: kinds.iter().map(|k| k.turns).sum(),
                depth_samples: samples,
                high_depth_mean: mean(&self.high_depth_sum),
                high_depth_max: self.high_depth_max.load(Ordering::Relaxed),
                low_depth_mean: mean(&self.low_depth_sum),
                low_depth_max: self.low_depth_max.load(Ordering::Relaxed),
                longest,
                kinds,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// No-op implementation
// ---------------------------------------------------------------------------

#[cfg(not(feature = "metrics"))]
mod imp {
    use super::CommandKind;
    use std::time::Duration;

    /// The `metrics`-off shape: zero-sized, and every method is nothing.
    #[derive(Debug, Default)]
    pub struct ActorMetrics;

    impl ActorMetrics {
        pub fn new() -> Self {
            Self
        }
        #[inline]
        pub fn record_turn(&self, _high_depth: usize, _low_depth: usize) {}
        #[inline]
        pub fn record_hold(&self, _kind: CommandKind, _held: Duration) {}
    }
}

pub use imp::ActorMetrics;

/// Saturation point for a recorded hold, in microseconds (~2,000 years).
///
/// Exists so the packed `longest` field cannot have a pathological duration
/// overflow into the kind bits. A hold this long is not a measurement, it is a
/// hang — and the counter should stay readable rather than start reporting the
/// wrong command.
///
/// Kept out of the `metrics` cfg so the invariant test below runs in the default
/// build too: the packing is a property of the layout, and a build that does not
/// record is exactly the build where nobody would notice it break.
#[allow(dead_code)]
const MICROS_CEILING: u64 = (1u64 << 56) - 1;

/// One command kind's holds, as of the moment [`ActorMetrics::snapshot`] read it.
#[cfg(feature = "metrics")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindSnapshot {
    pub kind: CommandKind,
    /// Turns spent on this kind.
    pub turns: u64,
    /// Turns that exceeded [`crate::CHUNK_BUDGET`]. Always 0 for the three
    /// kinds [`CommandKind::exempt_from_budget`] names — see there for why.
    pub over_budget: u64,
    pub mean: Duration,
    /// This kind's longest hold. Distinct from [`MetricsSnapshot::longest`],
    /// which names one command across all kinds and so tends to be permanently
    /// whichever kind is slowest overall.
    pub longest: Duration,
    /// Counts per [`BUCKET_BOUNDS_MICROS`], plus a final overflow bucket.
    pub buckets: [u64; BUCKET_COUNT],
}

/// What the actor has done since the database was opened.
#[cfg(feature = "metrics")]
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsSnapshot {
    /// Commands executed, i.e. the sum of [`KindSnapshot::turns`]. The two agree
    /// by construction rather than by coincidence.
    pub turns: u64,
    /// Loop iterations that took a queue-depth reading. Always at least
    /// `turns + 1` on a live actor, because the reading is taken on the way in
    /// to a `select!` that has not resolved yet. This is the denominator of the
    /// two means below, and it is exposed so the difference is visible rather
    /// than looking like drift.
    pub depth_samples: u64,
    pub high_depth_mean: f64,
    pub high_depth_max: u64,
    pub low_depth_mean: f64,
    pub low_depth_max: u64,
    /// The longest hold since open and what caused it. `None` before the first
    /// turn, and — honestly — also when every turn so far took under a
    /// microsecond, which on this path does not happen.
    pub longest: Option<(CommandKind, Duration)>,
    pub kinds: Vec<KindSnapshot>,
}

#[cfg(feature = "metrics")]
impl MetricsSnapshot {
    /// Kinds that broke the budget, worst first. The one-line answer to "is the
    /// 3 ms bound holding?".
    pub fn budget_violations(&self) -> Vec<&KindSnapshot> {
        let mut v: Vec<_> = self.kinds.iter().filter(|k| k.over_budget > 0).collect();
        v.sort_by_key(|k| std::cmp::Reverse(k.over_budget));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_indexes_to_its_own_slot() {
        for (i, &kind) in CommandKind::ALL.iter().enumerate() {
            assert_eq!(kind.index(), i, "{kind} is out of order in ALL");
        }
        assert_eq!(CommandKind::COUNT, CommandKind::ALL.len());
    }

    /// The budget is a bucket boundary, not a value inside one — so "fits in the
    /// budget" is a prefix sum and needs no interpolation.
    #[test]
    fn the_chunk_budget_is_exactly_a_bucket_boundary() {
        let budget = crate::CHUNK_BUDGET.as_micros() as u64;
        assert!(
            BUCKET_BOUNDS_MICROS.contains(&budget),
            "CHUNK_BUDGET is {budget} µs, which is not a bucket bound: \
             {BUCKET_BOUNDS_MICROS:?}"
        );
        assert_eq!(bucket_of(budget), bucket_of(budget - 1));
        assert_eq!(bucket_of(budget + 1), bucket_of(budget) + 1);
    }

    #[test]
    fn the_overflow_bucket_catches_everything_past_the_last_bound() {
        let last = *BUCKET_BOUNDS_MICROS.last().unwrap();
        assert_eq!(bucket_of(last), BUCKET_BOUNDS_MICROS.len() - 1);
        assert_eq!(bucket_of(last + 1), BUCKET_COUNT - 1);
        assert_eq!(bucket_of(u64::MAX), BUCKET_COUNT - 1);
    }

    /// The packing is the reason `longest` is one atomic: duration high, kind
    /// low, so a `fetch_max` on the word compares the duration.
    #[test]
    fn the_packing_leaves_room_for_both_fields() {
        assert!(
            (CommandKind::COUNT as u64) <= 0xFF,
            "the kind index must fit in the low 8 bits"
        );
        // The ceiling must survive being shifted up by the kind's width.
        assert_eq!(MICROS_CEILING.checked_shl(8), Some(MICROS_CEILING << 8));
        assert_eq!((MICROS_CEILING << 8) >> 8, MICROS_CEILING);
    }

    #[cfg(feature = "metrics")]
    #[test]
    fn the_longest_hold_names_the_command_that_caused_it() {
        let m = ActorMetrics::new();
        m.record_hold(CommandKind::AssertEdge, Duration::from_micros(500));
        m.record_hold(CommandKind::Archive, Duration::from_millis(40));
        m.record_hold(CommandKind::UpsertConcept, Duration::from_micros(900));

        let snap = m.snapshot();
        assert_eq!(
            snap.longest,
            Some((CommandKind::Archive, Duration::from_millis(40)))
        );
    }

    /// The regression the packing bug produced: a *short* hold of a
    /// later-declared kind must not outrank a long hold of an earlier one.
    ///
    /// The test above does not catch it, because `Archive` happens to be both
    /// the longest hold and a high enum index — which is exactly why the first
    /// version of the packing shipped past it. Here the two orderings disagree.
    #[cfg(feature = "metrics")]
    #[test]
    fn a_later_declared_kind_does_not_outrank_a_longer_hold() {
        let long = CommandKind::AssertEdge; // index 0
        let short = CommandKind::RebuildFts; // last index
        assert!(short.index() > long.index(), "the fixture needs the gap");

        let m = ActorMetrics::new();
        m.record_hold(long, Duration::from_millis(40));
        m.record_hold(short, Duration::from_micros(1));

        assert_eq!(
            m.snapshot().longest,
            Some((long, Duration::from_millis(40))),
            "the max is being taken over the kind index, not the duration"
        );
    }

    /// The three contractual exemptions must not show up as violations, or the
    /// violation count is noise on any database that archives.
    #[cfg(feature = "metrics")]
    #[test]
    fn an_exempt_kind_over_budget_is_not_a_violation() {
        let m = ActorMetrics::new();
        m.record_hold(CommandKind::Archive, Duration::from_millis(40));
        m.record_hold(CommandKind::AssertEdge, Duration::from_millis(40));

        let snap = m.snapshot();
        let violations = snap.budget_violations();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].kind, CommandKind::AssertEdge);
        assert_eq!(violations[0].over_budget, 1);

        // But the hold is still *recorded* — exempt means "not a violation",
        // not "not measured". A 40 ms archive is exactly what T1.1 exists to
        // shrink, and it cannot be shrunk if it is not counted.
        let archive = snap
            .kinds
            .iter()
            .find(|k| k.kind == CommandKind::Archive)
            .unwrap();
        assert_eq!(archive.turns, 1);
        assert_eq!(archive.mean, Duration::from_millis(40));
    }

    #[cfg(feature = "metrics")]
    #[test]
    fn queue_depth_is_a_mean_and_a_high_water_mark() {
        let m = ActorMetrics::new();
        m.record_turn(0, 4);
        m.record_turn(10, 0);

        let snap = m.snapshot();
        // No command ran, so `turns` is 0 while `depth_samples` is 2. The two
        // counters are different facts and this is the case that shows it.
        assert_eq!(snap.turns, 0);
        assert_eq!(snap.depth_samples, 2);
        assert_eq!(snap.high_depth_mean, 5.0);
        assert_eq!(snap.high_depth_max, 10);
        assert_eq!(snap.low_depth_mean, 2.0);
        assert_eq!(snap.low_depth_max, 4);
    }
}
