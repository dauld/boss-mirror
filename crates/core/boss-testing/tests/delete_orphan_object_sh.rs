//! `infra/forge/delete-orphan-object.sh` is RUN, not read — against a
//! stubbed `kubectl` and a fixture tree that is a real git repository,
//! so every verdict below is one the script actually reached.
//!
//! WHY THE VERB EXISTS (backlog a139d5bb, filed urgent 2026-09-10). The
//! converge runs `kubectl apply` with no `--prune`, so deleting a
//! manifest removes the DECLARATION and leaves the OBJECT running. The
//! only way to clear one was a human typing `kubectl -n boss delete svc
//! <name>`, and `infra/ops/verbs.json` — the allowlist the ops-runner
//! executes — had no verb that could. A gated-green car whose lint
//! exits 1 on an orphan therefore could not land: the next converge
//! would fail until a person intervened.
//!
//! WHY NOT A `kubectl delete` VERB. The allowlist's whole value is that
//! every verb is a reviewed word with bounded params; "delete any object
//! by name" hands an agent namespace-wide destruction through an audited
//! door, which is worse than no door. So the verb's authority is
//! DERIVED, not granted: `infra/cluster/undeclared-objects.sh` re-runs
//! the same computation the orphan lint uses — every live object in the
//! namespaces the tree owns that no manifest declares — and the verb
//! acts on one named object ONLY IF that computation names it. Every
//! other answer is a refusal that says which test refused.
//!
//! What each case below pins, in the order the script applies them:
//! the param shape, a committed manifest tree, the derivation's verdict
//! (declared / out of scope / not live / controller-owned / unreadable),
//! the verb's own kind floor, and the capture-before-delete. Plus the
//! allowlist's own validation, exercised THROUGH `ops-runner.sh` with
//! the real `verbs.json`.
//!
//! Nothing here touches a cluster. `kubectl` is a stub on every path,
//! and a `delete` it receives is appended to a file.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// `id <args>` as a trimmed string. The test reads its own account the
/// same way the script under test reads the tree's.
fn id_out(args: &[&str]) -> String {
    let out = Command::new("id").args(args).output().expect("id runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A real account on this box that is NOT the account running the test.
///
/// `as_owner` in the script runs the command directly when `id -un`
/// equals the owner, so the owner-dropping branch is only REACHABLE with
/// an owner the caller is not. A literal name makes that premise a
/// property of whoever runs the suite: this test was written with
/// `"nobody"` hardcoded under a gate that still ran as root, and the
/// moment the gate pod moved to uid 65534 — which IS `nobody` in the
/// base image — the drop correctly stopped happening and the test failed
/// on branches that touched nothing. So the name is MEASURED, not
/// spelled: the first candidate that resolves through `id -u` (the
/// script refuses an owner with no passwd entry) and is not the caller.
/// `nobody` stays first so a root run is unchanged.
fn an_owner_other_than_the_caller() -> Option<String> {
    let me = id_out(&["-un"]);
    ["nobody", "daemon", "bin", "sys", "mail", "root"]
        .into_iter()
        .find(|candidate| {
            *candidate != me
                && Command::new("id")
                    .args(["-u", candidate])
                    .output()
                    .is_ok_and(|o| o.status.success())
        })
        .map(str::to_string)
}

/// A scratch root PER PROCESS, not per case name. A fixed
/// `/tmp/delete-orphan-object-<case>` is shared by every account on the
/// box: a dir left by a root run is one `remove_dir_all` a later run as
/// another uid cannot do — and that failure was discarded, so the run
/// carried on and died 130 lines later on an unreadable bare
/// `PermissionDenied` from a fixture write. The pid makes the root ours,
/// and every failure here names the path it was at.
fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "delete-orphan-object-{case}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("scratch dir {}: {e}", dir.display()));
    dir
}

/// Write, naming the path when it fails. A bare `unwrap()` on a write
/// reports the errno and not the file, which is the whole diagnosis.
fn write_file(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn write_exec(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    write_file(path, body);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A manifest "file" holding one or more objects. The fixture writes
/// JSON into a `.yaml` file — JSON is valid YAML, and `kubectl create
/// --dry-run -o json -f` echoes it back, so the stub can simply `cat`.
fn doc(kind: &str, ns: &str, name: &str) -> String {
    if ns.is_empty() {
        format!(r#"{{"kind":"{kind}","metadata":{{"name":"{name}"}}}}"#)
    } else {
        format!(r#"{{"kind":"{kind}","metadata":{{"name":"{name}","namespace":"{ns}"}}}}"#)
    }
}

/// The fixture tree: a real git repository laid out like the repo, with
/// enough manifests to clear the derivation's "the scrape broke" floors
/// (at least 10 files, at least 20 declared objects).
fn fixture_tree(root: &Path) -> PathBuf {
    let tree = root.join("tree");
    let dir = tree.join("infra/cluster/manifests");
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("fixture manifests dir {}: {e}", dir.display()));
    std::fs::create_dir_all(tree.join("infra/gate-runner"))
        .unwrap_or_else(|e| panic!("fixture gate-runner dir under {}: {e}", tree.display()));

    let mut files: Vec<(String, String)> = vec![
        (
            "namespaces.yaml".into(),
            format!(
                "{}\n{}\n",
                doc("Namespace", "", "boss"),
                doc("Namespace", "", "boss-dev")
            ),
        ),
        (
            "cluster-scoped.yaml".into(),
            format!(
                "{}\n{}\n{}\n",
                doc("ClusterRole", "", "boss-reader"),
                doc("ClusterRoleBinding", "", "boss-reader"),
                doc("StorageClass", "", "longhorn-r1"),
            ),
        ),
        (
            "configmaps.yaml".into(),
            format!("{}\n", doc("ConfigMap", "boss", "boss-config")),
        ),
        (
            "deployment.yaml".into(),
            format!("{}\n", doc("Deployment", "boss", "boss")),
        ),
        (
            "cronjob.yaml".into(),
            format!("{}\n", doc("CronJob", "boss", "boss-backup")),
        ),
        (
            "statefulset.yaml".into(),
            r#"{"kind":"StatefulSet","metadata":{"name":"postgres","namespace":"boss"},
 "spec":{"volumeClaimTemplates":[{"metadata":{"name":"pgdata"}}]}}
"#
            .into(),
        ),
        (
            "dev-access.yaml".into(),
            format!(
                "{}\n{}\n{}\n",
                doc("Role", "boss-dev", "dev-session"),
                doc("RoleBinding", "boss-dev", "dev-session"),
                doc("Secret", "boss-dev", "dev-session-token"),
            ),
        ),
        (
            "dev.yaml".into(),
            format!(
                "{}\n{}\n",
                doc("Service", "boss-dev", "boss-dev"),
                doc("Deployment", "boss-dev", "boss-dev"),
            ),
        ),
        (
            "serviceaccount.yaml".into(),
            format!("{}\n", doc("ServiceAccount", "boss", "boss")),
        ),
        (
            "pvc.yaml".into(),
            format!("{}\n", doc("PersistentVolumeClaim", "boss", "boss-auth")),
        ),
    ];
    // Ten Services in `boss`, one file each: enough files and objects
    // that the floors are cleared by the fixture rather than relaxed
    // for it.
    for n in [
        "boss-jobs-internal",
        "boss-nats-internal",
        "boss-clock-internal",
        "boss-dispatcher-internal",
        "boss-ledger-internal",
        "boss-people-internal",
        "boss-events-internal",
        "boss-policy-internal",
        "boss-content-internal",
        "boss-front",
    ] {
        files.push((
            format!("{n}.yaml"),
            format!("{}\n", doc("Service", "boss", n)),
        ));
    }
    for (name, body) in &files {
        write_file(&dir.join(name), body);
    }

    git(&tree, &["init", "-q", "-b", "main"]);
    git(&tree, &["add", "-A"]);
    git(
        &tree,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture manifests",
        ],
    );
    tree
}

/// The stubbed `kubectl`. It answers exactly the six invocation shapes
/// the derivation and the verb use, reading the "cluster" from files:
///
///   $STUB_LIVE/<Kind>.<ns>  one line per live object: name<TAB>ownerKind
///   $STUB_FORBID            "<Kind> <ns>" per line — not listable
///   $STUB_BADPARSE          a manifest basename that fails to parse
///   $STUB_DELETED           every delete it is asked for, appended
fn stub_kubectl(root: &Path) -> PathBuf {
    let path = root.join("kubectl-stub");
    write_exec(
        &path,
        r#"#!/bin/bash
set -u
live() { # kind ns
    f="$STUB_LIVE/$1.$2"
    [ -f "$f" ] && cat "$f"
}
forbidden() { # kind ns
    [ -n "${STUB_FORBID:-}" ] && [ -f "$STUB_FORBID" ] && grep -qxF "$1 $2" "$STUB_FORBID"
}
case "${1:-}" in
version)
    echo '{}' ;;
create)
    f=""
    while [ $# -gt 0 ]; do [ "$1" = "-f" ] && { f="$2"; break; }; shift; done
    [ -n "$f" ] || { echo "stub: no -f" >&2; exit 1; }
    if [ -n "${STUB_BADPARSE:-}" ] && [ "$(basename "$f")" = "$STUB_BADPARSE" ]; then
        echo "error: error parsing $f: broken fixture" >&2
        exit 1
    fi
    cat "$f" ;;
get)
    kind="$2"; shift 2
    name=""
    case "${1:-}" in -*|"") ;; *) name="$1"; shift ;; esac
    ns=""; fmt=""
    while [ $# -gt 0 ]; do
        case "$1" in
            -n) ns="$2"; shift 2 ;;
            -o) fmt="$2"; shift 2 ;;
            *) shift ;;
        esac
    done
    forbidden "$kind" "$ns" && { echo "Error from server (Forbidden): $kind is forbidden" >&2; exit 1; }
    if [ -z "$name" ]; then
        live "$kind" "$ns"
        exit 0
    fi
    row=$(live "$kind" "$ns" | awk -F'\t' -v n="$name" '$1 == n')
    if [ -z "$row" ]; then
        echo "Error from server (NotFound): $kind \"$name\" not found" >&2
        exit 1
    fi
    case "$fmt" in
        yaml) printf 'kind: %s\nmetadata:\n  name: %s\n  namespace: %s\n' "$kind" "$name" "$ns" ;;
        *) printf '%s\n' "$row" | cut -f2 ;;
    esac ;;
delete)
    kind="$2"; name="$3"; shift 3
    ns=""
    while [ $# -gt 0 ]; do case "$1" in -n) ns="$2"; shift 2 ;; *) shift ;; esac; done
    printf '%s\t%s\t%s\n' "$kind" "$ns" "$name" >> "$STUB_DELETED"
    f="$STUB_LIVE/$kind.$ns"
    [ -f "$f" ] && { grep -v "^$name	" "$f" > "$f.new" || true; mv "$f.new" "$f"; }
    echo "$kind \"$name\" deleted" ;;
*)
    echo "stub kubectl: unexpected invocation: $*" >&2
    exit 64 ;;
esac
"#,
    );
    path
}

/// The live "cluster": `Kind.ns` files of `name<TAB>ownerKind` rows.
fn live_set(root: &Path, rows: &[(&str, &str, &str, &str)]) -> PathBuf {
    let dir = root.join("live");
    std::fs::create_dir_all(&dir).unwrap();
    for (kind, ns, name, owner) in rows {
        let f = dir.join(format!("{kind}.{ns}"));
        let mut body = std::fs::read_to_string(&f).unwrap_or_default();
        body.push_str(&format!("{name}\t{owner}\n"));
        std::fs::write(&f, body).unwrap();
    }
    dir
}

/// Every object the fixture tree declares, live — plus whatever the
/// case adds. A realistic cluster agrees with the tree except for the
/// orphans under test.
fn declared_live() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    let mut v = vec![
        ("ConfigMap", "boss", "boss-config", ""),
        ("Deployment", "boss", "boss", ""),
        ("CronJob", "boss", "boss-backup", ""),
        ("StatefulSet", "boss", "postgres", ""),
        ("ServiceAccount", "boss", "boss", ""),
        ("PersistentVolumeClaim", "boss", "boss-auth", ""),
        ("Role", "boss-dev", "dev-session", ""),
        ("RoleBinding", "boss-dev", "dev-session", ""),
        ("Service", "boss-dev", "boss-dev", ""),
        ("Deployment", "boss-dev", "boss-dev", ""),
    ];
    for n in [
        "boss-jobs-internal",
        "boss-nats-internal",
        "boss-clock-internal",
        "boss-dispatcher-internal",
        "boss-ledger-internal",
        "boss-people-internal",
        "boss-events-internal",
        "boss-policy-internal",
        "boss-content-internal",
        "boss-front",
    ] {
        v.push(("Service", "boss", n, ""));
    }
    v
}

struct Case {
    root: PathBuf,
    tree: PathBuf,
    kubectl: PathBuf,
    live: PathBuf,
    deleted: PathBuf,
}

impl Case {
    /// A case with the fixture tree, the stub, and a live set that is
    /// the declared set plus `extra`.
    fn new(name: &str, extra: &[(&str, &str, &str, &str)]) -> Self {
        let root = scratch(name);
        let tree = fixture_tree(&root);
        let kubectl = stub_kubectl(&root);
        let mut rows = declared_live();
        rows.extend_from_slice(extra);
        let live = live_set(&root, &rows);
        let deleted = root.join("deleted");
        std::fs::write(&deleted, "").unwrap();
        Case {
            root,
            tree,
            kubectl,
            live,
            deleted,
        }
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join("infra/forge/delete-orphan-object.sh"))
            .args(args)
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("BOSS_KUBECTL", &self.kubectl)
            .env("BOSS_CLUSTER_TREE", &self.tree)
            .env("STUB_LIVE", &self.live)
            .env("STUB_DELETED", &self.deleted)
            .env("STUB_FORBID", self.root.join("forbid"))
            .env("STUB_RUNUSER_LOG", self.runuser_log());
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("delete-orphan-object.sh runs");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    fn deletions(&self) -> String {
        std::fs::read_to_string(&self.deleted).unwrap_or_default()
    }

    fn runuser_log(&self) -> PathBuf {
        self.root.join("runuser.log")
    }

    /// A `runuser` on PATH that RECORDS the privilege drop and then runs
    /// the command as whoever is running the test. Standing in for the
    /// real one is what makes the owner-dropping branch reachable from a
    /// single account — the branch that, untested, shipped a verb that
    /// refused on every real invocation.
    fn stub_runuser(&self) -> PathBuf {
        let bin = self.root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let path = bin.join("runuser");
        write_exec(
            &path,
            "#!/bin/bash\n\
             printf '%s\\n' \"$*\" >> \"$STUB_RUNUSER_LOG\"\n\
             # `runuser -u <user> -- <cmd...>`: record who, then run the rest.\n\
             [ \"${1:-}\" = \"-u\" ] || { echo \"stub runuser: unexpected form: $*\" >&2; exit 64; }\n\
             shift 2\n\
             [ \"${1:-}\" = \"--\" ] && shift\n\
             exec \"$@\"\n",
        );
        path
    }

    fn runuser_calls(&self) -> String {
        std::fs::read_to_string(self.runuser_log()).unwrap_or_default()
    }
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(
            text.contains(n),
            "{what}: output does not name {n:?}:\n{text}"
        );
    }
}

// ---------------------------------------------------------------------------
// The one object the verb exists for.
// ---------------------------------------------------------------------------

/// A live object in a namespace the tree owns, of a kind the tree
/// declares there, that no manifest declares: the derivation names it,
/// so the verb deletes it — and captures what it was first, because a
/// deleted object cannot be read back.
#[test]
fn deletes_the_one_object_the_tree_proves_undeclared() {
    let c = Case::new("orphan", &[("Service", "boss", "boss-docs-internal", "")]);
    let (rc, out) = c.run(&["Service/boss/boss-docs-internal"]);
    assert_eq!(rc, 0, "the verb refused a genuine orphan:\n{out}");
    assert_eq!(
        c.deletions().trim(),
        "Service\tboss\tboss-docs-internal",
        "the verb did not delete exactly the named object:\n{out}"
    );
    contains_all(
        &out,
        &["undeclared", "boss-docs-internal", "kind: Service"],
        "the orphan case",
    );
}

/// `--dry-run` reaches the same verdict and deletes nothing: the way to
/// ask the audited door "would this be allowed?" without acting.
#[test]
fn dry_run_answers_without_deleting() {
    let c = Case::new("dry-run", &[("Service", "boss", "boss-docs-internal", "")]);
    let (rc, out) = c.run(&["Service/boss/boss-docs-internal", "--dry-run"]);
    assert_eq!(rc, 0, "--dry-run refused a genuine orphan:\n{out}");
    assert_eq!(c.deletions(), "", "--dry-run deleted something:\n{out}");
    contains_all(&out, &["DRY RUN", "boss-docs-internal"], "the dry-run case");
}

// ---------------------------------------------------------------------------
// Every refusal names what refused it.
// ---------------------------------------------------------------------------

/// The object the tree DOES declare. The refusal names the object and
/// the manifest that declares it — a verdict nobody has to re-derive.
#[test]
fn refuses_an_object_the_tree_declares() {
    let c = Case::new("declared", &[]);
    let (rc, out) = c.run(&["Service/boss/boss-jobs-internal"]);
    assert_ne!(rc, 0, "the verb accepted a DECLARED object:\n{out}");
    assert_eq!(c.deletions(), "", "a declared object was deleted:\n{out}");
    contains_all(
        &out,
        &["boss-jobs-internal", "declared", "boss-jobs-internal.yaml"],
        "the declared case",
    );
}

/// An object that is not there. "Not found" is not "orphaned", and the
/// refusal says so rather than exiting 0 on a no-op.
#[test]
fn refuses_an_object_that_does_not_exist() {
    let c = Case::new("absent", &[]);
    let (rc, out) = c.run(&["Service/boss/boss-ghost"]);
    assert_ne!(
        rc, 0,
        "the verb accepted an object that is not live:\n{out}"
    );
    assert_eq!(c.deletions(), "", "something was deleted:\n{out}");
    contains_all(&out, &["boss-ghost", "not live"], "the absent case");
}

/// A namespace the tree does not own. The tree declares no `Namespace`
/// for it, so "undeclared" is not a question this derivation can answer
/// there — the cluster's own furniture is not BOSS's orphan.
#[test]
fn refuses_a_namespace_the_tree_does_not_own() {
    let c = Case::new("foreign-ns", &[("Service", "kube-system", "kube-dns", "")]);
    let (rc, out) = c.run(&["Service/kube-system/kube-dns"]);
    assert_ne!(
        rc, 0,
        "the verb accepted an object outside the owned namespaces:\n{out}"
    );
    assert_eq!(c.deletions(), "", "something was deleted:\n{out}");
    contains_all(&out, &["kube-system", "own"], "the foreign-namespace case");
}

/// A kind the tree declares nothing of in that namespace: there is no
/// declared set to compare against, so every object of it would look
/// undeclared.
#[test]
fn refuses_a_kind_the_tree_declares_nothing_of() {
    let c = Case::new("unscoped-kind", &[("Ingress", "boss", "boss-front", "")]);
    let (rc, out) = c.run(&["Ingress/boss/boss-front"]);
    assert_ne!(
        rc, 0,
        "the verb accepted a kind the tree declares nothing of:\n{out}"
    );
    assert_eq!(c.deletions(), "", "something was deleted:\n{out}");
    contains_all(&out, &["Ingress", "boss"], "the unscoped-kind case");
}

/// Secret is excluded from the derivation by design — the tree
/// references secrets by name and creates them out of band, so its
/// declaration set for Secrets is incomplete and a sweep against it
/// would call every legitimate one an orphan.
#[test]
fn refuses_an_excluded_kind() {
    let c = Case::new("excluded-kind", &[("Secret", "boss-dev", "some-token", "")]);
    let (rc, out) = c.run(&["Secret/boss-dev/some-token"]);
    assert_ne!(rc, 0, "the verb accepted a Secret:\n{out}");
    assert_eq!(c.deletions(), "", "a Secret was deleted:\n{out}");
    contains_all(&out, &["Secret"], "the excluded-kind case");
}

/// A controller made it from a declared parent. Deleting the parent's
/// manifest is the declared change; the child follows.
#[test]
fn refuses_a_controller_owned_object() {
    let c = Case::new(
        "owned",
        &[("Deployment", "boss", "boss-sidecar", "StatefulSet")],
    );
    let (rc, out) = c.run(&["Deployment/boss/boss-sidecar"]);
    assert_ne!(rc, 0, "the verb accepted a controller-owned object:\n{out}");
    assert_eq!(
        c.deletions(),
        "",
        "a controller-owned object was deleted:\n{out}"
    );
    contains_all(
        &out,
        &["boss-sidecar", "owner"],
        "the controller-owned case",
    );
}

/// "Could not look" must never read as "nothing to report". A credential
/// that cannot list the pair means the derivation cannot answer, and the
/// verb refuses rather than treating an unlistable object as undeclared.
#[test]
fn refuses_when_the_credential_cannot_list_the_pair() {
    let c = Case::new(
        "forbidden",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    std::fs::write(c.root.join("forbid"), "Service boss\n").unwrap();
    let (rc, out) = c.run(&["Service/boss/boss-docs-internal"]);
    assert_ne!(rc, 0, "the verb acted on an unreadable pair:\n{out}");
    assert_eq!(c.deletions(), "", "something was deleted:\n{out}");
    contains_all(&out, &["Service", "boss"], "the unreadable case");
}

/// A manifest that does not parse drops its objects from the declared
/// set, which would make everything it declares look undeclared. The
/// derivation must fail loudly instead of quietly scraping less.
#[test]
fn refuses_when_a_manifest_cannot_be_parsed() {
    let c = Case::new("badparse", &[("Service", "boss", "boss-docs-internal", "")]);
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join("infra/forge/delete-orphan-object.sh"))
        .arg("Service/boss/boss-docs-internal")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_KUBECTL", &c.kubectl)
        .env("BOSS_CLUSTER_TREE", &c.tree)
        .env("STUB_LIVE", &c.live)
        .env("STUB_DELETED", &c.deleted)
        .env("STUB_BADPARSE", "boss-jobs-internal.yaml");
    let out = cmd.output().expect("script runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "the verb acted on a partial scrape:\n{text}"
    );
    assert_eq!(c.deletions(), "", "something was deleted:\n{text}");
    contains_all(
        &text,
        &["boss-jobs-internal.yaml", "parse"],
        "the bad-parse case",
    );
}

/// The declaration set the verb derives its authority from must be
/// COMMITTED content. An uncommitted edit under the manifests directory
/// — a rename in progress, a local experiment — means the tree that
/// proves the object undeclared is not the tree anyone reviewed.
#[test]
fn refuses_an_uncommitted_manifest_tree() {
    let c = Case::new("dirty", &[("Service", "boss", "boss-docs-internal", "")]);
    std::fs::remove_file(c.tree.join("infra/cluster/manifests/boss-front.yaml")).unwrap();
    let (rc, out) = c.run(&["Service/boss/boss-docs-internal"]);
    assert_ne!(
        rc, 0,
        "the verb derived authority from an uncommitted tree:\n{out}"
    );
    assert_eq!(c.deletions(), "", "something was deleted:\n{out}");
    contains_all(&out, &["boss-front.yaml"], "the dirty-tree case");
}

/// The verb's own floor, narrower than the derivation's scope: a kind
/// whose deletion destroys bytes or credentials stays a human step even
/// when the tree proves the object undeclared.
#[test]
fn refuses_a_kind_outside_the_verbs_floor() {
    let c = Case::new(
        "kind-floor",
        &[("PersistentVolumeClaim", "boss", "stray-data", "")],
    );
    let (rc, out) = c.run(&["PersistentVolumeClaim/boss/stray-data"]);
    assert_ne!(rc, 0, "the verb deleted a PVC:\n{out}");
    assert_eq!(c.deletions(), "", "a PVC was deleted:\n{out}");
    contains_all(&out, &["PersistentVolumeClaim"], "the kind-floor case");
    // And the narrowing is real: the derivation DOES name it, so the
    // refusal is the verb's floor and not a derivation miss.
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join("infra/cluster/undeclared-objects.sh"))
        .args(["--check", "PersistentVolumeClaim/boss/stray-data"])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_KUBECTL", &c.kubectl)
        .env("BOSS_CLUSTER_TREE", &c.tree)
        .env("STUB_LIVE", &c.live)
        .env("STUB_DELETED", &c.deleted);
    let d = cmd.output().expect("undeclared-objects.sh runs");
    assert_eq!(
        d.status.code(),
        Some(0),
        "the derivation did not name the PVC, so the kind floor is untested:\n{}{}",
        String::from_utf8_lossy(&d.stdout),
        String::from_utf8_lossy(&d.stderr)
    );
}

/// A malformed argument is refused before anything runs — and by the
/// script itself, not only by the allowlist, so the bound does not
/// depend on one layer.
#[test]
fn refuses_a_malformed_argument() {
    let c = Case::new("malformed", &[]);
    for bad in [
        "boss-docs-internal",
        "service/boss/x",
        "Service/boss",
        "Service//x",
        "",
    ] {
        let (rc, out) = c.run(&[bad]);
        assert_ne!(rc, 0, "the verb accepted {bad:?}:\n{out}");
        assert_eq!(c.deletions(), "", "{bad:?} deleted something:\n{out}");
        // And the refusal says what the shape is — so this case cannot
        // pass merely because the script is absent.
        contains_all(
            &out,
            &["<Kind>/<namespace>/<name>"],
            &format!("the {bad:?} case"),
        );
    }
}

// ---------------------------------------------------------------------------
// THE GIT READS RUN AS THE CHECKOUT'S OWNER.
// ---------------------------------------------------------------------------
// THE DEFECT (ops-request c9877f75, measured on the forge 2026-09-10). The
// first live run through the audited door refused: "/home/david/boss is
// not a git checkout, so the declaration set is not reviewable content."
// It is a checkout — forge-converge.sh fetches into it every tick. What
// failed is `git` run AS ROOT in a david-owned clone: since 2.35.2 git
// refuses to read across an ownership boundary ("dubious ownership"), and
// the ops-runner executes every verb as root. The bound was right and the
// refusal was safe; it was also unsatisfiable.
//
// All sixteen cases above passed while that was true, because a test
// creates its fixture as the user running it, so owner == caller and the
// drop is never taken. That is the hole these three close.
//
// The pattern is infra/gcp/boss-gcp-converge.sh's `as_owner` — the same
// hazard, already solved and already lint-enforced there
// (infra/lint/boss-gcp-converges-itself.sh §2b), with the probe runner's
// non-login `runuser -u <user> --` + explicit HOME for the invocation,
// because these are reads that need no credential helper and a login
// shell's profile output would land in a captured `rev-parse`.

/// With an owner that is not the caller, every git read is issued through
/// `runuser`, and the run still reaches its verdict.
///
/// The owner is MEASURED, not spelled — see `an_owner_other_than_the_caller`.
/// Naming it `nobody` made this case pass only while the gate ran as root,
/// and it failed for two branches that touched nothing the night the gate
/// pod moved to uid 65534 — `nobody` itself — because the drop it asserts
/// had correctly stopped being the thing the script does.
#[test]
fn the_git_reads_drop_to_the_checkouts_owner() {
    let Some(owner) = an_owner_other_than_the_caller() else {
        eprintln!(
            "SKIPPING the_git_reads_drop_to_the_checkouts_owner AND ASSERTING NOTHING: \
             this box has no resolvable account other than the caller ({} / uid {}), \
             so the owner-drop branch cannot be reached — the privilege drop is \
             UNCOVERED on this run",
            id_out(&["-un"]),
            id_out(&["-u"])
        );
        return;
    };
    let c = Case::new(
        "owner-drop",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    c.stub_runuser();
    let (rc, out) = c.run_env(
        &["Service/boss/boss-docs-internal", "--dry-run"],
        &[("BOSS_CLUSTER_TREE_OWNER", owner.clone())],
    );
    assert_eq!(
        rc, 0,
        "the verb refused when the tree belongs to someone else ({owner}):\n{out}"
    );
    assert_eq!(c.deletions(), "", "--dry-run deleted something:\n{out}");
    let calls = c.runuser_calls();
    assert!(
        !calls.is_empty(),
        "no git read went through runuser — it ran as the caller ({}, owner {owner}), \
         which is the defect:\n{out}",
        id_out(&["-un"])
    );
    let dropped_to = format!("-u {owner}");
    for needle in [dropped_to.as_str(), "rev-parse", "status --porcelain"] {
        assert!(
            calls.contains(needle),
            "the dropped commands do not include {needle:?}:\n{calls}"
        );
    }
    // And nothing ran git as the caller: every git invocation the script
    // makes is inside a dropped command.
    assert_eq!(
        calls.matches("git -C").count(),
        3,
        "expected the three git reads (rev-parse --git-dir, status, rev-parse HEAD):\n{calls}"
    );
}

/// When the caller already owns the checkout there is nothing to drop,
/// and the script must not reach for `runuser` — the forge's converge
/// skips it the same way, which is how a lint can drive the loop at all.
#[test]
fn it_does_not_drop_when_the_caller_owns_the_tree() {
    let c = Case::new(
        "owner-is-caller",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    c.stub_runuser();
    let (rc, out) = c.run(&["Service/boss/boss-docs-internal", "--dry-run"]);
    assert_eq!(rc, 0, "the verb refused on a tree the caller owns:\n{out}");
    assert_eq!(
        c.runuser_calls(),
        "",
        "the script dropped privilege it did not need:\n{out}"
    );
}

/// `stat -c %U` prints `UNKNOWN` when no passwd entry exists for the
/// owning uid. Proceeding would run git as the caller again — the exact
/// bug — so the verb refuses and names what it could not resolve.
#[test]
fn refuses_an_owner_it_cannot_resolve() {
    let c = Case::new(
        "owner-unknown",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    c.stub_runuser();
    for owner in ["UNKNOWN", "nosuchuser-zzz"] {
        let (rc, out) = c.run_env(
            &["Service/boss/boss-docs-internal", "--dry-run"],
            &[("BOSS_CLUSTER_TREE_OWNER", owner.into())],
        );
        assert_ne!(
            rc, 0,
            "the verb proceeded with an unresolvable owner {owner:?}:\n{out}"
        );
        assert_eq!(c.deletions(), "", "something was deleted:\n{out}");
        contains_all(&out, &[owner, "owner"], &format!("the {owner:?} case"));
    }
}

// ---------------------------------------------------------------------------
// The derivation is ONE definition, with a list form the lint can use.
// ---------------------------------------------------------------------------

/// `--list` is the whole orphan set, which is what the orphan lint
/// reports and what `--check` answers one object of. Both come from the
/// same run of the same computation, so the verb cannot admit an object
/// the lint would call declared.
#[test]
fn the_list_and_the_check_agree() {
    let c = Case::new("list", &[("Service", "boss", "boss-docs-internal", "")]);
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join("infra/cluster/undeclared-objects.sh"))
        .arg("--list")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_KUBECTL", &c.kubectl)
        .env("BOSS_CLUSTER_TREE", &c.tree)
        .env("STUB_LIVE", &c.live);
    let out = cmd.output().expect("undeclared-objects.sh runs");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        out.status.code(),
        Some(0),
        "--list failed:\n{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>(),
        vec!["Service\tboss\tboss-docs-internal"],
        "--list is not exactly the one orphan:\n{text}"
    );
}

// ---------------------------------------------------------------------------
// THROUGH THE RUNNER, with the real allowlist.
// ---------------------------------------------------------------------------

/// The allowlist's own validation: a malformed param never reaches the
/// script, and the refusal names the pattern on the packet's step — the
/// same text the journal carries.
#[test]
fn the_allowlist_refuses_a_malformed_param() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new(
        "runner-malformed",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    let verbs = rewritten_verbs(&c.root);
    let (text, meta) = run_runner(&c, &verbs, r#"["services/boss/x"]"#);
    let meta = meta.expect("the runner completed the execute step");
    assert_eq!(
        meta["disposition"], "refused",
        "a malformed param was not refused:\n{text}"
    );
    let reason = meta["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("object") && reason.contains("does not match"),
        "the refusal does not name the param and its pattern: {reason}"
    );
    assert_eq!(
        reason,
        meta["output"].as_str().unwrap_or_default(),
        "reason and output differ on a refusal"
    );
    assert!(
        text.contains(reason),
        "the journal line does not carry the same reason:\n{text}"
    );
    assert_eq!(
        c.deletions(),
        "",
        "a refused packet deleted something:\n{text}"
    );
}

/// And a well-formed one does reach the script, with `--dry-run` — the
/// verb is exercisable end to end through the audited door without
/// deleting anything.
#[test]
fn the_runner_answers_a_dry_run() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new(
        "runner-dry-run",
        &[("Service", "boss", "boss-docs-internal", "")],
    );
    let verbs = rewritten_verbs(&c.root);
    let (text, meta) = run_runner(
        &c,
        &verbs,
        r#"["Service/boss/boss-docs-internal","--dry-run"]"#,
    );
    let meta = meta.expect("the runner completed the execute step");
    assert_eq!(
        meta["disposition"], "answered",
        "the dry run was not answered:\n{text}"
    );
    assert_eq!(
        meta["exit_code"], "0",
        "the dry run did not exit 0:\n{text}"
    );
    let output = meta["output"].as_str().unwrap_or_default();
    assert!(
        output.contains("DRY RUN"),
        "the output is not the dry run's:\n{output}"
    );
    assert_eq!(c.deletions(), "", "a dry run deleted something:\n{text}");
}

/// The real `infra/ops/verbs.json`, with the forge's absolute script
/// paths rewritten to this tree — the allowlist under test is the one
/// that ships.
fn rewritten_verbs(root: &Path) -> PathBuf {
    let src = std::fs::read_to_string(repo_root().join("infra/ops/verbs.json"))
        .expect("verbs.json is readable");
    let dst = root.join("verbs.json");
    std::fs::write(
        &dst,
        src.replace("/home/david/boss/", &format!("{}/", repo_root().display())),
    )
    .unwrap();
    dst
}

/// One open ops-request for the forge carrying the verb and args, run
/// through `ops-runner.sh` against a stubbed system of record.
fn run_runner(c: &Case, verbs: &Path, args: &str) -> (String, Option<serde_json::Value>) {
    let bin = c.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(
        &bin.join("curl"),
        "#!/bin/sh\n\
         for a in \"$@\"; do case \"$a\" in @*) cp \"${a#@}\" \"$STUB_PUT\"; exit 0;; esac; done\n\
         cat \"$STUB_JOBS\"\n",
    );
    std::fs::write(
        c.root.join("jobs.json"),
        format!(
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"forge","verb":"delete-orphan-object","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}]}}"#
        ),
    )
    .unwrap();
    let put = c.root.join("put.json");
    let _ = std::fs::remove_file(&put);
    let out = Command::new("sh")
        .arg(repo_root().join("infra/ops/ops-runner.sh"))
        .env_clear()
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOST_ID", "forge")
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        .env("OPS_VERBS_FILE", verbs)
        .env("STUB_JOBS", c.root.join("jobs.json"))
        .env("STUB_PUT", &put)
        .env("BOSS_KUBECTL", &c.kubectl)
        .env("BOSS_CLUSTER_TREE", &c.tree)
        .env("STUB_LIVE", &c.live)
        .env("STUB_DELETED", &c.deleted)
        .output()
        .expect("ops-runner.sh runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let meta = std::fs::read_to_string(&put)
        .ok()
        .map(|s| serde_json::from_str::<serde_json::Value>(&s).expect("PUT payload is JSON"))
        .map(|v| v["metadata"].clone());
    (text, meta)
}
