//! A key a human signs has one declared writer, and the server — not
//! the caller — says who that writer is (design f623e425, David
//! 2026-09-25; backlog 6c9183de).
//!
//! WHY. An ops-request's `approve` step carries the keys the runner
//! renders on the host — `plan`, `verb`, `host`, `args`,
//! `rendered_plan_sha256` — and a passkey stamp binds the step's whole
//! shape. Until this module any caller with Update on the step could
//! merge those keys, so the passkey could be asked to sign a plan the
//! runner never rendered. The runner's re-render and the ceremony
//! binding REFUSE a swap after the fact; nothing PREVENTED one.
//!
//! THE DECLARATION IS REGISTRY DATA. A Workflow row names the one
//! writer of a key on the field itself (`StepField::writer`, e.g.
//! `writer = "runner:ops"`); the field list is fixed at admission and
//! the step PUT refuses a body that changes it (b433bdf3), so the
//! declaration cannot be written away by the caller it binds. A step
//! with no declared writer answers exactly as before.
//!
//! WHO THE WRITER IS NEVER COMES FROM THE CALLER. The `x-boss-user` id
//! is self-asserted: every machine-door caller holds the same estate
//! token and can type `automation:ops-runner`. A rule keyed on that id
//! is design option D, rejected because a guard that LOOKS like
//! protection against the adversary the review named, and is not, is a
//! mostly-sure guard. So a declared writer is satisfied ONLY by a
//! [`CredentialedCaller`] request extension, which a client cannot set:
//! only a server-side door that resolved a presented credential inserts
//! one. Until that door is mounted (the credential kind, its broker
//! handler and the resolve step are the next cars of design f623e425),
//! no caller satisfies a declared writer, which is why no live protocol
//! declares one yet — the declaration lands on the ops-request row with
//! the credential delivery, never before it.
//!
//! THE HOST BINDING. A credential bound to a host writes only packets
//! whose `host` (job metadata) is that host — "a runner for host h
//! writes only requests whose host is h" — so the credential of one
//! runner cannot author the plan another host's runner is asked to run.

use boss_core::job::StepField;
use serde::Serialize;
use serde_json::Value;

/// The caller as a server-side credential door resolved it. Inserted
/// into the request's extensions by that door and by nothing else; a
/// client has no way to set a request extension, which is the whole
/// point — this is the only identity a declared writer believes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialedCaller {
    /// The writer name the credential satisfies (`runner:ops`).
    pub principal: String,
    /// The actor the credential belongs to, for the record.
    pub actor_id: String,
    /// The host the credential is bound to, when it is bound to one.
    pub host: Option<String>,
}

/// A key whose value this write would change, and the writer its
/// protocol declares for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReservedKey {
    pub key: String,
    pub writer: String,
}

/// The job-metadata key a host-bound credential is judged against.
pub const HOST_KEY: &str = "host";

/// Every key with a declared writer whose value differs between the
/// stored metadata and the metadata as this write would leave it. A key
/// added, changed or removed (`null` through the merge door) is a
/// change; an unchanged re-send is not, so a read-modify-write caller
/// that sends the stored value back is never refused for it.
pub fn reserved_keys_changed(fields: &[StepField], old: &Value, new: &Value) -> Vec<ReservedKey> {
    fields
        .iter()
        .filter_map(|f| {
            let writer = f.writer.as_deref()?;
            (old.get(&f.name) != new.get(&f.name)).then(|| ReservedKey {
                key: f.name.clone(),
                writer: writer.to_string(),
            })
        })
        .collect()
}

/// Whether `caller` is the declared `writer` for a packet whose `host`
/// is `job_host`. `Err` carries why not, in words the refusal returns.
pub fn admits(
    caller: Option<&CredentialedCaller>,
    writer: &str,
    job_host: Option<&str>,
) -> Result<(), String> {
    let Some(caller) = caller else {
        return Err(format!(
            "the request presented no credential the server resolved, and only a credential \
             for `{writer}` may write this key — the `x-boss-user` id is self-asserted and is \
             never read as a writer"
        ));
    };
    if caller.principal != writer {
        return Err(format!(
            "the presented credential is for `{}`, not `{writer}`",
            caller.principal
        ));
    }
    match (caller.host.as_deref(), job_host) {
        (None, _) => Ok(()),
        (Some(bound), Some(host)) if bound == host => Ok(()),
        (Some(bound), Some(host)) => Err(format!(
            "the presented credential is bound to host `{bound}`, and this packet's host is `{host}`"
        )),
        (Some(bound), None) => Err(format!(
            "the presented credential is bound to host `{bound}`, and this packet names no host"
        )),
    }
}

/// The reserved keys this caller may not change, each with why. Empty
/// means the write may proceed as far as declared writers go.
pub fn refused(
    reserved: &[ReservedKey],
    caller: Option<&CredentialedCaller>,
    job_host: Option<&str>,
) -> Vec<(ReservedKey, String)> {
    reserved
        .iter()
        .filter_map(|r| {
            admits(caller, &r.writer, job_host)
                .err()
                .map(|why| (r.clone(), why))
        })
        .collect()
}

/// The 409 body: the step, the door, every refused key with its
/// declared writer, who asked (the self-asserted id, reported and not
/// believed) and the resolved credential when there was one — the shape
/// the `human_only` refusal takes, so a caller reads both the same way.
pub fn refusal_body(
    step_id: &str,
    step_title: &str,
    door: &str,
    asked_by: &str,
    caller: Option<&CredentialedCaller>,
    refused: &[(ReservedKey, String)],
) -> Value {
    serde_json::json!({
        "error": "this write changes a key its protocol reserves to one declared writer",
        "step_id": step_id,
        "step_title": step_title,
        "door": door,
        "asked_by": asked_by,
        "credential": caller.map(|c| serde_json::json!({
            "principal": c.principal,
            "actor_id": c.actor_id,
            "host": c.host,
        })),
        "refused_keys": refused
            .iter()
            .map(|(r, why)| serde_json::json!({ "key": r.key, "writer": r.writer, "why": why }))
            .collect::<Vec<_>>(),
        "rule": "a key a human signs has one declared writer, and the server knows who that \
                 writer is (design f623e425; backlog 6c9183de)",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(name: &str, writer: Option<&str>) -> StepField {
        StepField {
            name: name.into(),
            field_type: "string".into(),
            required: false,
            filled_by: Default::default(),
            item_keys: Vec::new(),
            covers: None,
            binds: None,
            item_value_max_bytes: None,
            item_one_of: Vec::new(),
            writer: writer.map(str::to_string),
        }
    }

    fn runner(host: Option<&str>) -> CredentialedCaller {
        CredentialedCaller {
            principal: "runner:ops".into(),
            actor_id: "automation:ops-runner".into(),
            host: host.map(str::to_string),
        }
    }

    #[test]
    fn only_a_declared_key_whose_value_changes_is_reserved() {
        let fields = [field("plan", Some("runner:ops")), field("comment", None)];
        let old = json!({ "plan": "PLAN a", "comment": "x" });
        // Unchanged plan, changed comment: nothing reserved.
        assert!(
            reserved_keys_changed(&fields, &old, &json!({ "plan": "PLAN a", "comment": "y" }))
                .is_empty()
        );
        // Changed, added and removed are each a change.
        for new in [json!({ "plan": "PLAN wipe" }), json!({ "comment": "x" })] {
            assert_eq!(
                reserved_keys_changed(&fields, &old, &new),
                vec![ReservedKey {
                    key: "plan".into(),
                    writer: "runner:ops".into()
                }]
            );
        }
        assert_eq!(
            reserved_keys_changed(&fields, &json!({}), &json!({ "plan": "p" })).len(),
            1
        );
    }

    #[test]
    fn no_credential_never_satisfies_a_declared_writer() {
        let why = admits(None, "runner:ops", Some("forge")).unwrap_err();
        assert!(why.contains("self-asserted"), "{why}");
    }

    #[test]
    fn a_credential_for_another_principal_is_refused() {
        let other = CredentialedCaller {
            principal: "runner:other".into(),
            ..runner(None)
        };
        assert!(admits(Some(&other), "runner:ops", Some("forge")).is_err());
    }

    #[test]
    fn a_host_bound_credential_writes_only_its_own_hosts_packets() {
        assert_eq!(
            admits(Some(&runner(Some("forge"))), "runner:ops", Some("forge")),
            Ok(())
        );
        assert!(admits(Some(&runner(Some("forge"))), "runner:ops", Some("boss-gcp")).is_err());
        assert!(admits(Some(&runner(Some("forge"))), "runner:ops", None).is_err());
        assert_eq!(
            admits(Some(&runner(None)), "runner:ops", Some("forge")),
            Ok(())
        );
    }
}
