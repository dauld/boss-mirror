// The migration ORDER, defined once.
//
// `infra/postgres/schema/*.sql` is the definition of what runs; this
// is the definition of the order it runs in. Three readers need that
// order and none of them can call the others: `migrate.sh` derives it
// at runtime in shell, `build.rs` derives it at build time to emit
// `SCHEMA_FILES` (because `include_str!` needs literal paths), and the
// equality test in `test_db.rs` re-derives it to check the generated
// list has not gone stale. The build script and the test now `include!`
// THIS file, so the two Rust readers cannot disagree.
//
// They did disagree, and it redded a train on 2026-09-09. The key was
// written twice; `build.rs` parsed the prefix as `u64` and the test as
// `u32`. While every prefix was `NNN-` or a twelve-digit
// `YYYYMMDDHHMM-` both fitted, and the two orders were accidentally
// equal. The moment a fourteen-digit `YYYYMMDDHHMMSS-` prefix arrived
// (~2.0e13, adopted to end prefix collisions) it overflowed the u32,
// fell into the no-prefix arm and sorted LAST for the test while
// sorting numerically for the generator. Same files, different order,
// and migrations are APPLIED in that order.
//
// The rule, matching `migrate.sh`'s `sort -t- -k1,1n`: the numeric
// prefix decides; a file with no numeric prefix sorts last rather than
// landing silently in the middle of a run; equal prefixes tiebreak on
// the whole name so the order is total and stable.

/// Sort key for one schema file name, e.g. `03-jobs.sql`.
///
/// `u64`, not `u32`: a `YYYYMMDDHHMMSS-` prefix is ~2.0e13 and a u32
/// would overflow into the no-prefix arm — which is exactly the bug
/// this file exists to make unrepeatable.
#[allow(dead_code)]
fn schema_sort_key(name: &str) -> (u64, String) {
    let num: u64 = name
        .split('-')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(u64::MAX);
    (num, name.to_string())
}

/// FNV-1a's offset basis — the seed for every fingerprint below.
#[allow(dead_code)]
const FNV1A_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a, continued from `seed`.
///
/// Not a security primitive and not trying to be. The properties
/// required are that the same bytes yield the same number across
/// processes and runs, that different bytes usually do not, and that it
/// adds no dependency to a crate every test in the workspace links.
/// One implementation, because a hash that lives twice can drift
/// (CLAUDE.md §9a) — `schema_fingerprint` in `test_db.rs` held the
/// second copy.
#[allow(dead_code)]
fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
    let mut hash = seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Fingerprint of an ORDERED schema list: every name and every byte of
/// SQL, in apply order.
///
/// This is the number that makes a stale build detectable. `build.rs`
/// computes it over the files it reads and emits it as a constant;
/// `test_db.rs` recomputes it over the list actually linked into the
/// binary and over the directory as it is on disk at run time. Three
/// readings of one definition — derived, never authored, so no second
/// list exists to drift.
///
/// Length-prefixed per field, so no two different lists can hash alike
/// by moving a byte across an entry boundary (`("ab","c")` and
/// `("a","bc")` must differ).
#[allow(dead_code)]
fn schema_set_fingerprint<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> u64 {
    let mut hash = FNV1A_OFFSET;
    for (name, sql) in entries {
        hash = fnv1a(hash, &(name.len() as u64).to_le_bytes());
        hash = fnv1a(hash, name.as_bytes());
        hash = fnv1a(hash, &(sql.len() as u64).to_le_bytes());
        hash = fnv1a(hash, sql.as_bytes());
    }
    hash
}
