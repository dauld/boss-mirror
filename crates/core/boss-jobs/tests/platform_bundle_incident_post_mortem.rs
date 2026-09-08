//! `infra/platform/workflows/incident-post-mortem.toml` keeps its
//! decided shape. One pin file per kind file (see `platform_bundle.rs`),
//! so a new protocol touches no shared line.

use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

/// A fresh incident-post-mortem does not supersede itself (backlog-item
/// cb9661fe). v1's `superseded` terminal read `superseded_by != ""`,
/// and a missing metadata path is `Absent` — unequal to every literal —
/// so the terminal was ready the instant a packet opened and the
/// dispatcher closed it before a step could be filled. The bundle row
/// gates on a PRESENT, NON-EMPTY marker: false while it is absent or
/// empty, true only once someone names the packet that replaces this
/// one. Evaluated with the same engine the dispatcher runs, over the
/// payload a freshly-opened Job presents (the trigger done, nothing
/// else, no job metadata).
#[test]
fn a_fresh_incident_post_mortem_does_not_supersede_itself() {
    use boss_expr::{Context, NoHelpers, Value, eval, parse};
    let ipm = bundled("incident-post-mortem");
    let superseded = ipm
        .steps
        .iter()
        .find(|s| s.title == "superseded")
        .expect("a `superseded` terminal");
    assert_eq!(
        superseded.terminal.as_ref().map(|t| t.outcome.as_str()),
        Some("superseded")
    );
    let predicate = parse(&superseded.ready_when).expect("the predicate parses");
    let fires = |job_metadata: serde_json::Value| {
        let payload = serde_json::json!({
            "subject": {},
            "job": { "metadata": job_metadata },
            "steps": { "opened": { "done": true, "metadata": {} } },
        });
        eval(
            &predicate,
            &Context {
                payload: &payload,
                helpers: &NoHelpers,
            },
        )
    };
    assert_eq!(
        fires(serde_json::json!({})),
        Ok(Value::Bool(false)),
        "a packet opened with no `superseded_by` must stay open"
    );
    assert_eq!(
        fires(serde_json::json!({ "superseded_by": "" })),
        Ok(Value::Bool(false)),
        "an empty marker is no marker"
    );
    assert_eq!(
        fires(serde_json::json!({ "superseded_by": "9bfd0b89" })),
        Ok(Value::Bool(true)),
        "naming the superseding packet is what closes this one"
    );
}
