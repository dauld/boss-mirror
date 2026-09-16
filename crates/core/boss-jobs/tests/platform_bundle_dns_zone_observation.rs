//! `infra/platform/workflows/dns-zone-observation.toml` keeps its
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

/// The zone observation packet (design 4c565f8c, backlog 5e58922c):
/// the `dns.observe` handler completes `observe` BY SLUG with the
/// verdict list, and the terminals fork on `result`. This pins the spec
/// side of that contract so a reshape cannot strand the handler — or
/// nominate a human for a step a machine does.
#[test]
fn the_zone_observation_keeps_its_decided_shape() {
    let wf = bundled("dns-zone-observation");
    assert_eq!(
        wf.subject_kinds,
        vec!["custom".to_string()],
        "the Subject is the zone"
    );

    let step = |title: &str| {
        wf.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("dns-zone-observation has no `{title}` step"))
    };

    // A machine step: nobody nominated, gated on the trigger.
    let observe = step("observe");
    assert_eq!(
        observe.authority_role, None,
        "observe is completed by the dns.observe handler, not nominated to a person"
    );
    assert!(observe.ready_when.contains("steps.due.done"));

    // Required at done: the per-record verdict list, each element naming
    // its record and verdict (the probe for 5e58922c reads exactly this),
    // the comparator's summary, and the result the terminals read.
    let field = |name: &str| {
        observe
            .fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("observe has no `{name}` field"))
    };
    let verdicts = field("verdicts");
    assert!(verdicts.required);
    assert_eq!(verdicts.field_type, "array");
    assert_eq!(
        verdicts.item_keys,
        vec!["record".to_string(), "verdict".to_string()],
        "every verdict element names its record and its verdict — a packet without them is not an observation"
    );
    assert!(field("summary").required);
    let result = field("result");
    assert!(result.required);
    assert_eq!(
        result.field_type, "match|findings",
        "the two words the terminals fork on"
    );

    // Two terminals, one per result, both closing the packet.
    for (title, word) in [("matched", "match"), ("findings", "findings")] {
        let t = step(title);
        assert_eq!(t.kind, "outcome");
        assert_eq!(t.terminal.as_ref().map(|t| t.outcome.as_str()), Some(title));
        assert!(
            t.ready_when
                .contains(&format!("steps.observe.metadata.result = \"{word}\"")),
            "`{title}` forks on result = {word}; got `{}`",
            t.ready_when
        );
    }
}
