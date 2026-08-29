//! Can a packet admitted under one protocol version be re-pinned to
//! the next one, automatically?
//!
//! **The problem.** A Job is an immutable envelope plus a protocol set
//! fixed at admission, and in-flight packets stay pinned to the version
//! they were admitted under. That is what makes protocols cheap to
//! edit — publishing v2 cannot disturb work already moving. But it also
//! means every edit strands its in-flight packets on the old version,
//! and the only remedies today are to wait them out or to convert each
//! one by hand.
//!
//! David, 2026-08-18: "we should be able to convert the jobs to the new
//! protocol. In fact, I think we should consider a job protocol
//! translation service. That could figure out whether a packet is
//! convertible from one protocol to the next automatically, for example
//! because the new protocol is strictly looser, and I think we may do
//! this a lot."
//!
//! **The relation.** `v2` is *strictly looser* than `v1` when re-pinning
//! cannot invalidate any state a packet might currently be in, and
//! cannot retroactively demand evidence a packet has already moved past.
//! Loosening is the common shape of a protocol edit — widening who may
//! claim a step, dropping a sign-off, lowering an assurance bar — and
//! every one of those is safe by construction: nothing a packet has
//! already done stops being enough.
//!
//! **This answers a question about SHAPE, not about any one packet.**
//! If v2 is looser than v1, EVERY in-flight v1 packet converts, whatever
//! state it is in — that is what makes the check worth running once per
//! version pair instead of once per packet. When the verdict is
//! `NeedsReview`, some packets may still convert (a stricter step the
//! packet already completed cannot hurt it), but deciding that needs the
//! packet, and this function deliberately does not look at one.
//!
//! **Conservative on purpose.** Every reason this reports is a case
//! where a packet COULD be invalidated, not one where it necessarily is.
//! `NeedsReview` means "a human or a packet-aware pass decides", never
//! "impossible". The failure we cannot afford is the opposite one:
//! silently re-pinning a packet onto a protocol that demands something
//! it never collected, which turns a completed step into a lie about
//! evidence that was never produced.

use crate::registry::{StepSpec, WorkflowSpec};
use boss_core::job::Assurance;
use std::collections::{BTreeMap, BTreeSet};

/// Why a version pair is not automatically convertible. Each names the
/// step it concerns (or `None` for a workflow-level reason) so an
/// operator reading the verdict knows where to look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Obstacle {
    /// Step slug (`StepSpec::title`), or `None` for workflow-level.
    pub step: Option<String>,
    /// What got stricter, in words.
    pub reason: String,
}

impl Obstacle {
    fn workflow(reason: impl Into<String>) -> Self {
        Self {
            step: None,
            reason: reason.into(),
        }
    }
    fn step(slug: &str, reason: impl Into<String>) -> Self {
        Self {
            step: Some(slug.to_string()),
            reason: reason.into(),
        }
    }
}

/// The verdict for a version pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Convertibility {
    /// `to` is strictly looser than (or identical to) `from`. Every
    /// in-flight packet on `from` can be re-pinned to `to` without
    /// inspecting it.
    Automatic,
    /// Something got stricter, or changed in a way this check cannot
    /// prove safe. Not a refusal — a referral.
    NeedsReview(Vec<Obstacle>),
}

impl Convertibility {
    pub fn is_automatic(&self) -> bool {
        matches!(self, Self::Automatic)
    }
    /// The obstacles, empty when automatic.
    pub fn obstacles(&self) -> &[Obstacle] {
        match self {
            Self::Automatic => &[],
            Self::NeedsReview(o) => o,
        }
    }
}

/// How hard a stamp is to produce. Raising this bar is stricter;
/// lowering it is looser. `None` means the spec did not say, which the
/// runtime reads as the default (`Session`, the weakest) — so an
/// unstated bar and an explicit `Session` are the same requirement and
/// must compare equal, or every protocol that merely *writes down* its
/// existing default would read as a tightening.
fn assurance_rank(a: Option<Assurance>) -> u8 {
    match a.unwrap_or_default() {
        Assurance::Session => 0,
        Assurance::Presence => 1,
    }
}

/// Fields a step REQUIRES at completion. Optional fields are ignored:
/// adding one asks nothing of a packet.
fn required_fields(s: &StepSpec) -> BTreeSet<&str> {
    s.fields
        .iter()
        .filter(|f| f.required)
        .map(|f| f.name.as_str())
        .collect()
}

/// Is `to` strictly looser than `from` — can every in-flight packet on
/// `from` be re-pinned to `to` without being inspected?
///
/// Steps are matched by `title`, which is the stable slug within a
/// workflow (the same identifier `ready_when` predicates reference).
pub fn convertibility(from: &WorkflowSpec, to: &WorkflowSpec) -> Convertibility {
    let mut obstacles = Vec::new();

    if from.kind != to.kind {
        obstacles.push(Obstacle::workflow(format!(
            "different protocols entirely: {} vs {} — conversion is a \
             re-admission, not a re-pin",
            from.kind, to.kind
        )));
        // Nothing below this is meaningful across two different
        // protocols; step slugs that happen to collide would produce
        // noise, not information.
        return Convertibility::NeedsReview(obstacles);
    }

    // A packet's subject was validated at admission against `from`.
    // Narrowing the set can strand a packet whose subject kind is no
    // longer admissible.
    let from_subjects: BTreeSet<&str> = from.subject_kinds.iter().map(String::as_str).collect();
    let to_subjects: BTreeSet<&str> = to.subject_kinds.iter().map(String::as_str).collect();
    for dropped in from_subjects.difference(&to_subjects) {
        obstacles.push(Obstacle::workflow(format!(
            "subject kind `{dropped}` is no longer admissible — a packet \
             about one has nowhere to land"
        )));
    }

    let from_steps: BTreeMap<&str, &StepSpec> =
        from.steps.iter().map(|s| (s.title.as_str(), s)).collect();
    let to_steps: BTreeMap<&str, &StepSpec> =
        to.steps.iter().map(|s| (s.title.as_str(), s)).collect();

    // A step that exists on a materialized packet but not in the new
    // protocol is orphaned: it has a status, possibly evidence, and
    // nothing to validate it against.
    for slug in from_steps.keys() {
        if !to_steps.contains_key(slug) {
            obstacles.push(Obstacle::step(
                slug,
                "step removed — a materialized step would be orphaned, \
                 with evidence no protocol describes",
            ));
        }
    }

    // A new step is work the packet has not done. Harmless for a packet
    // that has not passed it, fatal for one that is already at a
    // terminal — and which of those is true depends on the packet.
    for slug in to_steps.keys() {
        if !from_steps.contains_key(slug) {
            obstacles.push(Obstacle::step(
                slug,
                "step added — packets past this point in the flow would \
                 gain work they have already moved beyond",
            ));
        }
    }

    // ORDER IS PART OF THE CONTRACT, because the runtime does not match
    // steps the way this function does.
    //
    // Everything above compares steps BY TITLE, through a BTreeMap. The
    // engine pairs them POSITIONALLY: step predicates are not stored on
    // the step row (there is no `ready_when` column), so readiness is
    // recomputed by zipping the spec's steps against the job's steps in
    // order (http/steps.rs, `A JOB'S STEP SET IS FIXED AT ADMISSION`).
    //
    // So a version that merely REORDERS the same steps is invisible to
    // every check above — same titles, same specs, same sets — and this
    // function would call it Automatic. Re-pinning on that verdict
    // misaligns every pair, and `registry::reevaluate` then refuses to
    // advance anything: the packet freezes with its terminal pending,
    // never closes, and never leaves its owner's queue.
    //
    // That is not hypothetical, and it is why this compares sequences
    // rather than sets. Design review 32a4e70d gained a step on
    // 2026-08-13 and froze exactly this way, producing feedback
    // 55c92985 — "I finished the top design review and it still shows
    // the same metadata and is in the same queue." A conversion path
    // acting on a wrong Automatic would do that to every in-flight
    // packet at once, and the divergence is only logged at warn.
    //
    // Compares the RELATIVE order of steps present in both specs, so it
    // stays meaningful when steps were also added or removed (each of
    // which is reported above on its own terms).
    let from_common: Vec<&str> = from
        .steps
        .iter()
        .map(|s| s.title.as_str())
        .filter(|t| to_steps.contains_key(t))
        .collect();
    let to_common: Vec<&str> = to
        .steps
        .iter()
        .map(|s| s.title.as_str())
        .filter(|t| from_steps.contains_key(t))
        .collect();
    if from_common != to_common {
        obstacles.push(Obstacle::workflow(format!(
            "steps reordered ({} -> {}) — readiness is recomputed by pairing \
             spec steps to job steps POSITIONALLY, so re-pinning would \
             misalign every pair and freeze the packet",
            from_common.join(", "),
            to_common.join(", ")
        )));
    }

    for (slug, f) in &from_steps {
        let Some(t) = to_steps.get(slug) else {
            continue;
        };

        // The predicate decides when a step becomes ready. Editing it
        // can un-ready a ready step or re-ready a completed one. A
        // WEAKER predicate is genuinely safe, but proving implication
        // between two expressions is a different piece of work than
        // this function, so any change is referred rather than guessed.
        if f.ready_when != t.ready_when {
            obstacles.push(Obstacle::step(
                slug,
                format!(
                    "`ready_when` changed ({} -> {}) — this check does not \
                     prove one predicate implies the other, so it refers \
                     rather than assumes",
                    f.ready_when, t.ready_when
                ),
            ));
        }

        // Authority. Widening is the loosening we expect most often:
        // `Some(role)` -> `None` opens a step to any authorized actor.
        match (f.authority_role.as_deref(), t.authority_role.as_deref()) {
            (Some(a), Some(b)) if a != b => obstacles.push(Obstacle::step(
                slug,
                format!("authority changed `{a}` -> `{b}` — neither contains the other"),
            )),
            (None, Some(b)) => obstacles.push(Obstacle::step(
                slug,
                format!("authority narrowed to `{b}` — a step claimed by someone else is now unclaimable"),
            )),
            // Some -> None is a widening, None -> None and equal roles
            // are no change. All safe.
            _ => {}
        }

        // Sign-offs. Requiring a NEW role means a step already
        // completed under `from` is retroactively missing a stamp.
        let f_signs: BTreeSet<&str> = f.sign_offs_required.iter().map(String::as_str).collect();
        let t_signs: BTreeSet<&str> = t.sign_offs_required.iter().map(String::as_str).collect();
        for added in t_signs.difference(&f_signs) {
            obstacles.push(Obstacle::step(
                slug,
                format!("sign-off `{added}` added — completed steps would be missing a stamp"),
            ));
        }

        if assurance_rank(t.assurance_required) > assurance_rank(f.assurance_required) {
            obstacles.push(Obstacle::step(
                slug,
                "assurance raised — stamps already collected were produced \
                 under a weaker bar and cannot be upgraded after the fact",
            ));
        }

        // Required completion fields. Adding one asks for evidence that
        // a completed step never collected.
        for added in required_fields(t).difference(&required_fields(f)) {
            obstacles.push(Obstacle::step(
                slug,
                format!("required field `{added}` added — completed steps never collected it"),
            ));
        }

        // Terminals name the outcome stamped on close. Changing or
        // removing one changes what a closed packet means.
        match (&f.terminal, &t.terminal) {
            (Some(a), Some(b)) if a.outcome != b.outcome => obstacles.push(Obstacle::step(
                slug,
                format!(
                    "terminal outcome changed `{}` -> `{}` — closed packets \
                     carry the old label",
                    a.outcome, b.outcome
                ),
            )),
            (Some(a), None) => obstacles.push(Obstacle::step(
                slug,
                format!("terminal `{}` removed — a packet closed here has an outcome the protocol no longer declares", a.outcome),
            )),
            // None -> Some adds a terminal, which strands no existing
            // packet: nothing has closed on a step that was not
            // terminal when they were admitted.
            _ => {}
        }
    }

    if obstacles.is_empty() {
        Convertibility::Automatic
    } else {
        Convertibility::NeedsReview(obstacles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Terminal;
    use boss_core::job::StepField;

    fn step(title: &str) -> StepSpec {
        StepSpec {
            title: title.to_string(),
            kind: "task".to_string(),
            ready_when: "true".to_string(),
            terminal: None,
            title_template: String::new(),
            sign_offs_required: Vec::new(),
            assurance_required: None,
            duration_hours: None,
            fields: Vec::new(),
            authority_role: None,
            claimable: None,
            metadata_defaults: serde_json::json!({}),
        }
    }

    fn wf(steps: Vec<StepSpec>) -> WorkflowSpec {
        WorkflowSpec::platform_seed(
            "design-doc-review",
            "Design doc review",
            "governance",
            vec!["custom".to_string()],
            steps,
        )
    }

    fn field(name: &str, required: bool) -> StepField {
        StepField {
            name: name.to_string(),
            field_type: "string".to_string(),
            required,
        }
    }

    #[test]
    fn a_protocol_is_convertible_to_itself() {
        // Idempotence. Republishing an unchanged protocol must not
        // strand its own packets.
        let a = wf(vec![step("review")]);
        assert_eq!(convertibility(&a, &a), Convertibility::Automatic);
    }

    #[test]
    fn widening_authority_converts_automatically() {
        // THE CASE THIS WAS BUILT FOR. Loosening design-doc-review so
        // an agent may perform the review, not only David, is exactly
        // `Some(role)` -> `None` on the review step.
        let mut narrow = step("review");
        narrow.authority_role = Some("cto".to_string());
        let wide = step("review");
        let v = convertibility(&wf(vec![narrow]), &wf(vec![wide]));
        assert!(
            v.is_automatic(),
            "opening a step to more actors cannot invalidate a packet: {:?}",
            v.obstacles()
        );
    }

    /// THE CASE EVERY OTHER CHECK IS BLIND TO.
    ///
    /// Same titles, same specs, same sets — only the sequence moved.
    /// Every comparison in this function goes through a BTreeMap keyed
    /// by title, so before this check the verdict was Automatic. The
    /// engine pairs spec steps to job steps positionally, so acting on
    /// that verdict would misalign every pair and freeze the packet
    /// with its terminal pending (the 32a4e70d / 55c92985 failure).
    #[test]
    fn reordering_the_same_steps_is_not_automatic() {
        let forward = wf(vec![step("triage"), step("review"), step("closed")]);
        let swapped = wf(vec![step("review"), step("triage"), step("closed")]);

        let v = convertibility(&forward, &swapped);
        assert!(
            !v.is_automatic(),
            "a pure reorder changes which spec step each job step is paired \
             with; nothing else in this function can see it"
        );
        let o = &v.obstacles()[0];
        assert_eq!(o.step, None, "reordering is a workflow-level fact");
        assert!(
            o.reason.contains("reordered") && o.reason.contains("POSITIONALLY"),
            "the reason must name the pairing that breaks: {:?}",
            o.reason
        );
    }

    /// ...and the check must not fire on an unchanged sequence, or every
    /// ordinary loosening would be referred for review and the
    /// Automatic verdict would stop meaning anything.
    #[test]
    fn keeping_the_order_stays_automatic() {
        let before = wf(vec![step("triage"), step("review"), step("closed")]);
        let after = wf(vec![step("triage"), step("review"), step("closed")]);
        assert!(convertibility(&before, &after).is_automatic());
    }

    /// A step added or removed is already reported on its own terms.
    /// The order check compares only the steps present in BOTH specs,
    /// so it stays quiet about a sequence that did not actually move —
    /// otherwise every add/remove would carry a second, misleading
    /// "reordered" obstacle naming steps nobody touched.
    #[test]
    fn inserting_a_step_does_not_also_report_a_phantom_reorder() {
        let before = wf(vec![step("triage"), step("review")]);
        let after = wf(vec![step("triage"), step("measure"), step("review")]);

        let v = convertibility(&before, &after);
        assert!(
            !v.is_automatic(),
            "an added step is work a packet may have passed"
        );
        assert!(
            v.obstacles()
                .iter()
                .all(|o| !o.reason.contains("reordered")),
            "triage and review kept their relative order: {:?}",
            v.obstacles()
        );
    }

    #[test]
    fn narrowing_authority_needs_review() {
        let open = step("review");
        let mut narrow = step("review");
        narrow.authority_role = Some("cto".to_string());
        let v = convertibility(&wf(vec![open]), &wf(vec![narrow]));
        assert!(!v.is_automatic());
        assert_eq!(v.obstacles()[0].step.as_deref(), Some("review"));
    }

    #[test]
    fn dropping_a_sign_off_is_looser_but_adding_one_is_not() {
        let mut with = step("review");
        with.sign_offs_required = vec!["cto".to_string()];
        let without = step("review");

        assert!(
            convertibility(&wf(vec![with.clone()]), &wf(vec![without.clone()])).is_automatic(),
            "dropping a required stamp asks less of every packet"
        );
        let tightened = convertibility(&wf(vec![without]), &wf(vec![with]));
        assert!(!tightened.is_automatic());
        assert!(
            tightened.obstacles()[0].reason.contains("missing a stamp"),
            "the reason must say what breaks: {:?}",
            tightened.obstacles()
        );
    }

    #[test]
    fn adding_a_required_field_needs_review_but_an_optional_one_does_not() {
        let bare = step("review");
        let mut optional = step("review");
        optional.fields = vec![field("note", false)];
        let mut demanded = step("review");
        demanded.fields = vec![field("note", true)];

        assert!(
            convertibility(&wf(vec![bare.clone()]), &wf(vec![optional])).is_automatic(),
            "an optional field asks nothing of a completed step"
        );
        assert!(!convertibility(&wf(vec![bare]), &wf(vec![demanded])).is_automatic());
    }

    #[test]
    fn raising_assurance_needs_review_and_lowering_it_does_not() {
        let mut session = step("review");
        session.assurance_required = Some(Assurance::Session);
        let mut presence = step("review");
        presence.assurance_required = Some(Assurance::Presence);

        assert!(
            !convertibility(&wf(vec![session.clone()]), &wf(vec![presence.clone()])).is_automatic()
        );
        assert!(convertibility(&wf(vec![presence]), &wf(vec![session])).is_automatic());
    }

    #[test]
    fn writing_down_the_default_assurance_is_not_a_tightening() {
        // `None` means "unstated", which the runtime reads as Session.
        // If this compared as weaker-than-Session, every protocol that
        // merely made its existing default explicit would read as a
        // tightening and strand its packets for no reason.
        let unstated = step("review");
        let mut explicit = step("review");
        explicit.assurance_required = Some(Assurance::Session);
        assert!(convertibility(&wf(vec![unstated]), &wf(vec![explicit])).is_automatic());
    }

    #[test]
    fn removing_a_step_orphans_it() {
        let v = convertibility(
            &wf(vec![step("review"), step("flush")]),
            &wf(vec![step("review")]),
        );
        assert!(!v.is_automatic());
        assert_eq!(v.obstacles()[0].step.as_deref(), Some("flush"));
    }

    #[test]
    fn adding_a_step_needs_review() {
        let v = convertibility(
            &wf(vec![step("review")]),
            &wf(vec![step("review"), step("audit")]),
        );
        assert!(!v.is_automatic());
        assert!(v.obstacles()[0].reason.contains("step added"));
    }

    #[test]
    fn editing_a_predicate_is_referred_not_guessed() {
        let mut edited = step("review");
        edited.ready_when = "job.metadata.ready = true".to_string();
        let v = convertibility(&wf(vec![step("review")]), &wf(vec![edited]));
        assert!(!v.is_automatic());
        assert!(
            v.obstacles()[0].reason.contains("does not prove"),
            "the verdict must be honest about WHY it referred: {:?}",
            v.obstacles()
        );
    }

    #[test]
    fn narrowing_the_admissible_subjects_strands_packets() {
        let mut from = wf(vec![step("review")]);
        from.subject_kinds = vec!["custom".to_string(), "asset".to_string()];
        let to = wf(vec![step("review")]);
        let v = convertibility(&from, &to);
        assert!(!v.is_automatic());
        assert!(v.obstacles()[0].reason.contains("asset"));
    }

    #[test]
    fn changing_a_terminal_outcome_needs_review() {
        let mut a = step("done");
        a.terminal = Some(Terminal {
            outcome: "succeeded".to_string(),
        });
        let mut b = step("done");
        b.terminal = Some(Terminal {
            outcome: "shipped".to_string(),
        });
        assert!(!convertibility(&wf(vec![a]), &wf(vec![b])).is_automatic());
    }

    #[test]
    fn two_different_protocols_are_not_a_re_pin() {
        let a = wf(vec![step("review")]);
        let mut b = wf(vec![step("review")]);
        b.kind = "ship-a-change".to_string();
        let v = convertibility(&a, &b);
        assert!(!v.is_automatic());
        assert_eq!(
            v.obstacles().len(),
            1,
            "comparing steps across unrelated protocols would be noise, not information"
        );
    }

    #[test]
    fn several_tightenings_are_all_reported_not_just_the_first() {
        // An operator deciding whether to convert needs the whole list;
        // fixing one obstacle only to be shown the next is the slow
        // version of this tool.
        let bare = step("review");
        let mut tight = step("review");
        tight.authority_role = Some("cto".to_string());
        tight.sign_offs_required = vec!["cfo".to_string()];
        tight.fields = vec![field("evidence", true)];
        let v = convertibility(&wf(vec![bare]), &wf(vec![tight]));
        assert_eq!(v.obstacles().len(), 3, "{:?}", v.obstacles());
    }
}
