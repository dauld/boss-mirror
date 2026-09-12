//! The §9a pin on the silence roster's second source.
//!
//! `cadence.silence.sweep` watches two kinds of declared cadence:
//!
//! 1. **Timer-driven kinds**, declared as `interval_minutes.<kind>` args
//!    on the sweep's own rule row. That fact also lives in the systemd
//!    timer that executes the chore, so it is pinned by
//!    `infra/lint/timers-leave-a-packet.sh` — the bash check that fails
//!    CI when a rostered timer's kind is missing from the args, or when
//!    the two numbers disagree.
//! 2. **Clock-rule cadences**, DERIVED from the rule registry by
//!    `cadence_roster::clock_cadences`. Nothing is declared twice here,
//!    so there is nothing to pin in the §9a sense — the rule row is the
//!    single definition. What CAN go wrong is that a new rule drops out
//!    of the derivation silently: a computed `kind`, a guard shape the
//!    parser does not know, a schedule with no minute interval. That is
//!    the same "outside the roster by construction" defect one level up,
//!    and it is what this file refuses.
//!
//! The check lives HERE rather than in the bash lint deliberately. The
//! derivation is Rust; a bash re-implementation of "which rules declare
//! a cadence" would be a second definition of exactly the thing §9a
//! warns about, and it would drift the first time the parser learns a
//! new guard shape. So the preconditions are pinned where the
//! derivation lives, over the SHIPPED registry directory — which is the
//! registry's definition: `boss_dispatcher::rules::seed` publishes it
//! into the `dispatcher_rules` table at boot, and
//! `the_registry_equals_the_authored_directory_after_a_seed` proves the
//! two are the same set.
//!
//! Measured 2026-09-10 (backlog cf0f5e2d): eight clock-rule cadences
//! existed, all eight were outside the roster, and three families of
//! them had been silent for nine to nineteen days with nothing saying
//! so. The public mirror drifted 238 commits behind and a human noticed
//! by hand.

use std::collections::BTreeSet;

use boss_dispatcher::rules::registry::parse_raw_path;
use boss_dispatcher_handlers::handlers::cadence_roster::{
    ClockCadence, Guard, NotACadence, clock_cadences,
};
use boss_dispatcher_handlers::handlers::cadence_silence::declarations;

/// The authored registry — the directory, not a file. Adding a rule is
/// dropping a file in.
const RULES_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../infra/dispatcher/rules"
);

/// The sweep whose roster this is.
const SWEEP_RULE: &str = "cadence-silence-sweep-daily";

/// A scheduled rule that spawns no packet, with the reason it is not a
/// declared cadence. Adding a name here is a decision; the default is
/// that a scheduled spawner declares a cadence somebody watches.
///
/// REMOVING a name is equally a decision, and the loop at the bottom of
/// `every_scheduled_rule_declares_a_cadence_or_says_why_not` forces it:
/// an exemption for a rule that is no longer scheduled silently covers
/// the next rule to take that name. `design-review-level-sweep` was
/// dropped on 2026-09-10 for exactly that reason — it ran
/// `docs.design.sweep`, one packet per doc with open questions, and
/// train #298 (`0c86bdfd`) retired the rule along with the whole corpus
/// index it fed (`f5da586c`; no `boss-docs`, no `design-review-spawn`,
/// no `docs/design/*.md` parse). The SHAPE it stood for — a `jobs.spawn`
/// with a computed subject — is still read and still skipped by
/// `clock_cadences`; it just has no shipped rule exhibiting it today, so
/// an entry here would be an exemption with nothing to exempt.
const SPAWNS_NOTHING_ON_PURPOSE: &[(&str, &str)] = &[
    (
        "network-census-daily",
        "runs `network.census`, which records an observation and spawns no packet. \
         `kind=network-census` has total=0 and always will — a rule that produces no \
         packet cannot be watched by a sweep that reads packets, and reading that zero \
         as 'never fired' is the claim cf0f5e2d's first pass withdrew.",
    ),
    (
        "recheck-failing-probes-daily",
        "runs `jobs.run-car-probes` scoped to cars whose probe already failed; it files \
         a `run-car-probe` ops-request per such car, so on a day with no failing probe \
         it produces NOTHING, and that zero is the healthy reading. The packets it does \
         file are the same kind the arrival rule files, so a sweep keyed by kind could \
         not tell the two apart either.",
    ),
    (
        SWEEP_RULE,
        "IS the sweep. Its own firing leaves no packet of a kind — its findings are \
         backlog-items keyed by cadence, and a silence in the sweep itself is the gap \
         backlog 6bf34846 tracks (an alarm that reports through its subject dies with \
         it).",
    ),
];

fn shipped() -> (Vec<ClockCadence>, Vec<NotACadence>) {
    let raw = parse_raw_path(RULES_DIR).expect("parse the shipped rule registry directory");
    clock_cadences(&raw.rules)
}

/// THE pin: every scheduled rule either declares a cadence the sweep can
/// measure, or is named above with the reason it does not. A rule that
/// falls out of the derivation for any other reason fails here, by name.
#[test]
fn every_scheduled_rule_declares_a_cadence_or_says_why_not() {
    let (cadences, skipped) = shipped();
    assert!(
        !cadences.is_empty(),
        "no clock-rule cadences derived from {RULES_DIR} — the scrape broke, so a green \
         result here would mean nothing"
    );
    for s in &skipped {
        let named = SPAWNS_NOTHING_ON_PURPOSE
            .iter()
            .any(|(n, _)| *n == s.rule());
        assert!(
            named,
            "scheduled rule `{}` declares no cadence the silence sweep can measure: {s:?}\n\n  \
             A clock rule that spawns a packet on a schedule IS a declared cadence. Dropping \
             out of the derivation is silent, which is exactly how eight of them went \
             unwatched until 2026-09-10 (cf0f5e2d). Either give the spawn a literal `kind` \
             and `subject` so its identity is fixed, teach \
             `cadence_roster::clock_cadences` the shape, or add the rule to \
             SPAWNS_NOTHING_ON_PURPOSE in this file with the reason no packet is expected.",
            s.rule()
        );
    }
    // A stale exemption is not free: left standing it holds a future rule
    // of that name out of the roster without anyone deciding so.
    let skipped_names: BTreeSet<&str> = skipped.iter().map(NotACadence::rule).collect();
    for (name, _) in SPAWNS_NOTHING_ON_PURPOSE {
        assert!(
            skipped_names.contains(name),
            "SPAWNS_NOTHING_ON_PURPOSE names `{name}`, which now either declares a cadence or \
             is not a scheduled rule any more. Remove the entry — a dead exemption silently \
             covers the next rule to take that name."
        );
    }
}

/// The eight cadences that were dead behind their own guards. Named
/// explicitly: this is the regression the packet was filed for, and a
/// rule deleted or renamed out of the roster should cost a decision, not
/// pass quietly.
#[test]
fn the_measured_clock_cadences_are_on_the_roster() {
    let (cadences, _) = shipped();
    let labels: BTreeSet<String> = cadences.iter().map(ClockCadence::label).collect();
    for want in [
        "maintenance-sweep/image-freshness",
        "maintenance-sweep/disk-headroom",
        "maintenance-sweep/empty-decisions",
        "maintenance-sweep/deploy-convergence",
        "maintenance-sweep/stale-build-caches",
        "maintenance-sweep/cluster-conformance",
        "publish-to-github/github-mirror",
    ] {
        assert!(
            labels.contains(want),
            "`{want}` is not on the derived roster. These are the clock cadences measured silent \
             on 2026-09-10 — the mirror one for NINETEEN days, the sweeps for nine — and the \
             roster deriving from the rules is what notices next time. This list is \
             deliberately HAND-WRITTEN and not derived: deriving it from the same directory \
             the roster derives from would make this test vacuous, and catching a derivation \
             that silently loses an entry is the whole point. So a shrinking list is an EDIT, \
             and the edit must say why — `maintenance-sweep/doc-status` was removed on \
             2026-09-10 because train #295 (01240ef2) retired \
             `maintenance-sweep-doc-status-daily` with the flush pipeline: that sweep whole \
             content was the stale-statuses drifted-status report, and a packet status IS its \
             status. Found: {labels:?}"
        );
    }
}

/// CONSERVATION, which is stronger than any floor — the shape worth
/// reaching for whenever a derivation PARTITIONS its input.
///
/// `clock_cadences` splits the scheduled rules into exactly two buckets:
/// a measurable cadence, or a named `NotACadence` reason. So their sum
/// must equal the number of scheduled rules in the directory, and that
/// equation survives every legitimate change: teaching the parser a new
/// guard shape moves a rule ACROSS the line and keeps the total, where a
/// floor would have to be re-judged. What it refuses is the one thing a
/// floor cannot see — a rule that lands in NEITHER bucket, which is how
/// eight cadences sat outside the roster for nineteen days (cf0f5e2d).
///
/// Conservation is the strongest of the three non-vacuity shapes and the
/// narrowest: it applies only where the output partitions the input.
/// Where it does, prefer it; `assert_roster_floor!` is for the rest.
#[test]
fn every_scheduled_rule_lands_in_exactly_one_bucket() {
    let raw = parse_raw_path(RULES_DIR).expect("parse the shipped rule registry directory");
    let scheduled = raw.rules.iter().filter(|r| r.schedule.is_some()).count();
    let (cadences, skipped) = clock_cadences(&raw.rules);
    assert_eq!(
        cadences.len() + skipped.len(),
        scheduled,
        "{scheduled} rules in {RULES_DIR} carry a schedule, but the derivation accounted for \
         {} of them ({} cadences + {} named non-cadences). A scheduled rule in neither bucket \
         is a cadence nobody watches and nobody can name — and unlike a thinned roster, no \
         floor would notice, because the count it left behind is still plausible.",
        cadences.len() + skipped.len(),
        cadences.len(),
        skipped.len()
    );
    // And the partition is not trivially empty on both sides.
    boss_testing::assert_roster_floor!(
        raw.rules,
        40,
        "the authored dispatcher rule registry at {RULES_DIR} (61 files on 2026-09-11)"
    );
}

/// FAIL-CLOSED on the guard. The sweep explains a silence by naming the
/// open packet its guard asks about; a guard shape the parser cannot read
/// is a suppression nobody can name, so it must be taught rather than
/// discovered during the next nineteen-day outage.
#[test]
fn every_derived_cadence_has_a_readable_guard_or_none_at_all() {
    let (cadences, _) = shipped();
    boss_testing::assert_roster_floor!(
        cadences,
        6,
        "the clock cadences derived from {RULES_DIR} (7 on 2026-09-11)"
    );
    for c in &cadences {
        if let Some(Guard::Unreadable(src)) = &c.guard {
            panic!(
                "clock rule `{}` guards on `{src}`, a shape `cadence_roster::parse_guard` does \
                 not read. The sweep would call its silence UNEXPLAINED and never name the \
                 packet holding it, which is the whole finding of cf0f5e2d. Teach the parser \
                 the shape.",
                c.rule
            );
        }
    }
}

/// §9a, the direction that matters here: a clock-rule cadence must NOT
/// also be declared on the sweep's args. Derived and declared would be
/// the same fact in two places — and the declared one is coarser (seven
/// sweep rules share the kind `maintenance-sweep`), so the pair would
/// alarm twice and disagree about what is silent.
#[test]
fn no_clock_rule_kind_is_also_declared_on_the_sweeps_args() {
    let raw = parse_raw_path(RULES_DIR).expect("parse the shipped rule registry directory");
    let sweep = raw
        .rules
        .iter()
        .find(|r| r.name == SWEEP_RULE)
        .unwrap_or_else(|| panic!("{SWEEP_RULE} is in the shipped registry"));
    let args: Vec<(String, boss_dispatcher::rules::expr::Value)> = sweep
        .do_steps
        .iter()
        .flat_map(|d| d.args.iter())
        .map(|(k, v)| {
            // The stored arg is an expression source; every roster entry
            // is a quoted integer literal.
            let n: i64 = v.trim().trim_matches('"').parse().unwrap_or(-1);
            (k.clone(), boss_dispatcher::rules::expr::Value::Int(n))
        })
        .collect();
    let declared: BTreeSet<String> = declarations(&args).into_iter().map(|(k, _)| k).collect();
    assert!(
        !declared.is_empty(),
        "read no `interval_minutes.*` declarations off {SWEEP_RULE} — the parse broke, so \
         this check would pass vacuously"
    );

    let (cadences, _) = shipped();
    for c in &cadences {
        assert!(
            !declared.contains(&c.kind),
            "kind `{}` is BOTH declared on {SWEEP_RULE}'s args and derived from clock rule \
             `{}`. One fact, two places (CLAUDE.md §9a) — and they are not even the same \
             fact: the derived roster watches `{}` while the arg watches every packet of \
             `{}` together, so the pair would file two alarms and disagree about which is \
             silent. Drop the arg; the rule row already says it.",
            c.kind,
            c.rule,
            c.label(),
            c.kind
        );
    }
}
