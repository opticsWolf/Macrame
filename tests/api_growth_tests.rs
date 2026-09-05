//! Every public struct that can grow a field, and what stops the growth from
//! being a major version (0.15.13, W15.3, review C-11,
//! [D-255](../docs/architecture/s13-decision-register.md#d-255)).
//!
//! # What this replaces
//!
//! Until this release `tuning_tests` carried a call that named every field of
//! `Tuning` on purpose, because [D-155](../docs/architecture/s13-decision-register.md#d-155)
//! had chosen `Default` over `#[non_exhaustive]` and wanted the cost of that
//! choice visible: an exhaustive literal breaks when a field is added. It duly
//! broke three times. W15.3 made the literal illegal, which removes the
//! demonstration along with the defect — so the property moves here, where it
//! is asserted rather than exhibited, and where it covers every struct on the
//! surface instead of one.
//!
//! # The two failure modes, which point in opposite directions
//!
//! A public struct with public fields and no `#[non_exhaustive]` cannot gain a
//! field after 1.0 without a major version, because a caller's literal
//! enumerates the fields it had. That is C-11.
//!
//! The attribute's own failure mode is the mirror image: on a struct nothing
//! else can build, it makes the type **unconstructible** outside this crate,
//! and silently — the crate keeps compiling, because a literal inside the
//! defining crate is still legal. Writing this release hit that three times,
//! on `Overlap`, `NodeAttributes` and `MaterializedState`, each caught only
//! because a test or the Python binding happened to fabricate one. So the
//! registry records not just that a struct is growable but *how a caller gets
//! one*, and both directions are checked.
//!
//! # Why the baseline rather than the source
//!
//! `docs/architecture/public-api.txt` is the surface as `cargo-public-api`
//! reports it, which is where a re-export makes one type visible under three
//! paths and where a field is distinguishable from a method of the same name.
//! Parsing `src/**` for `pub struct` would answer a question about the text
//! instead of about the API — and the baseline is blessed in the same commit
//! as the change it describes, so it is never behind the code by more than a
//! review ([D-205](../docs/architecture/s13-decision-register.md#d-205)).
//!
//! The one thing the baseline cannot see is in [`SOURCES`].
//!
//! # What the baseline could not see either, found by mutating
//!
//! Two of the three properties above are properties of the *record*. Delete
//! `#[non_exhaustive]` from a struct in `src/` and every assertion in this
//! file still passes, because the baseline still carries it: what notices is
//! `scripts/check_public_api.py`, which is deliberately not a test (it needs a
//! nightly toolchain and `cargo-public-api`). The claim above — that the
//! baseline is never behind the code by more than a review — is true of a
//! release and false of a working tree, and a mutation lives in a working
//! tree. So the attribute is checked against the source as well, in
//! [`every_registered_struct_carries_the_attribute_in_its_own_source`].
//!
//! The other gap was in the setter contract. `has_method` asks whether a
//! method of the right *name* is on the surface; nothing asked whether it
//! assigns the field it is named after. Two mutations survived the whole
//! suite on that — `writer_cache_size` writing into `reader_cache_size`, and
//! `poll_interval` assigning nothing — and the first is a failure mode the
//! struct literal this release forbade could not have had, because a literal
//! names each field exactly once. That is
//! [`every_setter_assigns_the_field_it_is_named_after`], which is the only
//! test here that constructs anything.

const BASELINE: &str = include_str!("../docs/architecture/public-api.txt");

/// The way a caller gets one.
///
/// [`Ctor::Derived`] exists because `check_public_api.py` omits auto-derived
/// impls — a baseline that moved when a *dependency* changed is a baseline
/// people learn to bless without reading ([D-205]). The cost is that a derived
/// `Default` is invisible there, and for `Tuning` that is the **only** way to
/// build one: the surface as recorded shows six setters on a struct with no
/// public constructor at all. Read from the source instead, in [`SOURCES`].
///
/// [D-205]: ../docs/architecture/s13-decision-register.md#d-205
#[derive(Debug, Clone, Copy)]
enum Ctor {
    /// An inherent method, which the baseline lists.
    Named(&'static str),
    /// `Default::default`, derived.
    Derived,
}

/// How a caller gets one, which is what decides what has to be checked.
#[derive(Debug, Clone, Copy)]
enum Entry {
    /// The crate builds it and the caller reads it. `#[non_exhaustive]` costs
    /// such a struct nothing: what it forbids — the literal and the exhaustive
    /// destructure — are forms a caller of a *report* has no reason to write,
    /// and the `..` that fixes the second is one token.
    Returned,
    /// A caller builds one, through the named entry point. Every field is
    /// either an argument of it or assigned afterwards, which `pub` fields on
    /// a value you own still allow.
    Built { ctor: Ctor },
    /// A caller builds one *and* the type's contract is that each field is
    /// settable by name. The fields supplied at construction are listed; every
    /// other one must have a method named after it, or an explicit rename.
    Tuned {
        ctor: Ctor,
        at_entry: &'static [&'static str],
        renamed: &'static [(&'static str, &'static str)],
    },
}

struct Growable {
    /// The type's own name. Re-export paths are found from it, because a type
    /// the prelude carries is three lines in the baseline and one decision.
    name: &'static str,
    entry: Entry,
    /// Why it is in the category it is in — the part a later reader needs and
    /// cannot recover from the code.
    why: &'static str,
}

/// Every public struct in the crate that has public fields.
///
/// The first two tests below are what keep this list honest in both
/// directions: a struct added to the surface with a public field and no entry
/// here fails, and so does an entry naming a struct the surface no longer has.
const REGISTRY: &[Growable] = &[
    // --- the three C-11 named -------------------------------------------
    Growable {
        name: "Tuning",
        entry: Entry::Tuned {
            ctor: Ctor::Derived,
            at_entry: &[],
            renamed: &[],
        },
        why: "the struct whose documented purpose is to keep acquiring knobs — \
              W5.3, W5.4 and W7.4 each added one. D-155 shipped it with Default \
              and no attribute; D-255 pays the price D-155 named and declined.",
    },
    Growable {
        name: "SnapshotCadence",
        entry: Entry::Tuned {
            ctor: Ctor::Derived,
            at_entry: &[],
            renamed: &[],
        },
        why: "two knobs and a Default, taken by three public constructors; the \
              retention policy §5.5 leaves unspecified is the field most likely \
              to arrive next.",
    },
    Growable {
        name: "TraversalBuilder",
        entry: Entry::Tuned {
            ctor: Ctor::Named("new"),
            at_entry: &["start_node"],
            renamed: &[("branch", "on_branch")],
        },
        why: "already had a setter for every field, so C-11 costs it the \
              attribute alone. `limit` arrived at 0.15.10 and `as_of_recorded` \
              at 0.13.2, both additive only because nobody outside had written \
              the literal.",
    },
    // --- the same shape, on structs that already carried the attribute ---
    Growable {
        name: "ConceptUpsert",
        entry: Entry::Tuned {
            ctor: Ctor::Named("new"),
            at_entry: &["id", "title"],
            renamed: &[("branch", "on_branch")],
        },
        why: "`#[non_exhaustive]` since before W15.3; listed here so the \
              setter-per-field contract is checked rather than assumed.",
    },
    Growable {
        name: "EdgeAssertion",
        entry: Entry::Tuned {
            ctor: Ctor::Named("new"),
            at_entry: &["source", "target", "edge_type"],
            renamed: &[("branch", "on_branch")],
        },
        why: "as `ConceptUpsert`, and the write path's other half.",
    },
    Growable {
        name: "ReadPlan",
        entry: Entry::Tuned {
            ctor: Ctor::Named("new"),
            at_entry: &[],
            renamed: &[
                ("branch", "on"),
                ("valid", "valid_at"),
                ("recorded", "recorded_at"),
            ],
        },
        why: "D-251 gave it the attribute at birth so a fourth qualifier would \
              be additive, and D-252 added one a release later. The renames are \
              deliberate: `on(BranchId)` takes a validated id where the field \
              holds one.",
    },
    Growable {
        name: "EdgeBelief",
        entry: Entry::Tuned {
            ctor: Ctor::Named("new"),
            at_entry: &[
                "source_id",
                "target_id",
                "edge_type",
                "valid_from",
                "valid_to",
            ],
            renamed: &[("branch_id", "on_branch")],
        },
        why: "the fold's output, but callers assemble one into a \
              `MaterializedState` bound for `save_snapshot`.",
    },
    Growable {
        name: "NodeAttributes",
        entry: Entry::Tuned {
            ctor: Ctor::Named("new"),
            at_entry: &["id", "title", "content"],
            renamed: &[],
        },
        why: "hydrated by the crate and fabricated by callers' fixtures, which \
              is how W15.3 discovered it needed a constructor at all.",
    },
    // --- built, with no setter contract to check -------------------------
    Growable {
        name: "Annotation",
        entry: Entry::Built {
            ctor: Ctor::Named("new"),
        },
        why: "three fields, all three given at construction; nothing is left to \
              set afterwards.",
    },
    Growable {
        name: "Interval",
        entry: Entry::Built {
            ctor: Ctor::Named("new"),
        },
        why: "two fields, both given at construction.",
    },
    Growable {
        name: "AsOf",
        entry: Entry::Built {
            ctor: Ctor::Named("bitemporal"),
        },
        why: "four constructors and two fields; `bitemporal` is the one that \
              supplies both, and D-183 argues that `now`, `valid_at` and \
              `recorded_at` are the only other reachable cases.",
    },
    Growable {
        name: "CostEstimator",
        entry: Entry::Built {
            ctor: Ctor::Named("new"),
        },
        why: "three sizes given at construction; the estimate is what comes out \
              the other side.",
    },
    Growable {
        name: "Overlap",
        entry: Entry::Built {
            ctor: Ctor::Named("new"),
        },
        why: "an error payload no caller has to build — except that the Python \
              binding builds a sample of every `DbError` to check its mapping, \
              and a type nothing outside the crate can construct is a type \
              nothing outside the crate can test against.",
    },
    Growable {
        name: "MaterializedState",
        entry: Entry::Built {
            ctor: Ctor::Named("empty"),
        },
        why: "`save_snapshot` takes one, so a caller who assembles a state needs \
              a way in. `empty(ts)` is it; the other four fields are assigned, \
              which `pub` still allows on a value you own.",
    },
    // --- returned by the crate, never built by a caller -------------------
    Growable {
        name: "Branch",
        entry: Entry::Returned,
        why: "a row of the lineage register, returned by `branches` and `fork`.",
    },
    Growable {
        name: "Divergence",
        entry: Entry::Returned,
        why: "one edge two lineages disagree about, as `diff` reports it.",
    },
    Growable {
        name: "BulkProgress",
        entry: Entry::Returned,
        why: "handed to the caller's own progress callback.",
    },
    Growable {
        name: "CheckpointReport",
        entry: Entry::Returned,
        why: "what SQLite reported, in the three numbers D-156 got out of it.",
    },
    Growable {
        name: "BulkInterrupted",
        entry: Entry::Returned,
        why: "an error the crate raises. The one external *pattern* on it needed \
              a `..`, which is the whole cost of the attribute on a returned \
              struct.",
    },
    Growable {
        name: "CostEstimate",
        entry: Entry::Returned,
        why: "what the estimator answered, including which strategy it picked.",
    },
    Growable {
        name: "RebuildReport",
        entry: Entry::Returned,
        why: "what the shadow rebuild did, and what drift was left after it.",
    },
    Growable {
        name: "KindSnapshot",
        entry: Entry::Returned,
        why: "a metrics reading; `#[non_exhaustive]` since W4.2, which is where \
              D-207 stopped.",
    },
    Growable {
        name: "MetricsSnapshot",
        entry: Entry::Returned,
        why: "as `KindSnapshot`, and the struct on this list most likely to grow \
              a counter.",
    },
    Growable {
        name: "ArchiveReport",
        entry: Entry::Returned,
        why: "what the archive session moved, and where the horizon ended up.",
    },
    Growable {
        name: "HybridHit",
        entry: Entry::Returned,
        why: "a search result carrying both of its ranks.",
    },
    Growable {
        name: "VectorSearchResult",
        entry: Entry::Returned,
        why: "a search result and the distance the index scored it at.",
    },
    Growable {
        name: "MigrationOutcome",
        entry: Entry::Returned,
        why: "which rungs of the ladder the open climbed.",
    },
    Growable {
        name: "ChainCheck",
        entry: Entry::Returned,
        why: "ten numbers comparing a folded state against a composed one, and \
              an obvious candidate for an eleventh.",
    },
    Growable {
        name: "RehydrateReport",
        entry: Entry::Returned,
        why: "what `rehydrate` brought back out of the cold file, and how many rowids it had to reassign.",
    },
];

/// The sources a [`Ctor::Derived`] entry is read from, since the baseline
/// cannot see a derived impl.
///
/// One line per type rather than per file, so the test says which type it
/// failed to find a `Default` for rather than which file it was reading.
const SOURCES: &[(&str, &str)] = &[
    ("Tuning", include_str!("../src/connection.rs")),
    (
        "SnapshotCadence",
        include_str!("../src/temporal/snapshot.rs"),
    ),
];

/// Every file that defines a registered struct, for the one property the
/// baseline answers about the record rather than about the code.
///
/// A file rather than a type, because the question — *does this struct carry
/// the attribute* — is asked of the declaration, and a declaration is found by
/// searching the text for it. Adding a file here is cheaper than being wrong
/// about which file a type moved to.
const CRATE_SOURCES: &[&str] = &[
    include_str!("../src/branch.rs"),
    include_str!("../src/connection.rs"),
    include_str!("../src/error.rs"),
    include_str!("../src/graph/builder.rs"),
    include_str!("../src/graph/edge.rs"),
    include_str!("../src/graph/vector_filter.rs"),
    include_str!("../src/integrity/rebuild.rs"),
    // Behind the `metrics` feature in the crate and not behind one here: this
    // is text, so the property stays checked under `--no-default-features`.
    include_str!("../src/metrics.rs"),
    include_str!("../src/plan.rs"),
    include_str!("../src/schema/migrations.rs"),
    include_str!("../src/temporal/archive.rs"),
    include_str!("../src/temporal/as_of.rs"),
    include_str!("../src/temporal/interval.rs"),
    include_str!("../src/temporal/replay.rs"),
    include_str!("../src/temporal/snapshot.rs"),
    include_str!("../src/vector/hybrid.rs"),
    include_str!("../src/vector/search.rs"),
];

/// Whether the declaration of `name` in the crate's own text carries the
/// attribute — the attribute line immediately above `pub struct <name> {`,
/// which is where `rustfmt` puts it and the only place this crate writes it.
fn attributed_in_source(name: &str) -> Option<bool> {
    let head = format!("pub struct {name} ");
    let brace = format!("pub struct {name} {{");
    let tuple = format!("pub struct {name}(");
    for src in CRATE_SOURCES {
        for (i, line) in src.lines().enumerate() {
            let l = line.trim_start();
            if !(l.starts_with(&brace) || l.starts_with(&tuple) || l == head.trim_end()) {
                continue;
            }
            let above: Vec<&str> = src.lines().take(i).collect();
            return Some(
                above
                    .iter()
                    .rev()
                    .take_while(|p| {
                        let p = p.trim_start();
                        p.starts_with("#[") || p.starts_with("///") || p.starts_with("//")
                    })
                    .any(|p| p.trim_start().starts_with("#[non_exhaustive]")),
            );
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Reading the baseline
// ---------------------------------------------------------------------------

/// The path of an item line, without the `pub …` lead-in and without whatever
/// a signature or a field type puts after the name.
///
/// Stops at the first `(`, `<`, space, or *single* `:` — the `::` of the path
/// is stepped over, which is the whole reason this is not a `split`.
fn item_path(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("#[non_exhaustive] pub struct ")
        .or_else(|| line.strip_prefix("pub struct "))
        .or_else(|| line.strip_prefix("pub fn "))
        .or_else(|| line.strip_prefix("pub async fn "))
        .or_else(|| line.strip_prefix("pub "))?;
    if !rest.starts_with("macrame::") {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'<' | b' ' => break,
            b':' if bytes.get(i + 1) == Some(&b':') => i += 2,
            b':' => break,
            _ => i += 1,
        }
    }
    Some(&rest[..i])
}

/// A path split into everything-before and its last segment.
fn split_last(path: &str) -> (&str, &str) {
    match path.rfind("::") {
        Some(at) => (&path[..at], &path[at + 2..]),
        None => ("", path),
    }
}

/// Every `pub struct` line, as (type name, does it carry the attribute).
///
/// A type re-exported by the prelude appears more than once, which is what the
/// callers of this deduplicate for.
fn struct_lines() -> impl Iterator<Item = (&'static str, bool)> {
    BASELINE.lines().filter_map(|line| {
        let ne = line.starts_with("#[non_exhaustive] pub struct ");
        if !ne && !line.starts_with("pub struct ") {
            return None;
        }
        Some((split_last(item_path(line)?).1, ne))
    })
}

/// The public field names of `name`, under whichever path they are listed.
///
/// A field line is `pub <path>: <type>` and nothing else is — which is worth
/// stating, because the first version of this excluded `pub fn` and forgot
/// `pub async fn`, and every method of `Database` came back as a field.
fn fields_of(name: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = BASELINE
        .lines()
        .filter(|l| l.starts_with("pub macrame::") && l.contains(": "))
        .filter_map(item_path)
        .filter_map(|path| {
            let (owner, field) = split_last(path);
            (split_last(owner).1 == name).then_some(field)
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether `name` has a public method called `method`.
fn has_method(name: &str, method: &str) -> bool {
    BASELINE
        .lines()
        .filter(|l| l.starts_with("pub fn ") || l.starts_with("pub async fn "))
        .filter_map(item_path)
        .any(|path| {
            let (owner, m) = split_last(path);
            m == method && split_last(owner).1 == name
        })
}

/// Whether the declaration of `name` in `src` derives `Default`.
fn derives_default(src: &str, name: &str) -> bool {
    let Some(at) = src.find(&format!("\npub struct {name} {{")) else {
        return false;
    };
    // The attributes sit directly above the declaration; a few hundred bytes
    // is more than the longest of them and short enough that an unrelated
    // derive further up cannot be mistaken for this one.
    let head = &src[..at];
    let window = &head[head.len().saturating_sub(300)..];
    window.rfind("#[derive(").is_some_and(|d| {
        let rest = &window[d..];
        let end = rest.find(")]").unwrap_or(rest.len());
        rest[..end].contains("Default")
    })
}

/// Whether a caller can construct one, by whichever route the entry names.
fn constructible(name: &str, ctor: Ctor) -> bool {
    match ctor {
        Ctor::Named(method) => has_method(name, method),
        Ctor::Derived => SOURCES
            .iter()
            .find(|(ty, _)| *ty == name)
            .is_some_and(|(_, src)| derives_default(src, name) || has_method(name, "default")),
    }
}

fn entry_of(name: &str) -> Option<&'static Growable> {
    REGISTRY.iter().find(|g| g.name == name)
}

// ---------------------------------------------------------------------------
// The gates
// ---------------------------------------------------------------------------

/// **A public struct with public fields is registered here, or it is one field
/// away from a major version and nobody decided that.**
#[test]
fn every_public_struct_with_public_fields_is_registered() {
    let mut unregistered: Vec<&str> = struct_lines()
        .map(|(name, _)| name)
        .filter(|name| !fields_of(name).is_empty())
        .filter(|name| entry_of(name).is_none())
        .collect();
    unregistered.sort_unstable();
    unregistered.dedup();

    assert!(
        unregistered.is_empty(),
        "these public structs have public fields and no entry in REGISTRY: \
         {unregistered:?}. Adding one is a decision rather than a formality: \
         say whether a caller ever builds one, give it `#[non_exhaustive]`, and \
         if a caller does build one, make sure there is a way in that is not \
         the struct literal (W15.3, D-255)."
    );
}

/// The reverse, so the registry cannot outlive what it describes.
#[test]
fn every_registered_struct_is_still_in_the_public_surface() {
    let missing: Vec<&str> = REGISTRY
        .iter()
        .map(|g| g.name)
        .filter(|name| !struct_lines().any(|(n, _)| n == *name))
        .collect();
    assert!(
        missing.is_empty(),
        "REGISTRY names structs the public surface no longer has: {missing:?}. \
         A removed type takes its entry with it, or this file starts \
         describing a crate that does not exist."
    );
}

/// **The C-11 gate.** Every one of them carries the attribute.
#[test]
fn every_registered_struct_is_non_exhaustive() {
    let mut bare: Vec<&str> = struct_lines()
        .filter(|(name, ne)| !ne && entry_of(name).is_some())
        .map(|(name, _)| name)
        .collect();
    bare.sort_unstable();
    bare.dedup();

    assert!(
        bare.is_empty(),
        "these are registered as growable and are not `#[non_exhaustive]`: \
         {bare:?}. Without it, the next field one of them gains is a major \
         version, because a caller's literal enumerates the fields it had — \
         which is review item C-11, and what 0.15.13 closed."
    );
}

/// **The mirror-image gate**, and the one that would have caught all three of
/// the types W15.3 made unconstructible on the way to getting this right.
#[test]
fn every_struct_a_caller_builds_has_a_way_in_that_is_not_a_literal() {
    for g in REGISTRY {
        let ctor = match g.entry {
            Entry::Returned => continue,
            Entry::Built { ctor } | Entry::Tuned { ctor, .. } => ctor,
        };
        assert!(
            constructible(g.name, ctor),
            "a caller builds a {} — {ctor:?} is supposed to be how — and there \
             is no such constructor. `#[non_exhaustive]` forbids the struct \
             literal outside this crate *silently*: the crate itself keeps \
             compiling, because its own literals are still legal, so a type in \
             this state is one nobody outside can construct at all. Registered \
             because: {}",
            g.name,
            g.why
        );
    }
}

/// **The setter contract**, for the structs whose whole shape is that you name
/// the field you mean.
///
/// This is the assertion the deleted canary could only exhibit: a field added
/// to one of these without a setter is a field an external caller can reach
/// only by assignment, on a type whose every other field is set by name.
#[test]
fn every_field_of_a_tuned_struct_has_a_setter_named_after_it() {
    for g in REGISTRY {
        let Entry::Tuned {
            at_entry, renamed, ..
        } = g.entry
        else {
            continue;
        };
        for field in fields_of(g.name) {
            if at_entry.contains(&field) {
                continue;
            }
            let setter = renamed
                .iter()
                .find(|(f, _)| *f == field)
                .map_or(field, |(_, m)| *m);
            assert!(
                has_method(g.name, setter),
                "{}::{field} has no setter — `{setter}` is not on the public \
                 surface. Either add one, name the field in `at_entry` because \
                 construction supplies it, or name the method that does set it \
                 in `renamed`.",
                g.name
            );
        }
    }
}

/// Every entry says why, because the category is a judgement and the next
/// reader is owed the reasoning rather than the verdict.
#[test]
fn every_entry_says_why_it_is_in_the_category_it_is_in() {
    for g in REGISTRY {
        assert!(
            g.why.len() > 30,
            "{}'s `why` is too short to be a reason",
            g.name
        );
    }
}

/// **The same property, asked of the code rather than of the record.**
///
/// Found by mutation: deleting `#[non_exhaustive]` from `SnapshotCadence`
/// leaves every other test in this file green, because they read
/// `public-api.txt` and the baseline still says the attribute is there. The
/// baseline is blessed in the same commit as the change it describes
/// ([D-205]), which makes it honest about a *release* and says nothing about a
/// working tree — and a defect lives in a working tree before it lives in a
/// release. `scripts/check_public_api.py` would catch it, and is not a test.
///
/// [D-205]: ../docs/architecture/s13-decision-register.md#d-205
#[test]
fn every_registered_struct_carries_the_attribute_in_its_own_source() {
    for g in REGISTRY {
        let found = attributed_in_source(g.name).unwrap_or_else(|| {
            panic!(
                "no `pub struct {}` in any file of `CRATE_SOURCES`. Either the \
                 type moved and the list has not, or it is a tuple struct \
                 declared some other way — say which in the list.",
                g.name
            )
        });
        assert!(
            found,
            "`{}` is registered as growable and its declaration in `src/` does \
             not carry `#[non_exhaustive]`. The baseline may still say it \
             does: `public-api.txt` is blessed at release, so between the edit \
             and the blessing the record and the code disagree, and every \
             other assertion in this file believes the record.",
            g.name
        );
    }
}

/// **The setter contract, by value.**
///
/// `every_field_of_a_tuned_struct_has_a_setter_named_after_it` asks the
/// baseline whether a method of the right name exists. Nothing asked whether
/// it assigns the field it is named after — and two mutations survived the
/// whole suite on that: `Tuning::writer_cache_size` writing into
/// `reader_cache_size`, and `SnapshotCadence::poll_interval` assigning
/// nothing.
///
/// The first is a failure mode **this release created**. A struct literal
/// names each field exactly once, so a caller could not write one field twice
/// and leave another at its default; a chain of setters can, and a setter that
/// writes its neighbour's field is invisible to any test that sets both and
/// reads back the one written last — which is what `cache_size_tests` does,
/// deliberately, because the writer's connection cannot be named from outside
/// the actor.
///
/// So: every field of every `Tuned` struct, set through its own setter to a
/// value nothing else in the chain uses, and read back. Written out rather
/// than generated, because a macro over eight types with eight field types
/// would be harder to read than the thing it replaced.
#[test]
fn every_setter_assigns_the_field_it_is_named_after() {
    use macrame::prelude::*;
    use macrame::temporal::NodeAttributes;
    use macrame::util::FakeClock;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    // --- Tuning: the struct C-11 is about ---------------------------------
    let clock = Arc::new(FakeClock::new(SystemTime::UNIX_EPOCH));
    let t = Tuning::default()
        .cadence(CadencePolicy::Disabled)
        .clock(clock)
        .wal_autocheckpoint(WalCheckpointPolicy::EveryPages(4_242))
        .writer_cache_size(-64_000)
        .reader_cache_size(-8_000)
        .future_stamps(FutureStampPolicy::Allow);
    assert_eq!(t.cadence, CadencePolicy::Disabled, "Tuning::cadence");
    assert!(t.clock.is_some(), "Tuning::clock");
    assert_eq!(
        t.wal_autocheckpoint,
        WalCheckpointPolicy::EveryPages(4_242),
        "Tuning::wal_autocheckpoint"
    );
    // The two cache sizes are checked against *each other*, not only against
    // their own values: distinct numbers, so a setter that writes its
    // neighbour's field fails here rather than being overwritten by the next
    // call in the chain.
    assert_eq!(
        t.writer_cache_size,
        Some(-64_000),
        "Tuning::writer_cache_size"
    );
    assert_eq!(
        t.reader_cache_size,
        Some(-8_000),
        "Tuning::reader_cache_size"
    );
    assert_eq!(
        t.future_stamps,
        FutureStampPolicy::Allow,
        "Tuning::future_stamps"
    );

    // --- SnapshotCadence ---------------------------------------------------
    let c = SnapshotCadence::default()
        .every_entries(4_321)
        .poll_interval(Duration::from_millis(777));
    assert_eq!(c.every_entries, 4_321, "SnapshotCadence::every_entries");
    assert_eq!(
        c.poll_interval,
        Duration::from_millis(777),
        "SnapshotCadence::poll_interval"
    );

    // --- TraversalBuilder --------------------------------------------------
    let b = TraversalBuilder::new("start")
        .max_depth(7)
        .edge_types(vec!["relates".into()])
        .min_weight(0.25)
        .attribute_mode(AttributeMode::Omit)
        .as_of_valid("2026-01-02T00:00:00.000000Z")
        .as_of_recorded("2026-01-03T00:00:00.000000Z")
        .on_branch("alt")
        .content(true)
        .limit(11);
    assert_eq!(b.start_node, "start", "TraversalBuilder::start_node");
    assert_eq!(b.max_depth, 7, "TraversalBuilder::max_depth");
    assert_eq!(b.edge_types, vec!["relates".to_string()], "edge_types");
    assert_eq!(b.min_weight, 0.25, "TraversalBuilder::min_weight");
    assert_eq!(
        b.attribute_mode,
        Some(AttributeMode::Omit),
        "TraversalBuilder::attribute_mode"
    );
    assert_eq!(
        b.as_of_valid.as_deref(),
        Some("2026-01-02T00:00:00.000000Z"),
        "TraversalBuilder::as_of_valid"
    );
    assert_eq!(
        b.as_of_recorded.as_deref(),
        Some("2026-01-03T00:00:00.000000Z"),
        "TraversalBuilder::as_of_recorded"
    );
    assert_eq!(b.branch.as_deref(), Some("alt"), "TraversalBuilder::branch");
    assert!(b.content, "TraversalBuilder::content");
    assert_eq!(b.limit, Some(11), "TraversalBuilder::limit");

    // --- ConceptUpsert -----------------------------------------------------
    let alt = BranchId::new("alt").unwrap();
    let u = ConceptUpsert::new("c1", "Title")
        .content("body")
        .valid_from("2026-01-04T00:00:00.000000Z")
        .valid_to("2026-01-05T00:00:00.000000Z")
        .embedding_model("model-a")
        .retired(true)
        .on_branch(alt.clone());
    assert_eq!(u.id, "c1", "ConceptUpsert::id");
    assert_eq!(u.title, "Title", "ConceptUpsert::title");
    assert_eq!(u.content, "body", "ConceptUpsert::content");
    assert_eq!(u.valid_from, "2026-01-04T00:00:00.000000Z", "valid_from");
    assert_eq!(u.valid_to, "2026-01-05T00:00:00.000000Z", "valid_to");
    assert_eq!(
        u.embedding_model.as_deref(),
        Some("model-a"),
        "ConceptUpsert::embedding_model"
    );
    assert!(u.retired, "ConceptUpsert::retired");
    assert_eq!(
        u.branch.as_ref().map(BranchId::as_str),
        Some("alt"),
        "ConceptUpsert::branch"
    );

    // --- EdgeAssertion -----------------------------------------------------
    let e = EdgeAssertion::new("a", "b", "relates")
        .weight(0.75)
        .properties("{}")
        .valid_from("2026-01-06T00:00:00.000000Z")
        .valid_to("2026-01-07T00:00:00.000000Z")
        .on_branch(alt.clone());
    assert_eq!(e.source, "a", "EdgeAssertion::source");
    assert_eq!(e.target, "b", "EdgeAssertion::target");
    assert_eq!(e.edge_type, "relates", "EdgeAssertion::edge_type");
    assert_eq!(e.weight, 0.75, "EdgeAssertion::weight");
    assert_eq!(e.properties, "{}", "EdgeAssertion::properties");
    assert_eq!(e.valid_from, "2026-01-06T00:00:00.000000Z", "valid_from");
    assert_eq!(e.valid_to, "2026-01-07T00:00:00.000000Z", "valid_to");
    assert_eq!(
        e.branch.as_ref().map(BranchId::as_str),
        Some("alt"),
        "EdgeAssertion::branch"
    );

    // --- ReadPlan ----------------------------------------------------------
    let p = ReadPlan::new()
        .valid_at("2026-01-08T00:00:00.000000Z")
        .recorded_at("2026-01-09T00:00:00.000000Z")
        .on(alt.clone())
        .limit(23);
    assert_eq!(
        p.valid.as_deref(),
        Some("2026-01-08T00:00:00.000000Z"),
        "ReadPlan::valid"
    );
    assert_eq!(
        p.recorded.as_deref(),
        Some("2026-01-09T00:00:00.000000Z"),
        "ReadPlan::recorded"
    );
    assert_eq!(
        p.branch.as_ref().map(BranchId::as_str),
        Some("alt"),
        "ReadPlan::branch"
    );
    assert_eq!(p.limit, Some(23), "ReadPlan::limit");

    // --- EdgeBelief --------------------------------------------------------
    let belief = EdgeBelief::new(
        "s",
        "t",
        "relates",
        "2026-01-10T00:00:00.000000Z",
        "2026-01-11T00:00:00.000000Z",
    )
    .on_branch("alt");
    assert_eq!(belief.source_id, "s", "EdgeBelief::source_id");
    assert_eq!(belief.target_id, "t", "EdgeBelief::target_id");
    assert_eq!(belief.edge_type, "relates", "EdgeBelief::edge_type");
    assert_eq!(
        belief.valid_from, "2026-01-10T00:00:00.000000Z",
        "valid_from"
    );
    assert_eq!(belief.valid_to, "2026-01-11T00:00:00.000000Z", "valid_to");
    assert_eq!(belief.branch_id, "alt", "EdgeBelief::branch_id");

    // --- NodeAttributes ----------------------------------------------------
    let n = NodeAttributes::new("n1", "Node", "text").embedding_model("model-b");
    assert_eq!(n.id, "n1", "NodeAttributes::id");
    assert_eq!(n.title, "Node", "NodeAttributes::title");
    assert_eq!(n.content, "text", "NodeAttributes::content");
    assert_eq!(
        n.embedding_model.as_deref(),
        Some("model-b"),
        "NodeAttributes::embedding_model"
    );
}

/// **`Overlap::new` puts each interval where the message says it is.**
///
/// The one constructor here whose arguments are interchangeable by type:
/// `asserted` and `existing` are both pairs of `String`, so swapping them
/// compiles and renders a refusal that blames the caller's interval for the
/// stored one's overlap. [D-075] boxed the eight fields into the variant and
/// this constructor groups them as the two intervals they are, which stops a
/// *caller* mis-ordering them and does nothing about the constructor itself —
/// a mutation swapping the two survived the whole suite, because the only
/// thing outside this crate that builds one is the binding's `DbError` sample
/// and it asserts the variant rather than the values.
///
/// [D-075]: ../docs/architecture/s13-decision-register.md#d-075
#[test]
fn the_overlap_constructor_does_not_swap_the_two_intervals() {
    use macrame::error::Overlap;

    let o = Overlap::new(
        "a",
        "b",
        "relates",
        ("2026-02-01T00:00:00.000000Z", "2026-02-02T00:00:00.000000Z"),
        ("2026-03-01T00:00:00.000000Z", "2026-03-02T00:00:00.000000Z"),
        false,
    );

    assert_eq!(
        (o.valid_from.as_str(), o.valid_to.as_str()),
        ("2026-02-01T00:00:00.000000Z", "2026-02-02T00:00:00.000000Z"),
        "the asserted interval landed somewhere other than `valid_from`/\
         `valid_to`; the refusal would name the stored interval as the one the \
         caller wrote"
    );
    assert_eq!(
        (o.existing_from.as_str(), o.existing_to.as_str()),
        ("2026-03-01T00:00:00.000000Z", "2026-03-02T00:00:00.000000Z"),
        "the existing interval landed somewhere other than `existing_from`/\
         `existing_to`"
    );
    assert_eq!(o.source_id, "a");
    assert_eq!(o.target_id, "b");
    assert_eq!(o.edge_type, "relates");
    assert!(!o.within_batch);
}
