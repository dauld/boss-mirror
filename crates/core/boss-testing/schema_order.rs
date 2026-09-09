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
