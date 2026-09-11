//! Custom assertion helpers with agent-friendly failure messages.

use serde_json::Value;

/// Assert that a JSON value contains a specific field with an expected value.
/// Useful for asserting on response bodies.
pub fn assert_json_field(value: &Value, field: &str, expected: &Value) {
    let actual = value.get(field);
    if actual != Some(expected) {
        panic!(
            "\n  JSON field assertion failed\n  field: {}\n  expected: {}\n  actual: {}\n  full value: {}\n",
            field,
            expected,
            actual
                .map(|v| v.to_string())
                .unwrap_or_else(|| "MISSING".to_string()),
            value,
        );
    }
}

/// Assert that a JSON value has a specific field present (any value).
pub fn assert_json_has_field(value: &Value, field: &str) {
    if value.get(field).is_none() {
        panic!(
            "\n  JSON field {} not found\n  full value: {}\n",
            field, value,
        );
    }
}

/// Assert that `field` is PRESENT in `value` and carries an **explicit
/// `null`** — not merely that indexing it answers null.
///
/// ```ignore
/// assert_explicit_null!(patch, "backlog_item");
/// assert_explicit_null!(patch, "backlog_item", "the arrival rule must find no edge");
/// ```
///
/// WHY THIS EXISTS RATHER THAN `assert!(value["field"].is_null())`.
/// `serde_json`'s `Index` on an object answers `Value::Null` for a key
/// that was never written, so `is_null()` is true for a deliberate null
/// and for a key nobody wrote alike — an assertion that cannot fail, and
/// therefore not an assertion. Found by the builder of
/// `feat/a-car-states-its-item-at-build-start` (backlog 2e4c200f) in a
/// test whose own name claimed the distinction its one line could not make.
///
/// BOTH PLACES BOSS RELIES ON THE DISTINCTION want this, and they mean
/// different things by it, which is why the macro is named for what it
/// CHECKS rather than for either meaning — say the meaning in the
/// message:
/// - `PATCH /api/jobs/{id}/metadata` reads present-and-null as **delete
///   this key** and an absent key as **leave the recorded value alone**:
///   opposite instructions, relied on by park / adopt / supersede.
/// - An event payload or wire form states an explicit null to say
///   **nothing measured this**, where a missing key reads as a payload
///   that forgot to say (`agent_runs`).
///
/// The inverse is [`assert_absent!`].
#[macro_export]
macro_rules! assert_explicit_null {
    ($value:expr, $field:expr $(,)?) => {
        $crate::assertions::assert_explicit_null_impl(&$value, $field, None)
    };
    ($value:expr, $field:expr, $($arg:tt)+) => {
        $crate::assertions::assert_explicit_null_impl(&$value, $field, Some(format!($($arg)+)))
    };
}

/// Assert that `value` does NOT MENTION `field` at all.
///
/// The inverse of [`assert_explicit_null!`], and the reason that one
/// exists: absent and present-and-null are different facts, and a test
/// that means one must not be satisfied by the other.
#[macro_export]
macro_rules! assert_absent {
    ($value:expr, $field:expr $(,)?) => {
        $crate::assertions::assert_absent_impl(&$value, $field, None)
    };
    ($value:expr, $field:expr, $($arg:tt)+) => {
        $crate::assertions::assert_absent_impl(&$value, $field, Some(format!($($arg)+)))
    };
}

/// Behind [`assert_explicit_null!`]. `#[track_caller]` so the panic names
/// the failing assertion's own line rather than this file's — a verdict
/// must name what failed.
#[track_caller]
pub fn assert_explicit_null_impl(value: &Value, field: &str, context: Option<String>) {
    let why = context.map(|c| format!("\n  {c}")).unwrap_or_default();
    let Some(map) = value.as_object() else {
        panic!(
            "\n  `{field}` cannot be explicitly null in something that is not a JSON object: {value}{why}\n"
        );
    };
    match map.get(field) {
        Some(&Value::Null) => {}
        Some(v) => {
            panic!("\n  `{field}` must be PRESENT AND NULL, but it carries {v}: {value}{why}\n")
        }
        None => panic!(
            "\n  `{field}` must be PRESENT AND NULL, but it was never mentioned — absent is a \
             different fact from an explicit null, and the reader acts on the difference: {value}{why}\n"
        ),
    }
}

/// Behind [`assert_absent!`].
#[track_caller]
pub fn assert_absent_impl(value: &Value, field: &str, context: Option<String>) {
    let why = context.map(|c| format!("\n  {c}")).unwrap_or_default();
    match value.as_object().and_then(|m| m.get(field)) {
        None => {}
        Some(&Value::Null) => panic!(
            "\n  `{field}` must be ABSENT, but it is present and explicitly null — a different \
             fact, which a metadata door reads as DELETE: {value}{why}\n"
        ),
        Some(v) => panic!("\n  `{field}` must be ABSENT, but it carries {v}: {value}{why}\n"),
    }
}

/// Assert that a DERIVED collection still holds at least `floor` items,
/// on the line above the per-item assertion that ranges over it.
///
/// ```ignore
/// let stations = derived_stations(&seedable_platform_workflows(), &[], now());
/// assert_roster_floor!(stations, 30, "the platform protocol bundle's constraint queues");
/// for s in &stations { assert!(...); }
/// ```
///
/// WHY THIS EXISTS. A per-item assertion over a derived set is **two
/// claims** — *the set is the right set*, and *each member satisfies P* —
/// and only the second is ever written. An empty set satisfies the second
/// one **vacuously**: `for x in roster() { assert!(..) }` over `vec![]`
/// passes, loudly green, having checked nothing. So a derivation that
/// legitimately returns fewer items — a retirement, a migration, a move —
/// silently weakens every assertion over it instead of reddening. This
/// macro is the first claim, written down.
///
/// Not hypothetical: `boss_jobs::registry::platform_workflows()` went to
/// `vec![]` on 2026-09-11 (train #311) when four protocols moved from code
/// to registry data, and four tests that ranged over it stayed green
/// checking nothing. The builder who emptied it found that by hand
/// (backlog `024c0db2`). This codebase is full of derived rosters
/// *because* CLAUDE.md §9a pushes lists into directories — the lint
/// roster is a directory, the schema order is a directory, the dispatcher
/// rules are a directory, the protocol bundle is a directory — and every
/// one of them is iterated by tests.
///
/// A FLOOR, NEVER AN EXACT COUNT. An exact total is a second copy of the
/// thing it counts (§9a) and has to be edited on every legitimate change,
/// which is the cost the pin was supposed to avoid. Set the floor
/// somewhere below today's real number, high enough that a roster which
/// collapsed to a handful fails: the prior art is the rule-name lint's
/// 60-of-60 non-vacuity guard and the gate's compile-input index floor of
/// 20 against a real 46. State today's number in the message so the next
/// reader knows the headroom.
///
/// AND IT WRITES ONE CLAIM, NOT BOTH. This deliberately does NOT take the
/// predicate: the per-item loop's value here is its failure prose, which
/// names the protocol, the station, the rule that broke — "a verdict must
/// name what failed" (CLAUDE.md §Diagnosis). Folding the loop into a
/// closure returning `bool` would trade that verdict for `false`. So the
/// guard is one line above the loop, and its absence is what review looks
/// for.
///
/// A floor of `0` is refused: it is the vacuity this exists to stop,
/// spelled as a guard.
#[macro_export]
macro_rules! assert_roster_floor {
    ($items:expr, $floor:expr $(,)?) => {
        $crate::assertions::assert_roster_floor_impl($items.len(), $floor, None)
    };
    ($items:expr, $floor:expr, $($arg:tt)+) => {
        $crate::assertions::assert_roster_floor_impl(
            $items.len(),
            $floor,
            Some(format!($($arg)+)),
        )
    };
}

/// Behind [`assert_roster_floor!`]. `#[track_caller]` so the panic names
/// the guarded test's own line, not this file's.
#[track_caller]
pub fn assert_roster_floor_impl(found: usize, floor: usize, context: Option<String>) {
    let why = context
        .map(|c| format!("\n  the roster: {c}"))
        .unwrap_or_default();
    assert!(
        floor > 0,
        "\n  a floor of 0 is not a floor — every universally quantified assertion is satisfied \
         by the empty set, which is the whole defect this guard exists to stop. Pick a number \
         below the derivation's real count and above a collapse.{why}\n"
    );
    if found < floor {
        panic!(
            "\n  DERIVED ROSTER BELOW ITS FLOOR: the derivation answered {found} item(s), and \
             the per-item assertions below this line need at least {floor} to be saying \
             anything.{why}\n\n  \
             A per-item assertion over a derived set is TWO claims — that the set is the right \
             set, and that each member satisfies P — and an empty or thinned set satisfies the \
             second one VACUOUSLY, staying green while checking nothing. This line is the first \
             claim.\n\n  \
             If the derivation broke, fix the derivation. If it legitimately shrank — a \
             retirement, a migration, a move — lower the floor IN THE SAME COMMIT that shrank \
             it, and say why in the message. Never raise it to an exact total: that is a second \
             copy of the thing it counts (CLAUDE.md §9a).\n"
        );
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// THE CLAIM THESE MACROS EXIST FOR, PINNED RATHER THAN ASSERTED IN
    /// PROSE. `serde_json`'s `Index` answers `Value::Null` for a key that
    /// was never written, so the two facts the macros separate are
    /// INDISTINGUISHABLE through `[...]` — and a comment saying so is not
    /// a mechanism (§9a). If a future `serde_json` ever made indexing
    /// panic or answer something else instead, this is the line that says
    /// the macros' reason for existing has changed.
    #[test]
    fn indexing_cannot_tell_an_absent_key_from_an_explicit_null() {
        let absent = json!({});
        let explicit = json!({ "hold": null });
        assert!(absent["hold"].is_null(), "an ABSENT key indexes to null");
        assert!(explicit["hold"].is_null(), "so does an explicit null");
        assert_eq!(
            absent["hold"], explicit["hold"],
            "indexing gives one answer to two different questions — which is why \
             `value[\"k\"].is_null()` asserts nothing about either"
        );
        // The distinction survives only through the map.
        assert_ne!(absent.get("hold"), explicit.get("hold"));
    }

    #[test]
    fn an_explicit_null_is_accepted() {
        assert_explicit_null!(json!({ "hold": null }), "hold");
        assert_explicit_null!(&json!({ "hold": null }), "hold", "and takes prose too");
    }

    /// THE TEST THE DEFECT CLASS DID NOT HAVE, and the only reason this
    /// macro is worth writing: it FAILS on an absent key, which is the
    /// one thing `value[field].is_null()` cannot do.
    #[test]
    #[should_panic(expected = "never mentioned")]
    fn an_absent_key_is_not_an_explicit_null() {
        assert_explicit_null!(json!({ "other": 1 }), "hold");
    }

    #[test]
    #[should_panic(expected = "but it carries \"yes\"")]
    fn a_real_value_is_not_an_explicit_null() {
        assert_explicit_null!(json!({ "hold": "yes" }), "hold");
    }

    #[test]
    fn assert_absent_is_the_inverse() {
        assert_absent!(json!({ "other": 1 }), "hold");
    }

    /// THE PAIR ARE OPPOSITES, not two spellings of one check — which is
    /// the whole fact the defect erased.
    #[test]
    #[should_panic(expected = "present and explicitly null")]
    fn an_explicit_null_is_not_absent() {
        assert_absent!(json!({ "hold": null }), "hold");
    }

    /// Not every `Value` is an object — a function that returned
    /// `Value::Null` where a patch belongs must not read as a null field.
    #[test]
    #[should_panic(expected = "not a JSON object")]
    fn a_non_object_has_no_fields_to_null() {
        assert_explicit_null!(json!(null), "hold");
    }

    /// THE DEFECT, PINNED. This is the shape the macro exists to stop,
    /// written out so the next reader can see that it PASSES: a
    /// universally quantified assertion over the empty set is satisfied,
    /// so a test body identical to a real one checks nothing and reports
    /// green. `platform_workflows()` going to `vec![]` on 2026-09-11 is
    /// the live instance (backlog `024c0db2`).
    #[test]
    fn a_per_item_assertion_over_an_empty_roster_passes_checking_nothing() {
        let emptied_roster: Vec<&str> = Vec::new();
        let mut checked = 0usize;
        for item in &emptied_roster {
            checked += 1;
            assert!(item.is_empty(), "a claim no member is ever asked to meet");
        }
        assert_eq!(
            checked, 0,
            "the loop body never ran, and the test still passed — which is why the guard has to \
             be a separate line"
        );
    }

    /// And the guard is what turns that green into a red.
    #[test]
    #[should_panic(expected = "DERIVED ROSTER BELOW ITS FLOOR")]
    fn the_floor_reds_the_same_emptied_roster() {
        let emptied_roster: Vec<&str> = Vec::new();
        assert_roster_floor!(emptied_roster, 4, "a roster that used to hold four");
    }

    #[test]
    fn a_roster_at_or_above_its_floor_is_accepted() {
        let roster = ["a", "b", "c", "d"];
        assert_roster_floor!(roster, 4);
        assert_roster_floor!(roster, 2, "and takes prose too, like its siblings");
    }

    /// A THINNED roster, not just an emptied one. The defect class is not
    /// "the list went to zero" — it is "the list got smaller and nobody
    /// was told", which is what a floor above a collapse catches.
    #[test]
    #[should_panic(expected = "answered 1 item(s)")]
    fn a_roster_thinned_below_its_floor_also_reds() {
        let roster = ["the last one standing"];
        assert_roster_floor!(roster, 6, "was seven on 2026-09-11");
    }

    /// A floor of zero is the vacuity spelled as a guard, so the helper
    /// refuses it rather than accepting a line that cannot fail — the
    /// same reason `assert_explicit_null!` exists above.
    #[test]
    #[should_panic(expected = "a floor of 0 is not a floor")]
    fn a_floor_of_zero_is_refused() {
        let roster: Vec<&str> = Vec::new();
        assert_roster_floor!(roster, 0);
    }

    /// The message has to carry both numbers: a verdict must name what
    /// failed (CLAUDE.md §Diagnosis), and here that means what the
    /// derivation answered as well as what was expected.
    #[test]
    fn the_verdict_names_the_count_the_floor_and_the_roster() {
        let err = std::panic::catch_unwind(|| {
            let roster = ["one", "two"];
            super::assert_roster_floor_impl(
                roster.len(),
                9,
                Some("the platform protocol bundle".to_string()),
            );
        })
        .expect_err("below the floor");
        let msg = err
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or("");
        for needle in ["2 item(s)", "at least 9", "the platform protocol bundle"] {
            assert!(
                msg.contains(needle),
                "the verdict must say `{needle}`: {msg}"
            );
        }
    }
}
