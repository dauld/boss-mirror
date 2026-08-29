//! Refused step writes — the denominator the reliability metric was
//! missing.
//!
//! A completed step is the only thing the record holds today, and
//! required-at-done validation guarantees every completed step carries
//! what its protocol asked for. So conformance measures 100% and always
//! will: it certifies the validator, not the work. What it cannot see is
//! the attempt that never became a completion.
//!
//! This module is the pure half of recording those attempts — the type
//! that crosses the port and the classifier that turns an HTTP refusal
//! into a stable vocabulary. Both are free of I/O so the vocabulary is
//! pinned by unit tests rather than by a running database.

use serde::{Deserialize, Serialize};

/// The coarse reason a write was refused. Deliberately small and
/// stable: the metric groups by this, and a vocabulary that grows with
/// every new error message cannot be grouped by at all.
///
/// Mirrored by the CHECK constraint on `step_write_refusals.error_class`
/// — a fact that lives twice, so [`ErrorClass::ALL`] exists and the
/// migration names this module as the definition (CLAUDE.md §9a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorClass {
    /// The body was well-formed but did not satisfy the protocol: a
    /// missing required field, a value outside a declared enum. This is
    /// the class that means "the actor tried to do the work and the
    /// protocol said not like that".
    Validation,
    /// Refused on authority — the actor may not do this here.
    Policy,
    /// Refused on the state of the world: the step is already
    /// completed, the claim was taken, the packet moved on.
    State,
    /// The request itself was malformed — an unparseable id, a body
    /// that is not an object. The actor never reached the protocol.
    Shape,
    /// The target does not exist.
    Missing,
    Other,
}

impl ErrorClass {
    pub const ALL: [ErrorClass; 6] = [
        ErrorClass::Validation,
        ErrorClass::Policy,
        ErrorClass::State,
        ErrorClass::Shape,
        ErrorClass::Missing,
        ErrorClass::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorClass::Validation => "validation",
            ErrorClass::Policy => "policy",
            ErrorClass::State => "state",
            ErrorClass::Shape => "shape",
            ErrorClass::Missing => "missing",
            ErrorClass::Other => "other",
        }
    }
}

/// Classify a refusal from what the caller actually saw.
///
/// Status alone is not enough and neither is the message alone. A 400
/// covers both "your id is gibberish" (the actor never reached the
/// protocol) and "this step requires a `disposition`" (the actor
/// reached it and was told no) — and only the second is evidence about
/// how hard the protocol is to comply with. So the status picks the
/// family and the message separates shape from substance within it.
///
/// Matching on message prefixes is a coupling to the handlers' error
/// strings, and it is the deliberate choice: the alternative is
/// threading a class through every `return` site, which is the drift
/// this design exists to avoid. An unrecognised 400 lands in `Shape`,
/// which under-counts `Validation` rather than inventing it.
pub fn classify(status: u16, body: &str) -> ErrorClass {
    match status {
        403 => ErrorClass::Policy,
        404 => ErrorClass::Missing,
        409 => ErrorClass::State,
        422 => ErrorClass::Validation,
        400 => {
            if body.starts_with("invalid step metadata")
                || body.starts_with("invalid step fields")
                || body.starts_with("invalid job metadata")
            {
                ErrorClass::Validation
            } else {
                ErrorClass::Shape
            }
        }
        _ => ErrorClass::Other,
    }
}

/// One refused step write, on its way to the port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepWriteRefusal {
    /// `None` when the refusal was *because* the id did not parse.
    pub job_id: Option<uuid::Uuid>,
    pub step_id: Option<uuid::Uuid>,
    pub actor_id: String,
    pub method: String,
    pub path: String,
    pub status_code: u16,
    pub error_class: ErrorClass,
    pub detail: String,
}

/// A refusal as it comes back out of storage — the write shape plus
/// what only the store knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedRefusal {
    pub id: i64,
    pub refused_at: chrono::DateTime<chrono::Utc>,
    #[serde(flatten)]
    pub refusal: StepWriteRefusal,
}

/// Whether a response on a step-write route is worth recording.
///
/// 5xx is excluded: a server fault is not an actor failing to comply
/// with a protocol, and counting it would put infrastructure noise in a
/// number meant to describe how hard a step is to complete correctly.
pub fn is_refusal(status: u16) -> bool {
    (400..500).contains(&status)
}

/// Whether this request was an attempt to WRITE a step.
///
/// Reads are excluded — a refused GET says nothing about whether an
/// obligation was dischargeable. The claim and sign-off sub-routes count:
/// both are how an actor takes on or discharges a step.
pub fn is_step_write(method: &str, path: &str) -> bool {
    if !matches!(method, "POST" | "PUT" | "PATCH") {
        return false;
    }
    // /api/jobs/{id}/steps and everything under it.
    let Some(rest) = path.strip_prefix("/api/jobs/") else {
        return false;
    };
    match rest.split_once('/') {
        Some((_job, tail)) => tail == "steps" || tail.starts_with("steps/"),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_required_field_is_validation_not_shape() {
        // The case that started this: `boss prove` refused six times on
        // `method: expected type 'browser|api|log', got string`. That is
        // an actor reaching the protocol and being told no — the signal
        // the metric exists to carry — and it arrives as a 400, the same
        // status as an unparseable id.
        assert_eq!(
            classify(
                400,
                "invalid step metadata: method: expected type 'browser|api|log', got string"
            ),
            ErrorClass::Validation
        );
        assert_eq!(classify(400, "invalid job id"), ErrorClass::Shape);
        assert_eq!(
            classify(400, "body must be a JSON object"),
            ErrorClass::Shape
        );
    }

    #[test]
    fn an_unrecognised_400_undercounts_rather_than_inventing() {
        // Prefix matching couples to handler strings. When a message is
        // reworded the class must degrade to Shape, never silently
        // inflate Validation — the reading that drives protocol changes.
        assert_eq!(classify(400, "some future wording"), ErrorClass::Shape);
    }

    #[test]
    fn each_status_family_maps_to_its_own_class() {
        assert_eq!(classify(403, "denied"), ErrorClass::Policy);
        assert_eq!(classify(404, "step not found"), ErrorClass::Missing);
        assert_eq!(classify(409, "step is completed"), ErrorClass::State);
        assert_eq!(classify(422, "unprocessable"), ErrorClass::Validation);
    }

    #[test]
    fn a_server_fault_is_not_a_refusal() {
        // Counting 5xx would put infrastructure noise into a number
        // meant to describe how hard a step is to complete correctly.
        assert!(is_refusal(400));
        assert!(is_refusal(409));
        assert!(!is_refusal(500));
        assert!(!is_refusal(200));
        assert!(!is_refusal(204));
    }

    #[test]
    fn only_step_writes_count() {
        let job = "/api/jobs/9fa67fb9-ba93-4383-a46e-542983c3bc54/steps";
        assert!(is_step_write("POST", job));
        assert!(is_step_write("PUT", &format!("{job}/5d404e64")));
        assert!(is_step_write("POST", &format!("{job}/5d404e64/claim")));
        assert!(is_step_write("POST", &format!("{job}/5d404e64/sign-offs")));

        // A refused READ says nothing about whether an obligation was
        // dischargeable.
        assert!(!is_step_write("GET", job));
        // Neighbouring job routes are not step writes.
        assert!(!is_step_write(
            "PATCH",
            "/api/jobs/9fa67fb9-ba93-4383-a46e-542983c3bc54/metadata"
        ));
        assert!(!is_step_write("POST", "/api/jobs"));
        assert!(!is_step_write("GET", "/api/stations/load"));
    }

    #[test]
    fn the_class_vocabulary_matches_the_check_constraint() {
        // §9a: this list lives here and in the migration's CHECK. It
        // cannot be collapsed across a language boundary, so it is
        // pinned — if a variant is added without touching the SQL, the
        // insert fails at runtime on a path nobody exercises locally.
        let names: Vec<&str> = ErrorClass::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            names,
            vec!["validation", "policy", "state", "shape", "missing", "other"],
            "keep in step with the CHECK in \
             infra/postgres/schema/202608291750-a-refused-step-write-is-recorded.sql"
        );
    }
}
