//! W14.3: what does one `assert_edge` actually pay for, and how much of it was
//! the actor forgetting what it knew a moment ago?
//!
//! Review C-6 counts three round trips on the single-edge write — a `branches`
//! query for the lineage shape, a compile of the overlap guard, and a compile of
//! `INSERT_LINK` — and says they are "a visible fraction" of the ~0.8 ms
//! transaction floor. A fraction is not a number, and the fraction turned out to
//! depend on something the finding does not mention: whether the database has
//! ever been forked. Once it has, the guard compiles its **resolved** form, and
//! the compile is the largest single cost in the turn.
//!
//! # What it reports
//!
//! One line per fixture, `best` and `mean` over `--iterations` assertions
//! through the public surface — so the number includes the channel, the actor's
//! turn, the guard, the insert and its triggers, and the response. That is the
//! latency a caller waits on, which is the only figure the finding is about.
//!
//! Two fixtures, because the shape decides what the guard compiles:
//!
//! - **trunk** — never forked, so [`LineageShape::Trunk`] and the four-parameter
//!   guard.
//! - **forked** — one branch exists and the write is still on the trunk, which
//!   is `TrunkOnForked` and takes the resolved statement (0.15.2, D-244).
//!
//! # Reading it
//!
//! Measured on one Windows box, release build, 500 iterations, before and
//! after [D-248]:
//!
//! ```text
//!             before                       after
//!    trunk  best 0.1842  mean 0.2705   best 0.0991  mean 0.1772
//!   forked  best 0.4011  mean 0.5060   best 0.1056  mean 0.1950
//! ```
//!
//! and the components, timed separately on a connection with the same file:
//! the `branches` query 10.8 µs, the guard's compile 3.9 µs on the trunk and
//! **151 µs** forked, `INSERT_LINK`'s compile 61 µs. So the forked write spent
//! nearly half its time compiling a statement it had compiled on the previous
//! call, and the trunk write about a third.
//!
//! The row worth reading twice is `forked`. A database with one abandoned
//! experiment in it paid **2.2×** the trunk's latency on every single-edge
//! write, and after this it pays 1.07× — the fork's cost was almost entirely
//! the guard's compile, not the guard.
//!
//! `best` is the honest figure here rather than `mean`: this is a latency floor
//! question, the fixture is small enough to be entirely in cache, and the mean
//! carries the OS's scheduling noise on a box that is also compiling. Both are
//! printed, and a run where they disagree by more than about 2× is a run that
//! was sharing the machine — see D-070 before quoting either.
//!
//! [D-248]: ../docs/architecture/s13-decision-register.md#d-248
//! [`LineageShape::Trunk`]: crate

use std::time::Instant;

use macrame::branch::BranchId;
use macrame::graph::EdgeAssertion;
use macrame::{ConceptUpsert, Database};

const GENESIS: &str = "2020-01-01T00:00:00.000000Z";

fn edge(i: usize) -> EdgeAssertion {
    EdgeAssertion::new("hub", format!("t{i}"), "LINKS")
        .valid_from(format!("2026-01-01T00:00:00.{:06}Z", i % 1_000_000))
        .valid_to(format!("2026-01-01T00:00:00.{:06}Z", (i % 1_000_000) + 1))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let iterations: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(500);

    let dir = tempfile::tempdir()?;
    println!("assert_edge, {iterations} iterations, one target per assertion\n");

    for forked in [false, true] {
        let label = if forked { "forked" } else { "trunk" };
        let db = Database::open(dir.path().join(format!("{label}.db"))).await?;

        // One source and a distinct target per assertion: the edge key differs
        // every time, so no assertion is refused and none of them is the
        // *second* write to a key, which would measure the single-open trigger
        // rather than the write.
        db.upsert_concept(ConceptUpsert::new("hub", "hub").valid_from(GENESIS))
            .await?;
        for i in 0..iterations {
            db.upsert_concept(ConceptUpsert::new(format!("t{i}"), "t").valid_from(GENESIS))
                .await?;
        }
        if forked {
            db.fork(BranchId::new("exp")?, BranchId::new("main")?)
                .await?;
        }

        let mut best = f64::MAX;
        let mut total = 0.0;
        for i in 0..iterations {
            let start = Instant::now();
            db.assert_edge(edge(i)).await?;
            let ms = start.elapsed().as_secs_f64() * 1e3;
            best = best.min(ms);
            total += ms;
        }
        println!(
            "{label:>8}  assert_edge best {best:.4} ms  mean {:.4} ms",
            total / iterations as f64
        );
        db.close().await?;
    }
    Ok(())
}
