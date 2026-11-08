//! The K2 gate (0.22.0, [Amendment 2](../docs/Macrame%20Update%20Plan%20v0.22.0%20-%20Amendment%202.md)
//! §5.4 cost 1; the P-2 probe's reader census).
//!
//! The census classifies every statement in `src/` that reads or mutates the
//! `concepts` table: identity rows (the mint answers), content rows (the
//! §5.3 resolution), write machinery, and DDL. The gate pins the count so a
//! new site cannot slip in unclassified — when this test fails, name the
//! new row's class in `docs/probes/m6-P2-reader-classification.md`, update the
//! document's counts, and update this pin.
//!
//! Pinned at the M6-b continuation's recount: 37 sites in 12 files. M6-a's
//! recount was 34; the post-merge review's write-side correction added the
//! route's log-first creation read, the state-at-cap hybrid (a table row at
//! the cap, the pair's newest log row otherwise), and the interval read's
//! own hybrids — all write machinery, classified in the P-2 doc.

use std::path::{Path, PathBuf};

const PINNED: usize = 37;

/// The statement shapes the census counts (rows of `concepts` in `FROM`,
/// `JOIN`, `INTO` and `UPDATE` positions, word-bounded as the probe's grep
/// ran). Rustdoc lines count too — row 1 of the census is one.
fn census(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    for keyword in [
        "from concepts",
        "join concepts",
        "into concepts",
        "update concepts",
    ] {
        let mut from = 0;
        while let Some(i) = lower[from..].find(keyword) {
            let end = from + i + keyword.len();
            let boundary = lower[end..]
                .chars()
                .next()
                .map(|c| !(c == '_' || c.is_ascii_alphabetic()))
                .unwrap_or(true);
            if boundary {
                return true;
            }
            from = end;
        }
    }
    false
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().map(|x| x == "rs").unwrap_or(false) {
                out.push(p);
            }
        }
    }
}

#[test]
fn the_concept_reader_census_stays_classified() {
    let mut files = Vec::new();
    collect(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(!files.is_empty(), "no sources found");

    let mut per_file: Vec<String> = Vec::new();
    let mut total = 0usize;
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        let count = text.lines().filter(|l| census(l)).count();
        total += count;
        if count > 0 {
            per_file.push(format!(
                "{}: {count}",
                f.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            ));
        }
    }
    per_file.sort();
    assert_eq!(
        per_file.len(),
        12,
        "the census's file count moved; see the doc"
    );

    assert_eq!(
        total,
        PINNED,
        "a `concepts` read or write appeared in src/ beyond the pinned census. \
         Name its class in docs/probes/m6-P2-reader-classification.md — identity \
         (the mint answers), content (§5.3's resolution; this test is the K2 \
         wedge), write machinery, or DDL — update the document's counts, and \
         update this pin. Current census:\n{}",
        per_file.join("\n")
    );
}
