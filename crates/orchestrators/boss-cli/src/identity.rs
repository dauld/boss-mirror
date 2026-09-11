//! Who a CLI call is signed as — the one definition of the CLI's
//! request identity.
//!
//! WHY THIS EXISTS (backlog 5083d6f5). The `completed_by` column
//! landed on 2026-09-08 and, on its first read, said the wrong thing:
//! the `Proven in prod` step of car 3c1f843f — completed by an
//! operator session running `boss prove` — carried
//! `completed_by = automation:train-conductor` while its assignee was
//! that operator. The column was right about what it measures (the
//! actor the write was SIGNED with); the CLI was wrong about what it
//! sent. Every jobs-API call any verb made carried the conductor's
//! automation identity, because the conductor was the first thing this
//! crate grew and the header it needed became "the" header.
//!
//! So an operator's act — a proof, a gate launch, a packet edit —
//! landed in the audit log and on the step as work the train
//! automation did. That is the provenance property (CLAUDE.md
//! §Founding ideas) failing at the first hop: the log holds what
//! happened, but not who did it.
//!
//! THE RULE. The CLI signs as the actor RUNNING it. The conductor
//! keeps signing as the conductor where the conductor IS the actor —
//! its own `boss train …` verbs, fired by its unit — and nowhere
//! else.
//!
//! HOW IT LEARNS WHO THAT IS, in order:
//!
//! 1. `BOSS_ACTOR` — the env var a session, a unit, or a wrapper
//!    sets. Explicit, and the one an automation sets to say so.
//! 2. `$BOSS_ACTOR_FILE`, else `$HOME/.config/boss/actor` — one line,
//!    the id. The per-machine answer for a workstation or a
//!    long-lived pod, set once instead of exported per shell.
//! 3. Nothing. Then the WRONG case must be loud: a wrong actor
//!    answers instead of erroring (CLAUDE.md §Doors), and signing as
//!    automation is exactly the wrong answer this car exists to stop.
//!    A WRITE is refused, naming both ways to fix it. A READ — which
//!    attributes nothing — proceeds under [`UNIDENTIFIED`], never
//!    under an automation slug, and says so once on stderr.
//!
//! The read/write split is deliberate. Refusing reads would stop
//! `boss orient`, `boss receipt` and every `--wait` poll on a
//! misconfigured box, for no provenance gained: a GET writes no
//! actor anywhere. Refusing WRITES stops exactly the calls whose
//! attribution was wrong.

use std::path::PathBuf;

use anyhow::{Result, bail};
use serde_json::json;

/// The env var that names the caller.
pub(crate) const ACTOR_ENV: &str = "BOSS_ACTOR";

/// Override for the actor file's location; `$HOME/.config/boss/actor`
/// when unset. Exists so a test can point at a scratch file without
/// touching `$HOME`.
pub(crate) const ACTOR_FILE_ENV: &str = "BOSS_ACTOR_FILE";

/// The conductor's own identity — the id its verbs sign with when
/// nothing overrides it, and the value its unit sets explicitly.
/// One definition, aliased by `train::ACTOR` (CLAUDE.md §9a).
pub(crate) const CONDUCTOR: &str = "automation:train-conductor";

/// The id a READ carries when nothing named the caller. Deliberately
/// not an `automation:` slug: an unidentified operator is not the
/// train, and the server's automation branch (`http/steps.rs`) must
/// not read it as one.
pub(crate) const UNIDENTIFIED: &str = "operator:unidentified";

/// Where a resolved id came from, so a message can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Source {
    Env,
    File(PathBuf),
}

/// A resolved caller: the id, and where it was learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Caller {
    pub(crate) id: String,
    pub(crate) source: Source,
}

/// The pure core of resolution: env wins, then the file, then
/// nothing. Blank and whitespace-only values are NOT answers — an
/// `export BOSS_ACTOR=` or an empty file is a misconfiguration, and
/// treating it as an id would sign every write as the empty string.
pub(crate) fn resolve_from(env: Option<String>, file: Option<(PathBuf, String)>) -> Option<Caller> {
    let from_env = env
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(|id| Caller {
            id,
            source: Source::Env,
        });
    from_env.or_else(|| {
        file.and_then(|(path, body)| {
            let id = body.trim().to_string();
            (!id.is_empty()).then_some(Caller {
                id,
                source: Source::File(path),
            })
        })
    })
}

/// Where the actor file lives. `None` when neither the override nor a
/// home directory can be determined — a container with no `$HOME` is
/// not an error here, it just has one fewer source.
fn actor_file_path() -> Option<PathBuf> {
    match std::env::var(ACTOR_FILE_ENV) {
        Ok(v) if !v.trim().is_empty() => Some(PathBuf::from(v.trim())),
        _ => dirs::home_dir().map(|h| h.join(".config").join("boss").join("actor")),
    }
}

/// The caller, from the environment of this process. An unreadable
/// file is the same as an absent one: it names nobody either way, and
/// the refusal below already says how to name somebody.
pub(crate) fn caller() -> Option<Caller> {
    let file =
        actor_file_path().and_then(|p| std::fs::read_to_string(&p).ok().map(|body| (p, body)));
    resolve_from(std::env::var(ACTOR_ENV).ok(), file)
}

/// What a refused write says. A refusal that does not say how to fix
/// it is a wall; this names both doors and the file's location on
/// THIS machine.
pub(crate) fn refusal(method: &str, path: &str) -> String {
    let file = actor_file_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "$HOME/.config/boss/actor".to_string());
    format!(
        "refusing to sign {method} {path}: nothing names the actor running this command, \
         and signing it as `{CONDUCTOR}` would credit the train automation with your act \
         (backlog 5083d6f5). Name yourself with `export {ACTOR_ENV}=<your id>` or by writing \
         that id into {file}."
    )
}

/// Is this a write — a call whose actor is recorded?
pub(crate) fn is_write(method: &reqwest::Method) -> bool {
    !matches!(
        *method,
        reqwest::Method::GET | reqwest::Method::HEAD | reqwest::Method::OPTIONS
    )
}

/// What one call gets signed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Signature {
    /// Somebody is named: the call goes out as them.
    As(String),
    /// A read nobody named. It goes out marked, not as automation.
    Unidentified,
    /// A write nobody named. It does not go out at all.
    Refused(String),
}

/// The decision, as a pure function of the method, the path and who
/// (if anyone) is named. The whole rule of this module is these three
/// arms; everything around it is plumbing that supplies the caller.
pub(crate) fn signature_for(
    method: &reqwest::Method,
    path: &str,
    caller: Option<Caller>,
) -> Signature {
    match (caller, is_write(method)) {
        (Some(c), _) => Signature::As(c.id),
        (None, false) => Signature::Unidentified,
        (None, true) => Signature::Refused(refusal(method.as_str(), path)),
    }
}

/// A [`Signature`] resolved to the id that goes on the wire — or the
/// refusal. The one impure step (it can print), kept separate from
/// the decision so a caller can test the decision without a process.
pub(crate) fn apply(signature: Signature) -> Result<String> {
    match signature {
        Signature::As(id) => Ok(id),
        Signature::Unidentified => {
            warn_unidentified();
            Ok(UNIDENTIFIED.to_string())
        }
        Signature::Refused(msg) => bail!("{msg}"),
    }
}

/// [`signature_for`], applied to this process's environment.
pub(crate) fn sign(method: &reqwest::Method, path: &str) -> Result<String> {
    apply(signature_for(method, path, caller()))
}

/// The id to sign a READ with — [`sign`] with the arm that cannot
/// fail already taken. For the handful of read verbs that build their
/// own request instead of going through `gate::api`.
pub(crate) fn reader() -> String {
    match signature_for(&reqwest::Method::GET, "", caller()) {
        Signature::As(id) => id,
        _ => {
            warn_unidentified();
            UNIDENTIFIED.to_string()
        }
    }
}

/// Said ONCE per process, so a `--wait` poll does not scroll the same
/// line a hundred times.
fn warn_unidentified() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        eprintln!(
            "boss: nothing names the actor running this command — reading as \
             `{UNIDENTIFIED}`. Writes will be refused until `{ACTOR_ENV}` (or the \
             actor file) names you."
        );
    });
}

/// The id the CONDUCTOR's own verbs sign with: whatever names the
/// caller if anything does — its unit sets `BOSS_ACTOR` explicitly,
/// and a human running `boss train cancel` by hand should sign as
/// themselves — and the conductor otherwise. This one never refuses:
/// a supervised loop that stops because its environment forgot to
/// name it is a worse failure than a correctly-attributed automation
/// write.
pub(crate) fn conductor() -> String {
    conductor_from(caller())
}

/// The pure core of [`conductor`].
pub(crate) fn conductor_from(caller: Option<Caller>) -> String {
    caller.map_or_else(|| CONDUCTOR.to_string(), |c| c.id)
}

/// The `x-boss-user` header for an id — the ONE place the header's
/// shape is written. Role and tier are what the policy layer reads;
/// the id is what provenance reads. Both matter: a read under a role
/// the policy does not grant comes back as an EMPTY collection rather
/// than an error (memory: empty API reads mean wrong actor).
pub(crate) fn header(id: &str) -> String {
    json!({
        "id": id,
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_env_var_names_the_caller() {
        let got = resolve_from(Some("claude@algedonic.dev".into()), None);
        assert_eq!(
            got,
            Some(Caller {
                id: "claude@algedonic.dev".into(),
                source: Source::Env
            })
        );
    }

    #[test]
    fn the_file_answers_when_the_env_does_not() {
        let got = resolve_from(
            None,
            Some((
                PathBuf::from("/home/x/.config/boss/actor"),
                "emp-david\n".into(),
            )),
        );
        assert_eq!(
            got,
            Some(Caller {
                id: "emp-david".into(),
                source: Source::File(PathBuf::from("/home/x/.config/boss/actor"))
            })
        );
    }

    #[test]
    fn the_env_var_beats_the_file() {
        let got = resolve_from(
            Some("emp-david".into()),
            Some((PathBuf::from("/f"), "someone-else".into())),
        );
        assert_eq!(got.map(|c| c.id), Some("emp-david".to_string()));
    }

    #[test]
    fn blank_is_not_an_answer() {
        // An `export BOSS_ACTOR=` and an empty actor file are
        // misconfigurations, not the empty-string actor.
        assert_eq!(resolve_from(Some("   ".into()), None), None);
        assert_eq!(
            resolve_from(None, Some((PathBuf::from("/f"), "\n".into()))),
            None
        );
        // ...and a blank env falls THROUGH to the file rather than
        // shadowing it.
        assert_eq!(
            resolve_from(
                Some("".into()),
                Some((PathBuf::from("/f"), "emp-david".into()))
            )
            .map(|c| c.id),
            Some("emp-david".to_string())
        );
    }

    #[test]
    fn an_unresolved_write_is_refused_and_says_how_to_fix_it() {
        // The whole point of the car: unresolved must never mean
        // "sign as the conductor".
        let msg = refusal("PUT", "/api/jobs/x/steps/y");
        assert!(
            msg.contains("refusing to sign PUT /api/jobs/x/steps/y"),
            "{msg}"
        );
        assert!(msg.contains(ACTOR_ENV), "{msg}");
        // It names the identity it REFUSED to use, so the reader can
        // tell this apart from a policy denial.
        assert!(msg.contains(CONDUCTOR), "{msg}");
    }

    #[test]
    fn an_unidentified_reader_is_not_automation() {
        // `http/steps.rs` reads an `automation:` prefix as a process
        // acting; the unidentified operator must not trip that.
        assert!(!UNIDENTIFIED.starts_with("automation:"));
        assert!(!UNIDENTIFIED.starts_with("rule:"));
        assert!(!UNIDENTIFIED.ends_with("-sim"));
        assert!(!UNIDENTIFIED.ends_with("-runner"));
    }

    #[test]
    fn writes_are_the_methods_that_record_an_actor() {
        for m in [
            reqwest::Method::POST,
            reqwest::Method::PUT,
            reqwest::Method::PATCH,
            reqwest::Method::DELETE,
        ] {
            assert!(is_write(&m), "{m} records an actor");
        }
        for m in [
            reqwest::Method::GET,
            reqwest::Method::HEAD,
            reqwest::Method::OPTIONS,
        ] {
            assert!(!is_write(&m), "{m} attributes nothing");
        }
    }

    fn named(id: &str) -> Option<Caller> {
        Some(Caller {
            id: id.into(),
            source: Source::Env,
        })
    }

    #[test]
    fn a_write_signs_the_caller() {
        // The claim of backlog 5083d6f5, at the decision point: the
        // step PUT a `boss prove` makes must carry the operator, not
        // the conductor.
        assert_eq!(
            signature_for(
                &reqwest::Method::PUT,
                "/api/jobs/x/steps/y",
                named("claude@algedonic.dev")
            ),
            Signature::As("claude@algedonic.dev".into())
        );
    }

    #[test]
    fn an_unnamed_write_is_refused_rather_than_signed_as_automation() {
        let sig = signature_for(&reqwest::Method::POST, "/api/jobs", None);
        match sig {
            Signature::Refused(msg) => assert!(msg.contains(ACTOR_ENV), "{msg}"),
            other => panic!("an unnamed write must not go out at all, got {other:?}"),
        }
    }

    #[test]
    fn an_unnamed_read_is_marked_not_attributed() {
        assert_eq!(
            signature_for(&reqwest::Method::GET, "/api/jobs", None),
            Signature::Unidentified
        );
    }

    #[test]
    fn the_conductor_still_signs_as_automation() {
        // Its unit names it explicitly; with nothing named at all it
        // is still the conductor, because the conductor's loop IS the
        // actor there. Never a refusal: a supervised loop must not
        // stop over its own attribution.
        assert_eq!(conductor_from(None), CONDUCTOR);
        assert_eq!(conductor_from(named(CONDUCTOR)), CONDUCTOR);
        // ...and a human at the same verb signs as themselves.
        assert_eq!(conductor_from(named("emp-david")), "emp-david");
    }

    #[test]
    fn the_header_carries_the_id_and_the_scope_the_policy_reads() {
        let v: serde_json::Value = serde_json::from_str(&header("emp-david")).unwrap();
        assert_eq!(v["id"], "emp-david");
        assert_eq!(v["role"], "platform-admin");
        assert_eq!(v["access_tier"], "operator");
    }
}
