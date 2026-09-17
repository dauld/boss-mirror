//! Provisioning an instance MINTS the instance's own Secrets: the
//! converge creates what nobody needs to know, copies what the instance
//! shares with its source, creates the one root it cannot mint EMPTY
//! (the gateway runs guest-only without it), and leaves by name only
//! what is genuinely a person's. The lib functions in
//! `infra/forge/cluster-deploy-lib.sh` are RUN against a stub `kubectl`
//! and fixture manifests, so every verdict below is one they reached.
//!
//! WHY (backlog dc1bc724, David 2026-09-16: no hand work unless
//! absolutely required). The playground car (07d7549c) landed a secret
//! gate that SKIPS an instance until six Secrets exist in its namespace,
//! and as filed David minted all six by hand — so the playground was
//! skipped on every converge from 2026-09-15 for want of a ceremony.
//! Measured against the rendered playground on 2026-09-16 (six names,
//! five keys), the six classify as:
//!
//!   internal  boss-secrets (postgres-password, admin-password, and
//!             database-url composed from them), boss-session-key —
//!             random bytes NOBODY needs to know; minted here
//!   shared    forgejo-registry (the registry pull credential), resend
//!             (the mail sender) — the SAME credential as the source
//!             instance's; copied from it when instances.toml declares
//!             `shares_with`
//!   root      boss-oidc (client-secret) — the Kanidm OIDC client the
//!             identity provider issues. MEASURED in boss-gateway
//!             oidc.rs `OidcConfig::from_env`: an EMPTY client secret
//!             reads as "no OIDC" and the gateway boots with guest
//!             sessions (BOSS_GUEST_ACCESS) and local auth; an ABSENT
//!             Secret wedges the pod in CreateContainerConfigError (the
//!             ref is not `optional`). So the object is created with the
//!             key present and empty, the way ensure_declared_secrets
//!             creates a broker Secret empty — and a ceremony fills it
//!   by hand   anything no recipe knows — the converge mints nothing
//!             for it and the gate still names it. The measured one
//!             was boss-tls, the lego certificate the Caddy front
//!             mounted; its last reference left the tree on 2026-09-17
//!             (backlog 21c17ebc: Let's Encrypt left the cluster, TLS
//!             is the edge's), so the fixture below stands in with a
//!             name no manifest carries and the class keeps its test
//!
//! What each case pins: the classification; an absent internal Secret
//! is minted with every key the manifests read, from random bytes, the
//! URL composed from the password and the instance's OWN postgres
//! Service as the manifests declare it; a shared Secret is copied with
//! its type and data and none of the source's object identity; the root
//! is created empty; the by-hand one is not created; VALUES never reach
//! stdout, stderr or kubectl's argv (they ride stdin) — the runner's
//! journal carries names only; a present Secret is never touched; a
//! required key the recipe does not know refuses by name; and the
//! runner provisions between the quiet read and the loud gate, after
//! the instance's Namespace exists, recording `instance_secrets_minted`.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::PathBuf;
use std::process::Command;

const LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const RENDERER: &str = "infra/cluster/render-instance.sh";
const INSTANCES: &str = "infra/cluster/instances.toml";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

fn require_jq() {
    assert!(
        has("jq"),
        "jq is missing, and this suite would otherwise pass by skipping — \
         jq is declared in infra/forge/boss-ci/required-tools.txt"
    );
}

/// The source instance's copy of each shared Secret, as the stub hands
/// it back for `get secret NAME -n boss -o json`. Values are fixture
/// strings the assertions can search the output for.
const REGISTRY_JSON: &str = r#"{"apiVersion":"v1","kind":"Secret","type":"kubernetes.io/dockerconfigjson",
 "metadata":{"name":"forgejo-registry","namespace":"boss","uid":"11111111-uid","resourceVersion":"4242",
   "creationTimestamp":"2026-09-01T00:00:00Z","labels":{"boss":"registry"},
   "managedFields":[{"manager":"kubectl"}],
   "annotations":{"kubectl.kubernetes.io/last-applied-configuration":"{}"}},
 "data":{".dockerconfigjson":"UkVHSVNUUlktRE9DS0VSQ0ZHLVZBTFVF"}}"#;
const RESEND_JSON: &str = r#"{"apiVersion":"v1","kind":"Secret","type":"Opaque",
 "metadata":{"name":"resend","namespace":"boss","uid":"22222222-uid","resourceVersion":"99"},
 "data":{"api-key":"UkVTRU5ELUFQSS1LRVktVkFMVUU="}}"#;
/// What the two base64 fields above decode to.
const REGISTRY_VALUE: &str = "REGISTRY-DOCKERCFG-VALUE";
const RESEND_VALUE: &str = "RESEND-API-KEY-VALUE";

/// Rendered "manifests" as JSON (valid YAML; what `kubectl create
/// --dry-run=client -o json` echoes back — the idiom the secret gate's
/// own test uses), shaped like the playground's render: the postgres
/// StatefulSet the URL is composed from, the boss Deployment with every
/// reference form, and one workload mounting a Secret no recipe knows
/// (`something-new` — the by-hand class; boss-tls was the real one
/// until 21c17ebc).
fn manifests(dir: &std::path::Path, ns: &str) {
    write_file(
        &dir.join("boss.yaml"),
        &format!(
            r#"{{"kind":"StatefulSet","metadata":{{"name":"postgres","namespace":"{ns}"}},"spec":{{"serviceName":"postgres","template":{{"spec":{{
  "imagePullSecrets":[{{"name":"forgejo-registry"}}],
  "containers":[{{"name":"postgres","env":[
    {{"name":"POSTGRES_USER","value":"boss"}},{{"name":"POSTGRES_DB","value":"boss"}},
    {{"name":"POSTGRES_PASSWORD","valueFrom":{{"secretKeyRef":{{"name":"boss-secrets","key":"postgres-password"}}}}}}],
    "ports":[{{"containerPort":5432}}]}}]}}}}}}}}
{{"kind":"Deployment","metadata":{{"name":"boss","namespace":"{ns}"}},"spec":{{"template":{{"spec":{{
  "imagePullSecrets":[{{"name":"forgejo-registry"}}],
  "initContainers":[{{"name":"boss-init","env":[
    {{"name":"PGPASSWORD","valueFrom":{{"secretKeyRef":{{"name":"boss-secrets","key":"postgres-password"}}}}}},
    {{"name":"DATABASE_URL","valueFrom":{{"secretKeyRef":{{"name":"boss-secrets","key":"database-url"}}}}}},
    {{"name":"BOSS_BOOTSTRAP_ADMIN_PASSWORD","valueFrom":{{"secretKeyRef":{{"name":"boss-secrets","key":"admin-password"}}}}}}]}}],
  "containers":[{{"name":"boss","env":[
    {{"name":"BOSS_POSTGRES_URL","valueFrom":{{"secretKeyRef":{{"name":"boss-secrets","key":"database-url"}}}}}},
    {{"name":"BOSS_OIDC_CLIENT_SECRET","valueFrom":{{"secretKeyRef":{{"name":"boss-oidc","key":"client-secret"}}}}}},
    {{"name":"BOSS_MAIL_API_TOKEN","valueFrom":{{"secretKeyRef":{{"name":"resend","key":"api-key"}}}}}},
    {{"name":"BOSS_BROKER_FORGEJO_TOKEN","valueFrom":{{"secretKeyRef":{{"name":"boss-credential-broker-root","key":"forgejo-token","optional":true}}}}}}]}}],
  "volumes":[
    {{"name":"boss-session-key","secret":{{"secretName":"boss-session-key","items":[{{"key":"session.key","path":"session.key"}}]}}}}]}}}}}}}}
"#
        ),
    );
    write_file(
        &dir.join("boss-something.yaml"),
        &format!(
            r#"{{"kind":"Deployment","metadata":{{"name":"boss-something","namespace":"{ns}"}},"spec":{{"template":{{"spec":{{
  "containers":[{{"name":"something"}}],"volumes":[{{"name":"new","secret":{{"secretName":"something-new"}}}}]}}}}}}}}
"#
        ),
    );
}

struct Case {
    root: PathBuf,
    kubectl: PathBuf,
    manifests: PathBuf,
    present: PathBuf,
    created: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("instance-mints-{name}"));
        let manifests = root.join("manifests");
        std::fs::create_dir_all(&manifests).unwrap();
        manifests_into(&manifests);
        let present = root.join("present");
        write_file(&present, "");
        let created = root.join("created");
        std::fs::create_dir_all(&created).unwrap();
        let source = root.join("source").join("boss");
        std::fs::create_dir_all(&source).unwrap();
        write_file(&source.join("forgejo-registry.json"), REGISTRY_JSON);
        write_file(&source.join("resend.json"), RESEND_JSON);
        let kubectl = root.join("kubectl");
        // The stub logs every argv (the fixture's own record — the test
        // reads it to prove the values were NOT there), cats the
        // manifests for a dry run, answers presence from a list, hands
        // back the source's copy of a Secret, and files what `create -f -`
        // receives on stdin by the object's name.
        write_exec(
            &kubectl,
            r#"#!/usr/bin/env bash
echo "kubectl $*" >> "$STUB_LOG"
all="$*"
last="${all##* }"
case "$all" in
  *"create --dry-run=client -o json -f "*)
    for f in "$last"/*.yaml; do cat "$f"; done ;;
  "get secret "*" -o json")
    name="$3"
    ns=$(printf '%s\n' "$@" | grep -A1 -x -- -n | tail -n1)
    f="$STUB_SOURCE/$ns/$name.json"
    if [ -f "$f" ]; then cat "$f"; exit 0; fi
    echo "Error from server (NotFound): secrets \"$name\" not found" >&2; exit 1 ;;
  *"get secret -n "*)
    ns=$(printf '%s\n' "$@" | grep -A1 -x -- -n | tail -n1)
    name="$last"
    if [ -n "${STUB_FORBIDDEN:-}" ] && [ "$name" = "$STUB_FORBIDDEN" ]; then
      echo "Error from server (Forbidden): secrets \"$name\" is forbidden: User cannot get resource \"secrets\" in namespace \"$ns\"" >&2; exit 1
    fi
    if grep -qx -- "$name" "$STUB_PRESENT"; then echo "NAME  TYPE  DATA"; exit 0; fi
    echo "Error from server (NotFound): secrets \"$name\" not found" >&2; exit 1 ;;
  "create -f -")
    body=$(cat)
    name=$(printf '%s' "$body" | jq -r .metadata.name)
    if [ -n "${STUB_CREATE_FAILS:-}" ] && [ "$name" = "$STUB_CREATE_FAILS" ]; then
      echo "Error from server (NotFound): error when creating \"STDIN\": namespaces \"x\" not found" >&2; exit 1
    fi
    printf '%s' "$body" > "$STUB_CREATED/$name.json"
    echo "secret/$name created"; exit 0 ;;
esac
exit 0
"#,
        );
        Self {
            root,
            kubectl,
            manifests,
            present,
            created,
        }
    }

    fn present(&self, names: &[&str]) {
        write_file(&self.present, &format!("{}\n", names.join("\n")));
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.root.join("calls")).unwrap_or_default()
    }

    fn created_names(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&self.created)
            .unwrap()
            .map(|e| {
                e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .trim_end_matches(".json")
                    .to_string()
            })
            .collect();
        v.sort();
        v
    }

    fn created(&self, name: &str) -> serde_json::Value {
        let text = std::fs::read_to_string(self.created.join(format!("{name}.json")))
            .unwrap_or_else(|e| panic!("{name} was created: {e}"));
        serde_json::from_str(&text).unwrap()
    }

    /// A data field of a created Secret, base64-decoded (by jq — the
    /// tool the lib itself encodes with).
    fn field(&self, name: &str, key: &str) -> String {
        let out = Command::new("jq")
            .args(["-r", &format!(".data[\"{key}\"] | @base64d")])
            .arg(self.created.join(format!("{name}.json")))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    /// Source the lib and run one function with the stub as its kubectl.
    fn run(&self, body: &str, env: &[(&str, &str)]) -> (i32, String, String) {
        let script = format!(". '{}'\n{}\n", repo_root().join(LIB).display(), body);
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&script)
            .env("STUB_LOG", self.root.join("calls"))
            .env("STUB_PRESENT", &self.present)
            .env("STUB_SOURCE", self.root.join("source"))
            .env("STUB_CREATED", &self.created)
            .env("K", &self.kubectl)
            .env("M", &self.manifests);
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

fn manifests_into(dir: &std::path::Path) {
    manifests(dir, "boss-x");
}

const ALL_SIX: &str =
    "boss-oidc boss-secrets boss-session-key forgejo-registry resend something-new";

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[test]
fn the_six_secrets_the_playground_needs_are_classified() {
    let c = Case::new("classify");
    for (name, class) in [
        ("boss-secrets", "internal"),
        ("boss-session-key", "internal"),
        ("forgejo-registry", "shared"),
        ("resend", "shared"),
        ("boss-oidc", "root"),
        ("something-new", "by-hand"),
    ] {
        let (rc, out, err) = c.run(&format!("instance_secret_class {name}"), &[]);
        assert_eq!(rc, 0, "{err}");
        assert_eq!(out.trim(), class, "{name} is {class}");
    }
}

#[test]
fn an_absent_instance_is_provisioned_with_values_that_never_reach_the_log() {
    require_jq();
    let c = Case::new("provision");
    c.present(&[]);
    let (rc, out, err) = c.run(
        &format!(r#"provision_instance_secrets "$K" "$K" "$K" boss-x "$M" boss "{ALL_SIX}""#),
        &[],
    );
    assert_eq!(
        rc, 0,
        "the line is the verdict; never a failed converge: {err}"
    );
    assert_eq!(
        out.trim(),
        "minted boss-secrets, boss-session-key | copied forgejo-registry, resend from boss \
         | empty boss-oidc (guest sessions only until a Kanidm client exists — root ceremony) \
         | by hand something-new",
        "the packet line: every absent Secret classified and what was done for it"
    );
    assert_eq!(
        c.created_names(),
        vec![
            "boss-oidc",
            "boss-secrets",
            "boss-session-key",
            "forgejo-registry",
            "resend"
        ],
        "five created; something-new is a person's — no recipe knows it"
    );

    // INTERNAL: boss-secrets carries every key the manifests read, from
    // random bytes, and the URL is composed from the password and the
    // instance's OWN postgres Service — user, database, service name
    // and port read off the rendered StatefulSet, never assumed.
    let pg = c.field("boss-secrets", "postgres-password");
    let admin = c.field("boss-secrets", "admin-password");
    let url = c.field("boss-secrets", "database-url");
    assert!(
        is_hex(&pg) && pg.len() >= 32,
        "a random hex password: {pg:?}"
    );
    assert!(
        is_hex(&admin) && admin.len() >= 32,
        "a random hex admin password: {admin:?}"
    );
    assert_ne!(pg, admin, "two draws, not one value twice");
    assert_eq!(
        url,
        format!("postgres://boss:{pg}@postgres.boss-x.svc.cluster.local:5432/boss"),
        "the URL the boss pod and every chore read, pointing INTO the instance"
    );
    let secrets = c.created("boss-secrets");
    assert_eq!(secrets["metadata"]["namespace"], "boss-x");
    assert_eq!(secrets["type"], "Opaque");
    let session = c.field("boss-session-key", "session.key");
    assert!(
        is_hex(&session) && session.len() == 64,
        "32 random bytes as hex — the shape boss-gateway load_or_create_session_key reads \
         (hex, at least 32 bytes decoded): {session:?}"
    );

    // SHARED: the source's type and data, the instance's namespace,
    // and NONE of the source object's identity.
    let reg = c.created("forgejo-registry");
    assert_eq!(reg["type"], "kubernetes.io/dockerconfigjson");
    assert_eq!(reg["metadata"]["namespace"], "boss-x");
    assert_eq!(
        reg["metadata"]["labels"]["boss"], "registry",
        "labels ride along"
    );
    for gone in [
        "uid",
        "resourceVersion",
        "creationTimestamp",
        "managedFields",
        "annotations",
    ] {
        assert!(
            reg["metadata"].get(gone).is_none(),
            "metadata.{gone} is the source object's, not the copy's: {reg}"
        );
    }
    assert_eq!(
        c.field("forgejo-registry", ".dockerconfigjson"),
        REGISTRY_VALUE
    );
    assert_eq!(c.field("resend", "api-key"), RESEND_VALUE);

    // ROOT: the key exists and is empty — the pod starts, OIDC is off.
    let oidc = c.created("boss-oidc");
    assert_eq!(oidc["data"]["client-secret"], "", "{oidc}");
    assert_eq!(oidc["metadata"]["namespace"], "boss-x");

    // VALUES NEVER REACH THE LOG. Every value the run produced or copied
    // is known here; none of them is in stdout, stderr, or the argv the
    // stub recorded — they rode stdin.
    let calls = c.calls();
    for (what, value) in [
        ("postgres password", pg.as_str()),
        ("admin password", admin.as_str()),
        ("database url", url.as_str()),
        ("session key", session.as_str()),
        ("registry credential", REGISTRY_VALUE),
        ("resend key", RESEND_VALUE),
    ] {
        assert!(!out.contains(value), "the {what} is on stdout: {out}");
        assert!(!err.contains(value), "the {what} is on stderr: {err}");
        assert!(
            !calls.contains(value),
            "the {what} is in kubectl's argv: {calls}"
        );
    }
    assert!(
        !calls.contains("--from-literal") && !calls.contains("--from-file"),
        "no value rides argv: {calls}"
    );
    assert!(
        !calls.contains("apply") && !calls.contains("patch") && !calls.contains("delete"),
        "create is the only write; a Secret that exists is never rewritten: {calls}"
    );
    let creates = calls.lines().filter(|l| l.ends_with("create -f -")).count();
    assert_eq!(creates, 5, "{calls}");
}

#[test]
fn only_the_absent_secrets_are_touched() {
    require_jq();
    // Second converge on a partly-provisioned instance: everything but
    // the session key exists. One create, and the line says only that.
    let c = Case::new("partial");
    c.present(&[
        "boss-secrets",
        "boss-oidc",
        "forgejo-registry",
        "resend",
        "something-new",
    ]);
    let (rc, out, err) = c.run(
        r#"provision_instance_secrets "$K" "$K" "$K" boss-x "$M" boss "boss-session-key""#,
        &[],
    );
    assert_eq!(rc, 0, "{err}");
    assert_eq!(out.trim(), "minted boss-session-key");
    assert_eq!(c.created_names(), vec!["boss-session-key"]);

    // Nothing absent: nothing created, and the line says so.
    let c = Case::new("nothing");
    let (rc, out, err) = c.run(
        r#"provision_instance_secrets "$K" "$K" "$K" boss-x "$M" boss """#,
        &[],
    );
    assert_eq!(rc, 0, "{err}");
    assert_eq!(out.trim(), "nothing absent");
    assert!(c.created_names().is_empty());
    assert!(!c.calls().contains("create -f -"), "{}", c.calls());
}

#[test]
fn a_shared_secret_without_a_source_or_absent_in_it_is_named_not_guessed() {
    require_jq();
    // No shares_with in instances.toml: the shared ones stay absent and
    // the line says why — the gate will name them, and a person decides.
    let c = Case::new("no-source");
    let (rc, out, err) = c.run(
        r#"provision_instance_secrets "$K" "$K" "$K" boss-x "$M" "" "forgejo-registry resend""#,
        &[],
    );
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        out.trim(),
        "cannot forgejo-registry (shared, but [boss-x] declares no shares_with in instances.toml), \
         resend (shared, but [boss-x] declares no shares_with in instances.toml)"
    );
    assert!(c.created_names().is_empty());

    // The source does not hold it either: named with the reason.
    let c = Case::new("absent-in-source");
    std::fs::remove_file(c.root.join("source/boss/resend.json")).unwrap();
    let (rc, out, err) = c.run(
        r#"provision_instance_secrets "$K" "$K" "$K" boss-x "$M" boss "forgejo-registry resend""#,
        &[],
    );
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        out.trim(),
        "copied forgejo-registry from boss | cannot resend (not found in boss)"
    );
    assert_eq!(c.created_names(), vec!["forgejo-registry"]);
}

#[test]
fn a_create_that_fails_is_named_and_a_key_without_a_recipe_refuses() {
    require_jq();
    let c = Case::new("create-fails");
    let (rc, out, err) = c.run(
        r#"provision_instance_secrets "$K" "$K" "$K" boss-x "$M" boss "boss-secrets boss-session-key""#,
        &[("STUB_CREATE_FAILS", "boss-secrets")],
    );
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        out.trim(),
        "minted boss-session-key | cannot boss-secrets (Error from server (NotFound): error when creating \"STDIN\": namespaces \"x\" not found)"
    );
    assert!(
        !err.contains("postgres://"),
        "a failed create does not echo the object: {err}"
    );

    // The manifests read a key of boss-secrets the recipe does not
    // mint: refuse by name rather than mint a Secret the pod cannot
    // start on ("key not found" in CreateContainerConfigError, which is
    // the same wedge one step later).
    let c = Case::new("unknown-key");
    write_file(
        &c.manifests.join("boss-chore.yaml"),
        r#"{"kind":"CronJob","metadata":{"name":"chore","namespace":"boss-x"},"spec":{"jobTemplate":{"spec":{"template":{"spec":{
  "containers":[{"name":"c","env":[
    {"name":"BOSS_MACHINE_TOKEN","valueFrom":{"secretKeyRef":{"name":"boss-secrets","key":"machine-token"}}}]}]}}}}}}
"#,
    );
    let (rc, out, err) = c.run(
        r#"provision_instance_secrets "$K" "$K" "$K" boss-x "$M" boss "boss-secrets""#,
        &[],
    );
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        out.trim(),
        "cannot boss-secrets (the manifests read key machine-token, which the converge has no recipe for)"
    );
    assert!(c.created_names().is_empty());

    // And a postgres StatefulSet the URL cannot be composed from.
    let c = Case::new("no-postgres");
    write_file(
        &c.manifests.join("boss.yaml"),
        r#"{"kind":"Deployment","metadata":{"name":"boss","namespace":"boss-x"},"spec":{"template":{"spec":{
  "containers":[{"name":"boss","env":[
    {"name":"DATABASE_URL","valueFrom":{"secretKeyRef":{"name":"boss-secrets","key":"database-url"}}}]}]}}}}
"#,
    );
    let (rc, out, _) = c.run(
        r#"provision_instance_secrets "$K" "$K" "$K" boss-x "$M" boss "boss-secrets""#,
        &[],
    );
    assert_eq!(rc, 0);
    assert!(
        out.contains("cannot boss-secrets (no postgres StatefulSet"),
        "the URL is composed from the manifests or not at all: {out}"
    );
    assert!(c.created_names().is_empty());
}

#[test]
fn the_quiet_read_answers_absent_names_and_cannot_tell_apart() {
    require_jq();
    let c = Case::new("absent");
    c.present(&["boss-secrets", "something-new"]);
    let (rc, out, err) = c.run(r#"instance_secrets_absent "$K" "$K" boss-x "$M""#, &[]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(
        out.trim(),
        "boss-oidc boss-session-key forgejo-registry resend",
        "the absent names, sorted as manifest_secrets lists them"
    );
    assert!(
        !err.contains("SKIPPED") && !err.contains("create secret"),
        "the quiet read prints no verdict and no shapes — provisioning comes next: {err}"
    );
    let (rc, _, err) = c.run(
        r#"instance_secrets_absent "$K" "$K" boss-x "$M""#,
        &[("STUB_FORBIDDEN", "resend")],
    );
    assert_eq!(rc, 2, "a read the credential cannot make is CANNOT TELL");
    assert!(err.contains("resend") && err.to_lowercase().contains("forbidden"));
    // The loud gate still reads through it (one presence loop, §9a).
    let lib = std::fs::read_to_string(repo_root().join(LIB)).unwrap();
    let start = lib.find("instance_secret_gate() {").unwrap();
    let body = &lib[start..];
    let end = body.find("\n}\n").unwrap();
    assert!(
        body[..end].contains("instance_secrets_absent"),
        "the gate asks the quiet read rather than looping itself"
    );
    assert_eq!(
        lib.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter(|l| l.contains("create -f -"))
            .count(),
        1,
        "every minted, copied or empty Secret is created through ONE stdin create"
    );
}

#[test]
fn the_runner_provisions_after_the_namespace_exists_and_before_the_loud_gate() {
    let src = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    let loop_start = src
        .find("STAGE=\"apply instances\"")
        .expect("the runner has the apply-instances stage");
    let after = &src[loop_start..];
    let quiet = after
        .find("$(instance_secrets_absent ")
        .expect("the loop reads the absent set quietly first");
    let ns = after
        .find("apply_namespaces \"$ins_ns\"")
        .expect("the instance's Namespace is applied before anything is created in it");
    let provision = after
        .find("$(provision_instance_secrets ")
        .expect("the loop provisions");
    let gate = after
        .find("$(instance_secret_gate ")
        .expect("the loud gate decides");
    let apply = after
        .find("apply_instance \"$ins_ns\"")
        .expect("the loop applies the instance");
    assert!(
        quiet < ns && ns < provision && provision < gate && gate < apply,
        "read quietly → namespace → provision → gate (loud) → apply"
    );
    assert!(
        after[provision..provision + 160].contains("\"$KAPPLY\""),
        "the values ride stdin, so the create goes through the `-i` kubectl (the bug the KAPPLY comment records)"
    );
    assert!(
        after[provision..gate].contains("run_summary_field instance_secrets_minted"),
        "the line rides the converge packet as `instance_secrets_minted`, recorded as soon as it is known"
    );
    assert!(
        after.contains("read -r iname ins_ns tdir _s _h share_ns"),
        "the loop reads the shares_with namespace off the instance list's sixth column"
    );
    // And apply_instance itself goes through the same namespace-first
    // helper: one definition of "the Namespace files first".
    let ai = src
        .find("apply_instance() {")
        .expect("apply_instance is defined");
    assert!(
        src[ai..ai + 200].contains("apply_namespaces \"$ns\""),
        "apply_instance applies the Namespace files through apply_namespaces"
    );
}

#[test]
fn the_instance_list_declares_where_the_playground_shares_from_and_refuses_an_unknown_source() {
    let (rc, out, err) = run_renderer(&repo_root(), &["--instances"]);
    assert_eq!(rc, 0, "{err}");
    let rows: Vec<Vec<&str>> = out.lines().map(|l| l.split('\t').collect()).collect();
    let play = rows
        .iter()
        .find(|r| r[0] == "playground")
        .expect("the playground is an instance");
    assert_eq!(
        play.get(5).copied(),
        Some("boss"),
        "the sixth column is the NAMESPACE the instance shares Secrets with: {out}"
    );
    let prod = rows.iter().find(|r| r[0] == "prod").unwrap();
    assert_eq!(
        prod.get(5).copied().unwrap_or(""),
        "",
        "the source shares from nobody: {out}"
    );
    let toml = std::fs::read_to_string(repo_root().join(INSTANCES)).unwrap();
    assert!(
        toml.contains("shares_with = \"prod\""),
        "the playground declares `shares_with = \"prod\"` — the one key"
    );

    // A fixture tree with the same manifests and an instance list
    // naming a source that does not exist: refused by name, nothing
    // rendered.
    let tree = fixture_tree("unknown-source");
    let path = tree.join(INSTANCES);
    let text = std::fs::read_to_string(&path).unwrap();
    write_file(
        &path,
        &text.replace("shares_with = \"prod\"", "shares_with = \"staging\""),
    );
    for args in [vec!["--instances"], vec!["--all", "OUT"]] {
        let out_dir = tree.join("out");
        let args: Vec<&str> = args
            .iter()
            .map(|a| {
                if *a == "OUT" {
                    out_dir.to_str().unwrap()
                } else {
                    a
                }
            })
            .collect();
        let (rc, _, err) = run_renderer(&tree, &args);
        assert_eq!(rc, 2, "refused: {err}");
        assert!(
            err.contains("shares_with") && err.contains("staging") && err.contains("[playground]"),
            "the refusal names the key, the value and the instance: {err}"
        );
        assert!(!out_dir.exists(), "nothing rendered");
    }
    // An instance that shares with itself is the same refusal.
    let tree = fixture_tree("self-source");
    let path = tree.join(INSTANCES);
    let text = std::fs::read_to_string(&path).unwrap();
    write_file(
        &path,
        &text.replace("shares_with = \"prod\"", "shares_with = \"playground\""),
    );
    let (rc, _, err) = run_renderer(&tree, &["--instances"]);
    assert_eq!(rc, 2, "{err}");
    assert!(
        err.contains("shares_with") && err.contains("itself"),
        "{err}"
    );
}

fn run_renderer(tree: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg(repo_root().join(RENDERER))
        .args(args)
        .env("BOSS_CLUSTER_TREE", tree)
        .output()
        .expect("bash runs the renderer");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// The repository's own manifests, roster, instance list and tenant
/// manifests copied into scratch — the idiom render_instance_sh.rs uses
/// — so a case can perturb the instance list alone.
fn fixture_tree(case: &str) -> PathBuf {
    let tree = scratch_dir(&format!("instance-mints-tree-{case}")).join("tree");
    let manifests = "infra/cluster/manifests";
    std::fs::create_dir_all(tree.join(manifests)).unwrap();
    for e in std::fs::read_dir(repo_root().join(manifests)).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), tree.join(manifests).join(e.file_name())).unwrap();
    }
    for f in ["infra/cluster/instance-manifests.txt", INSTANCES] {
        std::fs::copy(repo_root().join(f), tree.join(f)).unwrap();
    }
    let toml = std::fs::read_to_string(repo_root().join(INSTANCES)).unwrap();
    for line in toml.lines() {
        if let Some(d) = line.trim().strip_prefix("tenant_dir = ") {
            let t = format!("{}/seeds/tenant.toml", d.trim_matches('"'));
            std::fs::create_dir_all(tree.join(&t).parent().unwrap()).unwrap();
            std::fs::copy(repo_root().join(&t), tree.join(&t)).unwrap();
        }
    }
    tree
}
