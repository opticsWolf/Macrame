//! Build the seed corpora for the three snapshot targets (0.13.14, W8.4,
//! D-187).
//!
//! ```text
//! cargo run --bin seed
//! cargo +nightly fuzz run snapshot_container
//! ```
//!
//! **Why this is a program and not a directory of committed files.** A checked-
//! in corpus of valid v3 snapshots is correct exactly until `SNAP_FORMAT_VERSION`
//! next moves, after which every seed is refused at the version check and the
//! session effectively starts from an empty corpus — while still looking
//! seeded, in a run whose only visible output is a coverage number nobody has a
//! baseline for. Generating them from the build under test cannot go stale, and
//! costs a second.
//!
//! Every seed is derived from a snapshot `save_snapshot` genuinely wrote: the
//! payload and the plaintext are read back out of the container rather than
//! reconstructed, so nothing here is a second description of the writer.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use macrame::temporal::as_of::NodeAttributes;
use macrame::temporal::replay::MaterializedState;
use macrame::temporal::snapshot::{fuzzing, save_snapshot};

/// Shapes worth starting from, rather than one arbitrary state.
///
/// Sizes and shapes a mutator will not stumble onto: an empty graph, one that
/// is all concepts and no edges, one that is all edges, one large enough that
/// zstd emits more than a single literal block. The point of a seed is to hand
/// the fuzzer structure it would otherwise have to invent.
fn states() -> Vec<MaterializedState> {
    let ts = "2026-08-24T12:00:00.000000Z".to_string();

    let concept = |i: u32| NodeAttributes {
        id: format!("c{i}"),
        title: format!("concept {i}"),
        content: format!("content for concept {i} ").repeat(4),
        embedding_model: (i % 3 == 0).then(|| "model-a".to_string()),
    };

    let build = |seq: i64, n_concepts: u32, n_edges: u32, predates: bool| {
        let mut concepts = HashMap::new();
        for i in 0..n_concepts {
            concepts.insert(format!("c{i}"), concept(i));
        }
        let edges = (0..n_edges)
            .map(|i| {
                (
                    format!("c{}", i % n_concepts.max(1)),
                    format!("c{}", (i + 1) % n_concepts.max(1)),
                    "relates_to".to_string(),
                    ts.clone(),
                    "A".to_string(),
                )
            })
            .collect();
        MaterializedState {
            seq_anchor: seq,
            timestamp: ts.clone(),
            concepts,
            edges,
            predates_recorded_history: predates,
        }
    };

    vec![
        build(1, 0, 0, true),
        build(2, 1, 0, false),
        build(3, 8, 12, false),
        build(4, 0, 0, false),
        build(5, 400, 900, false),
    ]
}

fn corpus_dir(target: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus")
        .join(target);
    fs::create_dir_all(&dir).expect("create corpus dir");
    dir
}

fn main() {
    let containers = corpus_dir("snapshot_container");
    let payloads = corpus_dir("snapshot_payload");
    let frames = corpus_dir("snapshot_frame");

    let mut written = 0usize;
    for state in states() {
        let seq = state.seq_anchor;

        // A real file, written by the real writer, straight into the corpus.
        let path = save_snapshot(&containers, &state).expect("save_snapshot");
        let container = fs::read(&path).expect("read back the snapshot");

        let (payload, plain_len) =
            fuzzing::payload_of(&container).expect("a file this build just wrote is a container");

        // `snapshot_frame` reads eight little-endian bytes of declared length
        // and takes the rest as the payload — the split its own docs describe.
        let mut frame = plain_len.to_le_bytes().to_vec();
        frame.extend_from_slice(payload);
        fs::write(frames.join(format!("{seq:03}.frame")), &frame).expect("write frame seed");

        // `snapshot_payload` reads plaintext. Decompressed out of the container
        // rather than re-serialized, so it is what the writer produced.
        let plain = zstd::decode_all(payload).expect("the writer's own payload");
        assert_eq!(plain.len() as u64, plain_len, "the container disagrees with itself");
        fs::write(payloads.join(format!("{seq:03}.plain")), &plain).expect("write payload seed");

        written += 1;
    }

    println!("seeded {written} states into:");
    for dir in [&containers, &payloads, &frames] {
        println!("  {}", dir.display());
    }
}
