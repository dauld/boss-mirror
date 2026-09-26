//! A Secret the credential broker is declared to fill is CREATED, EMPTY,
//! by the converge when it is absent — never a value, never a touch on
//! one that exists. The lib functions in
//! `infra/forge/cluster-deploy-lib.sh` are RUN against a stub `kubectl`
//! and a fixture rules directory, so every verdict below is one they
//! actually reached.
//!
//! WHY (backlog 51c98681, 2026-09-16). The broker's install phase
//! PATCHes a value into its Secret and is deliberately not granted
//! `create` (it cannot be name-scoped in RBAC —
//! boss-credential-broker.yaml). So the empty Secret had to be
//! "pre-created out-of-band, once": a hand act at the head of every
//! machine rotation. For the forge token it was done by hand in
//! September; for the tunnel credential nothing had done it, the
//! connector waited in ContainerCreating, and every converge recorded
//! `cloudflared: skipped (secret absent: cloudflare-tunnel-credentials)`
//! forever. David: no hand work unless absolutely required; an empty
//! object declared in the registry is not a credential.
//!
//! WHICH SECRETS. The declaration is the broker's own: each
//! `credential.rotate.*` rule under infra/dispatcher/rules/ carries
//! `secret_namespace` / `secret_name` as handler args, and those args
//! are exactly what the handler PATCHes (credential_issuer.rs
//! `write_key`). Reading them means the converge creates the Secret the
//! broker will write, by construction — a second copy on the registry
//! row would be the §9a pair that drifts. The runner holds the admin
//! credential and the converged tree, so no read of the system of
//! record is needed to know what to create (CLAUDE.md §Diagnosis: an
//! arm that needs the patient is not an arm).
//!
//! What each case pins: the Secrets are derived from the rules (a
//! non-broker rule with the same args is not one; a broker rule whose
//! Secret cannot be read is a named refusal); an absent Secret is
//! created with NO data flags; a present one is not touched; a read the
//! credential cannot make refuses by name and creates nothing; a create
//! that fails is named, not hidden; the line rides the converge packet
//! as `secrets_declared`, after the roll and before the connector is
//! read; and ONE helper answers "does this Secret exist" for every loop
//! in the lib (§9a — two copies of that loop were what the previous car
//! left).

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::PathBuf;
use std::process::Command;

const LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const RULES: &str = "infra/dispatcher/rules";

/// A fixture: a rules directory shaped like the tree's (two broker
/// rules, one ordinary rule that also names a Secret, one broker rule
/// whose Secret is not a literal), and a stub kubectl that answers `get
/// secret` from a list of present names and logs every call.
struct Case {
    root: PathBuf,
    kubectl: PathBuf,
    rules: PathBuf,
    present: PathBuf,
}

const BROKER_RULE: &str = r#"[[rule]]
name = "broker-rotates-x"
why = "fixture"
on_event = "step.done.credential-rotation"
when = 'subject_id = "x"'
[[rule.do]]
handler = "credential.rotate.forgejo"
args = { forge_user = "\"david\"", secret_namespace = "\"boss-dev\"", secret_name = "\"forge-token-fixture\"", secret_key = "\"token\"" }
"#;

const TUNNEL_RULE: &str = r#"[[rule]]
name = "broker-rotates-y"
why = "fixture"
on_event = "step.done.credential-rotation"
when = 'subject_id = "y"'
[[rule.do]]
handler = "credential.rotate.cloudflare-tunnel"
args = { zone = "\"example.test\"", secret_namespace = "\"boss\"", secret_name = "\"tunnel-creds-fixture\"", secret_key = "\"credentials.json\"", restart_deployment = "\"cloudflared\"" }
"#;

/// Not the broker: a spawn rule that happens to carry the same arg
/// names. The converge must not create a Secret for it.
const OTHER_RULE: &str = r#"[[rule]]
name = "something-else"
why = "fixture"
on_event = "step.done.task"
[[rule.do]]
handler = "jobs.spawn"
args = { secret_namespace = "\"boss\"", secret_name = "\"not-a-broker-secret\"" }
"#;

/// A broker rule whose Secret name is an EXPRESSION, not a literal:
/// the converge cannot know what to create and must say so.
const UNREADABLE_RULE: &str = r#"[[rule]]
name = "broker-rotates-z"
why = "fixture"
on_event = "step.done.credential-rotation"
[[rule.do]]
handler = "credential.rotate.forgejo"
args = { secret_namespace = "\"boss\"", secret_name = "metadata.secret_name", secret_key = "\"token\"" }
"#;

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("declared-secrets-{name}"));
        let rules = root.join("rules");
        std::fs::create_dir_all(&rules).unwrap();
        write_file(&rules.join("broker-rotates-x.toml"), BROKER_RULE);
        write_file(&rules.join("broker-rotates-y.toml"), TUNNEL_RULE);
        write_file(&rules.join("something-else.toml"), OTHER_RULE);
        let present = root.join("present");
        write_file(&present, "");
        let kubectl = root.join("kubectl");
        write_exec(
            &kubectl,
            r#"#!/usr/bin/env bash
echo "kubectl $*" >> "$STUB_LOG"
all="$*"
last="${all##* }"
case "$all" in
  *"get secret -n "*)
    ns=$(printf '%s\n' "$@" | grep -A1 -x -- -n | tail -n1)
    name="$last"
    if [ -n "${STUB_FORBIDDEN:-}" ] && [ "$name" = "$STUB_FORBIDDEN" ]; then
      echo "Error from server (Forbidden): secrets \"$name\" is forbidden: User cannot get resource \"secrets\" in namespace \"$ns\"" >&2; exit 1
    fi
    if grep -qx -- "$name" "$STUB_PRESENT"; then echo "NAME  TYPE  DATA"; exit 0; fi
    echo "Error from server (NotFound): secrets \"$name\" not found" >&2; exit 1 ;;
  *"create secret generic "*)
    if [ -n "${STUB_CREATE_FAILS:-}" ]; then
      echo "Error from server (NotFound): namespaces \"$STUB_CREATE_FAILS\" not found" >&2; exit 1
    fi
    echo "secret/$last created"; exit 0 ;;
esac
exit 0
"#,
        );
        Self {
            root,
            kubectl,
            rules,
            present,
        }
    }

    fn present(&self, names: &[&str]) {
        write_file(&self.present, &format!("{}\n", names.join("\n")));
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.root.join("calls")).unwrap_or_default()
    }

    /// Source the lib and run one function with the stub as its kubectl.
    fn run(&self, body: &str, env: &[(&str, &str)]) -> (i32, String, String) {
        let script = format!(". '{}'\n{}\n", repo_root().join(LIB).display(), body);
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&script)
            .env("STUB_LOG", self.root.join("calls"))
            .env("STUB_PRESENT", &self.present)
            .env("K", &self.kubectl)
            .env("R", &self.rules);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }
}

#[test]
fn the_secrets_are_derived_from_the_broker_rules_and_nothing_else() {
    let c = Case::new("derive");
    let (rc, out, err) = c.run(r#"broker_secrets "$R""#, &[]);
    assert_eq!(rc, 0, "{err}");
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        vec![
            "boss\ttunnel-creds-fixture",
            "boss-dev\tforge-token-fixture"
        ],
        "ns<TAB>name for each credential.rotate.* rule, sorted; the spawn rule \
         carrying the same arg names is not the broker's and is not listed"
    );

    // The tree's own rules: the two Secrets the packet names, read from
    // the declaration the broker itself PATCHes — the measurement the
    // car is built on, pinned so a renamed arg cannot silently empty
    // the list.
    let (rc, out, err) = c.run(
        &format!(r#"broker_secrets "{}""#, repo_root().join(RULES).display()),
        &[],
    );
    assert_eq!(rc, 0, "{err}");
    let got: Vec<&str> = out.lines().collect();
    assert!(
        got.contains(&"boss\tcloudflare-tunnel-credentials"),
        "broker-rotates-the-cloudflare-tunnel declares boss/cloudflare-tunnel-credentials: {got:?}"
    );
    assert!(
        got.contains(&"boss-dev\tboss-dev-forge-token"),
        "broker-rotates-the-boss-dev-forge-token declares boss-dev/boss-dev-forge-token: {got:?}"
    );
    assert!(
        got.contains(&"boss\tforge-host-checkout-token"),
        "broker-rotates-the-forge-host-checkout-token declares boss/forge-host-checkout-token \
         (design 1c90d183, D5), once — its delivery rule names the same Secret: {got:?}"
    );
    for line in &got {
        let (ns, name) = line.split_once('\t').expect("ns<TAB>name");
        assert!(
            !ns.is_empty() && !name.is_empty() && !line.contains('"'),
            "a bare namespace and name, never TOML quoting: {line:?}"
        );
    }
}

/// Every Secret a broker rule declares has the broker's grant on it, by
/// NAME: a Role in that namespace whose one Secret rule is
/// `resourceNames: [<name>]`, `verbs: [get, patch]` — `get` for the
/// idempotence read, `patch` for the install, nothing else — bound to
/// the boss pod's service account. The rule declares the Secret and the
/// manifest grants it; that is one fact in two files (CLAUDE.md §9a),
/// and a rule without its grant would mint a token and then fail the
/// install with 403, the token orphaned at the issuer (design 1c90d183
/// added the third such Secret, boss/forge-host-checkout-token).
#[test]
fn every_declared_broker_secret_is_granted_to_the_broker_by_name() {
    let c = Case::new("granted");
    let (rc, out, err) = c.run(
        &format!(r#"broker_secrets "{}""#, repo_root().join(RULES).display()),
        &[],
    );
    assert_eq!(rc, 0, "{err}");
    let manifest = std::fs::read_to_string(
        repo_root().join("infra/cluster/manifests/boss-credential-broker.yaml"),
    )
    .expect("the broker manifest");
    let docs: Vec<&str> = manifest.split("\n---").collect();
    for line in out.lines() {
        let (ns, name) = line.split_once('\t').expect("ns<TAB>name");
        let role = docs.iter().find(|d| {
            d.contains("\nkind: Role\n")
                && d.contains(&format!("  namespace: {ns}\n"))
                && d.contains(&format!("resourceNames: [{name}]"))
        });
        let role = role
            .unwrap_or_else(|| panic!("no Role in {ns} grants the broker Secret {name} by name"));
        let secret_rule = role
            .split("  - apiGroups:")
            .find(|r| r.contains("resources: [secrets]") && r.contains(name))
            .expect("the Role's secrets rule");
        assert!(
            secret_rule.contains("verbs: [get, patch]"),
            "{ns}/{name}: the broker gets and patches, nothing more:\n{secret_rule}"
        );
        let role_name = role
            .lines()
            .find_map(|l| l.strip_prefix("  name: "))
            .expect("the Role's name");
        assert!(
            docs.iter().any(|d| d.contains("\nkind: RoleBinding\n")
                && d.contains(&format!("  namespace: {ns}\n"))
                && d.contains(&format!("  name: {role_name}\n"))
                && d.contains("    name: default\n    namespace: boss")),
            "{ns}/{name}: Role {role_name} is bound to the boss pod's service account"
        );
    }
}

#[test]
fn a_broker_rule_whose_secret_is_not_a_literal_is_a_named_refusal() {
    let c = Case::new("unreadable");
    write_file(&c.rules.join("broker-rotates-z.toml"), UNREADABLE_RULE);
    let (rc, out, err) = c.run(r#"broker_secrets "$R""#, &[]);
    assert_eq!(
        rc, 1,
        "a declaration the converge cannot read is refused, not skipped: {err}"
    );
    assert!(
        err.contains("broker-rotates-z.toml") && err.contains("credential.rotate.forgejo"),
        "the refusal names the file and the handler: {err}"
    );
    assert!(
        out.contains("boss\ttunnel-creds-fixture"),
        "the readable declarations are still printed, so the caller can say which were: {out}"
    );
}

#[test]
fn an_absent_declared_secret_is_created_empty_and_a_present_one_is_untouched() {
    let c = Case::new("create");
    c.present(&["forge-token-fixture"]);
    let (rc, out, err) = c.run(r#"ensure_declared_secrets "$K" "$R""#, &[]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        out.trim(),
        "created boss/tunnel-creds-fixture | present boss-dev/forge-token-fixture",
        "the packet line: what was created and what already existed, by ns/name"
    );
    let calls = c.calls();
    let creates: Vec<&str> = calls
        .lines()
        .filter(|l| l.contains("create secret"))
        .collect();
    assert_eq!(
        creates,
        vec!["kubectl -n boss create secret generic tunnel-creds-fixture"],
        "exactly one create, for the absent one, with NO data — no --from-literal, \
         no --from-file, no --from-env-file: {calls}"
    );
    assert!(
        !calls.contains("forge-token-fixture")
            || !calls.contains("create secret generic forge-token-fixture"),
        "a Secret that exists is never touched: {calls}"
    );
    assert!(
        !calls.contains("patch") && !calls.contains("delete") && !calls.contains("apply"),
        "create is the only write the converge makes to a Secret: {calls}"
    );

    // Second converge: both present, nothing created, the line says so.
    let c = Case::new("idempotent");
    c.present(&["forge-token-fixture", "tunnel-creds-fixture"]);
    let (rc, out, err) = c.run(r#"ensure_declared_secrets "$K" "$R""#, &[]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        out.trim(),
        "present boss/tunnel-creds-fixture, boss-dev/forge-token-fixture"
    );
    assert!(!c.calls().contains("create secret"), "{}", c.calls());
}

#[test]
fn a_read_the_credential_cannot_make_is_a_refusal_by_name_and_creates_nothing() {
    let c = Case::new("forbidden");
    c.present(&["forge-token-fixture"]);
    let (rc, out, err) = c.run(
        r#"ensure_declared_secrets "$K" "$R""#,
        &[("STUB_FORBIDDEN", "tunnel-creds-fixture")],
    );
    assert_eq!(
        rc, 0,
        "the line is the verdict; the converge is not held: {err}"
    );
    assert!(
        out.contains("cannot boss/tunnel-creds-fixture (")
            && out.to_lowercase().contains("forbidden"),
        "a Forbidden read is named on the packet with kubectl's reason, never read as absent: {out}"
    );
    assert!(
        out.contains("present boss-dev/forge-token-fixture"),
        "the other Secret's answer still rides the line: {out}"
    );
    assert!(
        !c.calls().contains("create secret"),
        "nothing is created on a read the credential could not make: {}",
        c.calls()
    );

    // A create that fails (the namespace does not exist yet) is named
    // the same way — the failure is on the packet, not only in the
    // journal.
    let c = Case::new("create-fails");
    c.present(&["forge-token-fixture"]);
    let (rc, out, _) = c.run(
        r#"ensure_declared_secrets "$K" "$R""#,
        &[("STUB_CREATE_FAILS", "boss")],
    );
    assert_eq!(rc, 0);
    assert!(
        out.contains("cannot boss/tunnel-creds-fixture (") && out.contains("not found"),
        "{out}"
    );

    // A rules directory the lib cannot fully read: the readable Secrets
    // are still ensured, and the line says a declaration was refused.
    let c = Case::new("partly-unreadable");
    write_file(&c.rules.join("broker-rotates-z.toml"), UNREADABLE_RULE);
    c.present(&[]);
    let (rc, out, err) = c.run(r#"ensure_declared_secrets "$K" "$R""#, &[]);
    assert_eq!(rc, 0, "{err}");
    assert!(
        out.contains("created boss/tunnel-creds-fixture, boss-dev/forge-token-fixture")
            && out.contains("unreadable declaration"),
        "{out}"
    );
}

#[test]
fn one_helper_answers_whether_a_secret_exists() {
    // §9a. instance_secret_gate and connector_status each carried their
    // own `get secret` + "not found" loop; a third would have made
    // three. One helper, and this count is how it stays one.
    let lib = std::fs::read_to_string(repo_root().join(LIB)).unwrap();
    // Code lines only: prose may name the commands as often as it likes.
    let code = |needle: &str| {
        lib.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter(|l| l.contains(needle))
            .count()
    };
    let reads = code("get secret -n");
    assert_eq!(
        reads, 1,
        "exactly one `kubectl get secret -n` in the lib — secret_presence — \
         every loop asks it; found {reads}"
    );
    assert_eq!(
        code("create secret generic"),
        // ensure_declared_secrets' one create, plus the two shapes
        // instance_secret_gate PRINTS for the operator (never runs).
        3,
        "the converge creates a Secret in exactly one place"
    );
    // The gate's presence loop is instance_secrets_absent since
    // dc1bc724 (the quiet half the runner provisions from); the gate
    // itself reads through it.
    for f in [
        "instance_secrets_absent",
        "connector_status",
        "ensure_declared_secrets",
    ] {
        let start = lib
            .find(&format!("{f}() {{"))
            .unwrap_or_else(|| panic!("{f} is defined"));
        let body = &lib[start..];
        let end = body.find("\n}\n").unwrap();
        assert!(
            body[..end].contains("secret_presence"),
            "{f} asks secret_presence rather than reading kubectl itself"
        );
    }
    let start = lib.find("instance_secret_gate() {").unwrap();
    let body = &lib[start..];
    let end = body.find("\n}\n").unwrap();
    assert!(
        body[..end].contains("instance_secrets_absent") && !body[..end].contains("$k get secret"),
        "instance_secret_gate reads presence through instance_secrets_absent"
    );
}

#[test]
fn the_runner_ensures_the_declared_secrets_after_the_roll_and_before_the_connector_is_read() {
    let src = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    let stamped = src
        .find("OUTCOME=\"converged=$HEAD\"")
        .expect("the runner marks the roll real");
    let after = &src[stamped..];
    let ensured = after
        .find("$(ensure_declared_secrets ")
        .expect("the runner ensures the declared Secrets after the roll");
    // The read is observe_connector's since 0b7804f3 (both ticks record
    // through it); its position after the Secrets exist is what matters.
    let connector = after
        .find("observe_connector \"$K\"")
        .expect("the runner reads the connector");
    assert!(
        ensured < connector,
        "the Secrets exist before the connector is read, so the field it records \
         is about the connector, not about a ceremony"
    );
    assert!(
        after[ensured..connector].contains("run_summary_field secrets_declared"),
        "the line rides the converge packet as `secrets_declared`, the field the probe reads"
    );
    assert!(
        after[ensured..ensured + 200].contains("infra/dispatcher/rules"),
        "the declaration read is the tree's broker rules"
    );
}
