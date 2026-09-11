//! The silence roster's SECOND source: the dispatcher rule registry.
//!
//! THE GAP THIS CLOSES (backlog cf0f5e2d, measured 2026-09-10).
//! `cadence.silence.sweep` reconciled declared cadence against actual
//! packets for eighteen kinds, and its roster was pinned by
//! `infra/lint/timers-leave-a-packet.sh` to the systemd TIMER files —
//! correctly, for what it covered. But the dispatcher also fires daily
//! CLOCK RULES that spawn a chore packet with no timer anywhere, and a
//! clock-rule cadence was outside the roster *by construction*. Three
//! families of them were silent and nothing said so:
//!
//! - `publish-to-github-daily` had not fired since 2026-08-22 —
//!   NINETEEN days — so the public mirror drifted 238 commits / 763
//!   files behind with nothing announcing it. The daily check exists
//!   precisely so "is the mirror current?" is a query instead of
//!   something somebody remembers to wonder about; a human noticed by
//!   hand instead.
//! - The seven `maintenance-sweep-*-daily` rules had not fired since
//!   2026-09-01 — nine days.
//! - `design-review-level-sweep` kept working throughout, which is the
//!   proof it is not the clock: that rule carried NO `when` guard. (It
//!   was itself retired hours later, with the corpus index it fed —
//!   train #298 — so look for it in the log, not the registry.)
//!
//! THE CAUSE WAS EACH RULE'S OWN DEDUP GUARD. Every one of the dead
//! rules fires only `NOT open_job_exists(<kind>, <target>)` (or
//! `NOT open_publish_exists(<subject>)`). The intent is right — do not
//! stack duplicate chores, the fix for 0517387b and 9f0c566a — but the
//! effect is that ONE packet nobody finished silently converts a cadence
//! into never-again. A cadence whose firing was SUPPRESSED is not the
//! same as one that fired and found nothing, and until this module
//! nothing in the system could tell them apart.
//!
//! WHAT THIS MODULE IS. A pure reading of rule rows: given the rules the
//! dispatcher is enforcing, which of them DECLARE a cadence (a packet of
//! identity `(kind, subject)` is expected every N minutes), and what
//! question does each one's guard ask. [`cadence_silence`] turns that
//! into the same measurement it already makes for timer-driven kinds,
//! plus one extra question when a cadence is silent: is an open packet
//! holding it, and for how long?
//!
//! WHY IT IS DERIVED AND NOT DECLARED (CLAUDE.md §9a). The obvious
//! alternative was to add eight more `interval_minutes.<kind>` args to
//! the sweep's own rule row. That would state a fact the rule registry
//! already holds — the cadence, the kind, the subject — in a second
//! place, and §9a's worked examples all say the same thing: collapse it
//! if you can, pin it only if you cannot. Here it collapses, because the
//! rule row IS the declaration. Two things fall out of that which a
//! second declaration could not give:
//!
//! - **The identity is the RULE, not the kind.** Seven sweep rules share
//!   the kind `maintenance-sweep` and differ only in subject, so a
//!   kind-level roster could not see six of them stop — it would go
//!   quiet only when ALL seven did. The derived roster watches
//!   `maintenance-sweep/image-freshness` separately from
//!   `maintenance-sweep/disk-headroom`.
//! - **The guard comes with it.** A declaration cannot tell you what
//!   suppresses a cadence; the rule row can, because the guard is the
//!   rule.
//!
//! WHAT IS DELIBERATELY NOT A DECLARED CADENCE. Only a `jobs.spawn`
//! do-step with a LITERAL kind and subject declares "a packet of this
//! identity must arrive". Two shapes are excluded on purpose, and both
//! were misread once already:
//!
//! - A rule that spawns NO packet. `network-census-daily` runs
//!   `network.census`; `kind=network-census` has `total=0` and always
//!   will, and the first pass of cf0f5e2d read that zero as "never
//!   fired" and withdrew the claim. A rule that produces no packet
//!   cannot be watched by a sweep that reads packets — it needs a
//!   different probe. This is the one shipped exclusion today.
//! - A rule whose spawn has a COMPUTED subject, so the packet's identity
//!   is not fixed and "one must arrive" is not a statement the sweep can
//!   check. `design-review-level-sweep` was the worked example — one
//!   packet per doc with open questions, where zero such docs is honest
//!   silence and a cadence assertion would alarm on a clean house — and
//!   train #298 retired it with the rest of the corpus index
//!   (`f5da586c`). The shape is still read and still skipped; no shipped
//!   rule exhibits it at the moment.
//!
//! A scheduled `jobs.spawn` rule this module cannot read — a computed
//! `kind`, a guard shape it does not know — would drop out of the roster
//! SILENTLY, which is the defect one level up. That is why
//! `tests/the_silence_roster_covers_the_clock_rules.rs` parses the
//! shipped registry directory and fails, naming the rule, when one does:
//! the derivation's preconditions are pinned where the derivation lives,
//! rather than restated in bash where the two could drift.

use boss_core::calendar::Cadence;
use boss_dispatcher::rules::helpers_inventory::PUBLISH_JOB_KIND;
use boss_dispatcher::rules::registry::RawRule;

/// The one handler that opens a packet from a schedule. A clock rule
/// whose `do` names anything else is not declaring that a packet of some
/// identity arrives (see the module doc's two worked exclusions).
pub const JOBS_SPAWN: &str = "jobs.spawn";

/// The question a clock rule's `when` guard asks, normalized.
///
/// Both shipped guard helpers reduce to the same question — is there an
/// open packet of `(kind, subject)`? — so the sweep asks it once and
/// does not care which helper phrased it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guard {
    /// `NOT open_job_exists(kind, subject)` / `NOT
    /// open_publish_exists(subject)`. `source` is the guard verbatim, so
    /// an alarm can quote the text an operator will go and read.
    OpenPacket {
        kind: String,
        subject: String,
        source: String,
    },
    /// A `when` this parser does not know. Reported, never assumed
    /// harmless: a guard nobody can read is a suppression nobody can
    /// name, so the sweep keeps calling such a silence UNEXPLAINED
    /// rather than quietly attributing it to a block it cannot see.
    Unreadable(String),
}

/// One cadence a clock rule declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockCadence {
    /// The rule that declares it — the thing an operator edits.
    pub rule: String,
    /// The packet kind the rule spawns.
    pub kind: String,
    /// The packet subject the rule spawns. Part of the identity: seven
    /// sweep rules share one kind and differ only here.
    pub subject: String,
    /// Minutes between expected packets, from the schedule's cadence.
    pub interval_min: i64,
    /// The guard that can suppress the firing, if the rule has one.
    pub guard: Option<Guard>,
}

impl ClockCadence {
    /// The identity in one string — the dedup key and the name in every
    /// message. `maintenance-sweep/image-freshness`.
    pub fn label(&self) -> String {
        format!("{}/{}", self.kind, self.subject)
    }
}

/// Why a scheduled rule yielded no cadence. Returned alongside the
/// cadences so a caller (the pin test) can name the rule instead of
/// discovering a silent omission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotACadence {
    /// No `jobs.spawn` do-step: the rule's effect is not a packet.
    SpawnsNothing { rule: String, handlers: Vec<String> },
    /// A `jobs.spawn` whose `kind` or `subject` arg is not a string
    /// literal — computed per firing, so no fixed identity is expected.
    ComputedIdentity {
        rule: String,
        kind: String,
        subject: String,
    },
    /// A schedule shape with no minute interval.
    UnreadableSchedule { rule: String, reason: String },
}

impl NotACadence {
    pub fn rule(&self) -> &str {
        match self {
            NotACadence::SpawnsNothing { rule, .. }
            | NotACadence::ComputedIdentity { rule, .. }
            | NotACadence::UnreadableSchedule { rule, .. } => rule,
        }
    }
}

/// Minutes between firings of one cadence shape.
///
/// The sparse shapes are nominal lengths (30 / 90 / 365 days), not
/// calendar-exact: the number is only ever multiplied by a silence
/// threshold of two or three, so a day of slack in a quarter changes no
/// verdict, and being approximate here is cheaper than being wrong
/// somewhere that matters.
pub fn cadence_interval_minutes(c: Cadence) -> Option<i64> {
    Some(match c {
        Cadence::EveryNMinutes(n) if n > 0 => i64::from(n),
        Cadence::EveryNMinutes(_) => return None,
        Cadence::Hourly => 60,
        Cadence::Daily => 1440,
        Cadence::Weekly => 10_080,
        Cadence::Biweekly => 20_160,
        Cadence::Monthly => 43_200,
        Cadence::Quarterly => 129_600,
        Cadence::Annually => 525_600,
    })
}

/// A rule arg / guard argument that is a plain double-quoted string
/// literal, unwrapped. Anything else (an identifier, a payload
/// reference, a comparison) is `None` — it is computed per firing and
/// has no fixed value this module may pretend to know.
pub fn string_literal(src: &str) -> Option<String> {
    let t = src.trim();
    let inner = t.strip_prefix('"')?.strip_suffix('"')?;
    // A second quote inside would mean a concatenation or an escape this
    // parser does not handle; refusing is the fail-closed answer.
    if inner.contains('"') {
        return None;
    }
    Some(inner.to_string())
}

/// Read one rule's `when` as the question it asks about open packets.
///
/// Knows exactly the two shipped dedup shapes, and says so when it does
/// not know a third: `Guard::Unreadable` is a finding's worth of
/// information, not a shrug.
pub fn parse_guard(when: &str) -> Guard {
    let source = when.trim().to_string();
    let Some(call) = source.strip_prefix("NOT ") else {
        return Guard::Unreadable(source);
    };
    if let Some([kind, subject]) = call_args(call.trim(), "open_job_exists").as_deref() {
        return Guard::OpenPacket {
            kind: kind.clone(),
            subject: subject.clone(),
            source,
        };
    }
    // The publish helper's own question: one mirror, one open publish
    // packet — keyed on the packet's SUBJECT, over a fixed kind.
    if let Some([subject]) = call_args(call.trim(), "open_publish_exists").as_deref() {
        return Guard::OpenPacket {
            kind: PUBLISH_JOB_KIND.to_string(),
            subject: subject.clone(),
            source,
        };
    }
    Guard::Unreadable(source)
}

/// `name("a", "b")` → `["a", "b"]`, and `None` unless every argument is
/// a plain string literal.
fn call_args(src: &str, name: &str) -> Option<Vec<String>> {
    let inner = src
        .strip_prefix(name)?
        .trim_start()
        .strip_prefix('(')?
        .strip_suffix(')')?;
    inner.split(',').map(string_literal).collect()
}

/// THE derivation: the cadences the enforced rules declare, plus every
/// scheduled rule that declares none and why.
///
/// Pure in the rule rows — the same shape `GET /api/dispatcher/rules`
/// serves, so the parser is unchanged if the reader ever becomes that
/// surface instead of the registry the runner loaded.
pub fn clock_cadences(rules: &[RawRule]) -> (Vec<ClockCadence>, Vec<NotACadence>) {
    let mut cadences = Vec::new();
    let mut skipped = Vec::new();
    for rule in rules {
        let Some(schedule) = &rule.schedule else {
            continue;
        };
        let Some(spawn) = rule.do_steps.iter().find(|d| d.handler == JOBS_SPAWN) else {
            skipped.push(NotACadence::SpawnsNothing {
                rule: rule.name.clone(),
                handlers: rule.do_steps.iter().map(|d| d.handler.clone()).collect(),
            });
            continue;
        };
        let kind_arg = spawn.args.get("kind").cloned().unwrap_or_default();
        let subject_arg = spawn.args.get("subject").cloned().unwrap_or_default();
        let (Some(kind), Some(subject)) = (string_literal(&kind_arg), string_literal(&subject_arg))
        else {
            skipped.push(NotACadence::ComputedIdentity {
                rule: rule.name.clone(),
                kind: kind_arg,
                subject: subject_arg,
            });
            continue;
        };
        let Some(interval_min) = cadence_interval_minutes(schedule.cadence) else {
            skipped.push(NotACadence::UnreadableSchedule {
                rule: rule.name.clone(),
                reason: format!("{:?} carries no minute interval", schedule.cadence),
            });
            continue;
        };
        cadences.push(ClockCadence {
            rule: rule.name.clone(),
            kind,
            subject,
            interval_min,
            guard: rule.when.as_deref().map(parse_guard),
        });
    }
    cadences.sort_by(|a, b| (a.kind.as_str(), a.subject.as_str()).cmp(&(&b.kind, &b.subject)));
    (cadences, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_dispatcher::rules::registry::{RawDoStep, RawSchedule};
    use std::collections::HashMap;

    fn spawn_step(args: &[(&str, &str)]) -> RawDoStep {
        RawDoStep {
            handler: JOBS_SPAWN.to_string(),
            args: args
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<HashMap<String, String>>(),
        }
    }

    fn daily(name: &str, when: Option<&str>, steps: Vec<RawDoStep>) -> RawRule {
        RawRule {
            name: name.to_string(),
            on_event: None,
            schedule: Some(RawSchedule {
                cadence: Cadence::Daily,
                anchor_date: "2026-08-14".parse().expect("a date"),
                business_calendar: None,
            }),
            when: when.map(str::to_string),
            do_steps: steps,
            delay: None,
            version: 1,
        }
    }

    /// The image-freshness sweep, verbatim from
    /// `infra/dispatcher/rules/maintenance-sweep-image-freshness-daily.toml`
    /// — the rule that went nine days dead behind its own guard.
    fn image_freshness() -> RawRule {
        daily(
            "maintenance-sweep-image-freshness-daily",
            Some(r#"NOT open_job_exists("maintenance-sweep", "image-freshness")"#),
            vec![spawn_step(&[
                ("kind", r#""maintenance-sweep""#),
                ("subject_kind", r#""custom""#),
                ("subject", r#""image-freshness""#),
                ("title", r#""CI image freshness sweep""#),
            ])],
        )
    }

    #[test]
    fn a_daily_spawner_declares_a_cadence_with_its_guard() {
        let (cadences, skipped) = clock_cadences(&[image_freshness()]);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(
            cadences,
            vec![ClockCadence {
                rule: "maintenance-sweep-image-freshness-daily".into(),
                kind: "maintenance-sweep".into(),
                subject: "image-freshness".into(),
                interval_min: 1440,
                guard: Some(Guard::OpenPacket {
                    kind: "maintenance-sweep".into(),
                    subject: "image-freshness".into(),
                    source: r#"NOT open_job_exists("maintenance-sweep", "image-freshness")"#.into(),
                }),
            }]
        );
        assert_eq!(cadences[0].label(), "maintenance-sweep/image-freshness");
    }

    /// Seven rules share the kind `maintenance-sweep`. A kind-level
    /// roster would go quiet only when ALL seven stopped; the identity
    /// is the (kind, subject) pair, which is why the roster is derived
    /// from the rules rather than declared per kind.
    #[test]
    fn sibling_sweeps_sharing_one_kind_are_separate_cadences() {
        let disk = daily(
            "maintenance-sweep-disk-daily",
            Some(r#"NOT open_job_exists("maintenance-sweep", "disk-headroom")"#),
            vec![spawn_step(&[
                ("kind", r#""maintenance-sweep""#),
                ("subject", r#""disk-headroom""#),
            ])],
        );
        let (cadences, _) = clock_cadences(&[image_freshness(), disk]);
        let labels: Vec<String> = cadences.iter().map(ClockCadence::label).collect();
        assert_eq!(
            labels,
            vec![
                "maintenance-sweep/disk-headroom",
                "maintenance-sweep/image-freshness"
            ],
            "one kind, two cadences, kind-then-subject sorted for a deterministic pass"
        );
    }

    /// The mirror's guard names no kind — the helper supplies it. The
    /// sweep must still end up asking about an open `publish-to-github`
    /// packet for `github-mirror`, because that is the packet that held
    /// the cadence for nineteen days.
    #[test]
    fn the_publish_guard_normalizes_to_the_publish_kind() {
        let rule = daily(
            "publish-to-github-daily",
            Some(r#"NOT open_publish_exists("github-mirror")"#),
            vec![spawn_step(&[
                ("kind", r#""publish-to-github""#),
                ("subject", r#""github-mirror""#),
            ])],
        );
        let (cadences, _) = clock_cadences(&[rule]);
        assert_eq!(
            cadences[0].guard,
            Some(Guard::OpenPacket {
                kind: PUBLISH_JOB_KIND.into(),
                subject: "github-mirror".into(),
                source: r#"NOT open_publish_exists("github-mirror")"#.into(),
            })
        );
    }

    /// An unguarded cadence is still a cadence — it just cannot be
    /// suppressed, so a silence there is never explained away. This was
    /// `design-review-level-sweep`'s shape, and the reason it was the one
    /// clock rule still firing when the guarded eight went quiet.
    #[test]
    fn a_rule_with_no_guard_declares_a_cadence_that_cannot_be_suppressed() {
        let rule = daily(
            "an-unguarded-daily-spawner",
            None,
            vec![spawn_step(&[
                ("kind", r#""maintenance-sweep""#),
                ("subject", r#""unguarded""#),
            ])],
        );
        let (cadences, _) = clock_cadences(&[rule]);
        assert_eq!(cadences[0].guard, None);
    }

    /// FAIL-LOUD on a guard shape this parser does not know: the sweep
    /// must not attribute a silence to a block it cannot see.
    #[test]
    fn an_unknown_guard_shape_is_unreadable_not_absent() {
        assert_eq!(
            parse_guard("open_questions > 0"),
            Guard::Unreadable("open_questions > 0".into())
        );
        assert_eq!(
            parse_guard(r#"NOT open_review_exists(path)"#),
            Guard::Unreadable("NOT open_review_exists(path)".into())
        );
        assert_eq!(
            parse_guard(r#"NOT open_job_exists("maintenance-sweep", target)"#),
            Guard::Unreadable(r#"NOT open_job_exists("maintenance-sweep", target)"#.into()),
            "a computed target is not a fixed question, so it is not read as one"
        );
    }

    /// A handler that spawns nothing declares no cadence —
    /// `network-census-daily`'s shape, and the withdrawn half of
    /// cf0f5e2d's first pass: `kind=network-census` has total=0 and
    /// always will.
    #[test]
    fn a_schedule_that_spawns_nothing_is_not_a_cadence() {
        let rule = daily(
            "network-census-daily",
            None,
            vec![RawDoStep {
                handler: "network.census".into(),
                args: HashMap::new(),
            }],
        );
        let (cadences, skipped) = clock_cadences(&[rule]);
        assert!(cadences.is_empty());
        assert_eq!(
            skipped,
            vec![NotACadence::SpawnsNothing {
                rule: "network-census-daily".into(),
                handlers: vec!["network.census".into()],
            }]
        );
    }

    /// A spawner whose SUBJECT is computed declares no cadence: the
    /// packet's identity is not fixed, so "one of these must arrive" is
    /// not a statement the sweep can check, and zero of them may be
    /// honest silence rather than a suppression.
    ///
    /// The fixture is deliberately a made-up rule name. It used to be
    /// `design-review-level-sweep` (`docs.design.sweep`, one packet per
    /// doc with open questions), which train #298 retired with the corpus
    /// index — and a unit test naming a rule the registry no longer ships
    /// reads as a claim about today's registry when it is only a claim
    /// about a shape.
    #[test]
    fn a_computed_identity_is_not_a_cadence_and_says_which_rule() {
        let rule = daily(
            "a-daily-spawner-with-a-computed-subject",
            None,
            vec![RawDoStep {
                handler: JOBS_SPAWN.into(),
                args: [
                    ("kind".to_string(), r#""some-review""#.to_string()),
                    ("subject".to_string(), "path".to_string()),
                ]
                .into_iter()
                .collect(),
            }],
        );
        let (cadences, skipped) = clock_cadences(&[rule]);
        assert!(cadences.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].rule(), "a-daily-spawner-with-a-computed-subject");
        assert!(matches!(skipped[0], NotACadence::ComputedIdentity { .. },));
    }

    #[test]
    fn an_event_rule_declares_no_cadence() {
        let rule = RawRule {
            name: "spawn-restock-on-low-inventory".into(),
            on_event: Some("jobs.inventory.low".into()),
            schedule: None,
            when: Some("on_hand <= reorder_point".into()),
            do_steps: vec![spawn_step(&[
                ("kind", r#""ingredient-restock""#),
                ("subject", r#""x""#),
            ])],
            delay: None,
            version: 1,
        };
        let (cadences, skipped) = clock_cadences(&[rule]);
        assert!(
            cadences.is_empty() && skipped.is_empty(),
            "an event rule is not a cadence at all — it is not a declaration that went missing"
        );
    }

    #[test]
    fn every_cadence_shape_carries_a_minute_interval() {
        assert_eq!(cadence_interval_minutes(Cadence::Daily), Some(1440));
        assert_eq!(cadence_interval_minutes(Cadence::Hourly), Some(60));
        assert_eq!(cadence_interval_minutes(Cadence::EveryNMinutes(5)), Some(5));
        assert_eq!(cadence_interval_minutes(Cadence::EveryNMinutes(0)), None);
    }

    #[test]
    fn only_a_plain_string_literal_is_read_as_one() {
        assert_eq!(string_literal(r#""a-b""#), Some("a-b".to_string()));
        assert_eq!(string_literal(r#"  "a"  "#), Some("a".to_string()));
        assert_eq!(string_literal("path"), None);
        assert_eq!(string_literal(r#""a" + "b""#), None);
    }
}
