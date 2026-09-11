//! `infra/cluster/undeclared-objects.sh` is RUN, not read — against a
//! stubbed `kubectl` and a fixture manifest tree, so every verdict below
//! is one the script actually reached. Nothing here touches a cluster.
//!
//! WHY THIS FILE EXISTS, SEPARATELY FROM THE VERB'S (backlog 19aa75e0).
//! The derivation answers one question — "what is running that the tree
//! does not declare" — and two programs read the answer: the orphan lint
//! reports it, and `delete-orphan-object` DERIVES ITS AUTHORITY from it.
//! So the failure mode that matters is not a wrong orphan; it is a
//! SMALLER declared set. A manifest that will not parse drops every
//! object it declares out of the declared set, and a declared object
//! then reads as UNDECLARED — which, at the other end of the verb, is a
//! path to deleting it.
//!
//! `delete_orphan_object_sh.rs` covers that through `--check`, the one
//! mode the verb calls. It is the narrow mode: `--list` and `--declared`
//! are the modes the LINT reads, and their refusal was pinned by
//! nothing. These tests pin all three, and pin the two properties a
//! refusal needs in order to be read as one:
//!
//!   * it NAMES the file that would not parse, and
//!   * its exit code is distinguishable from both "clean" and "found an
//!     orphan" — which in `--list` share exit 0, so a consumer that only
//!     tests `-ne 0` cannot tell a refusal from a finding.
//!
//! Plus the reasons a refusal must carry rather than drop: a pair this
//! credential cannot list, and an object it cannot read, each with the
//! server's own words. "Could not look" reduced to a count is the
//! record thrown away before the reader sees it (CLAUDE.md §Diagnosis).

use std::path::{Path, PathBuf};
use std::process::Command;

/// The derivation's exit code for "cannot answer" — distinct from 0
/// (answered; stdout is the answer), 1 (a usage error: this run asked
/// nothing well-formed) and 3 (`--check` says NO). The verb maps every
/// code other than 0 and 3 to CANNOT ANSWER already, so this is a
/// sharpening of the contract, not a break in it.
const CANNOT_ANSWER: i32 = 4;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

/// A scratch directory THIS uid can own outright. `/tmp` is sticky, so a
/// fixed name left behind by another uid is a directory this run can
/// neither remove nor write — the gate runs as 65534 and a developer as
/// root, and a test that fails on whichever ran second is a test that
/// reds cars for no reason.
fn scratch(case: &str) -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(u32::MAX);
    let dir = std::env::temp_dir().join(format!("undeclared-objects-{case}-{uid}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write_exec(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// One object, as JSON. The fixture writes JSON into `.yaml` files —
/// JSON is valid YAML, and `kubectl create --dry-run -o json -f` echoes
/// it back, so the stub can simply `cat`.
fn doc(kind: &str, ns: &str, name: &str) -> String {
    if ns.is_empty() {
        format!(r#"{{"kind":"{kind}","metadata":{{"name":"{name}"}}}}"#)
    } else {
        format!(r#"{{"kind":"{kind}","metadata":{{"name":"{name}","namespace":"{ns}"}}}}"#)
    }
}

/// Every object the fixture tree declares in a namespace it owns, as
/// `(kind, ns, name)`. The live set is built from this, so the fixture's
/// cluster agrees with its tree except where a case says otherwise.
fn declared_in_namespaces() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut v = vec![
        ("ConfigMap", "boss", "boss-config"),
        ("Deployment", "boss", "boss"),
        ("CronJob", "boss", "boss-backup"),
        ("ServiceAccount", "boss", "boss"),
        ("PersistentVolumeClaim", "boss", "boss-auth"),
        ("Role", "boss-dev", "dev-session"),
        ("RoleBinding", "boss-dev", "dev-session"),
        ("Service", "boss-dev", "boss-dev"),
        ("Deployment", "boss-dev", "boss-dev"),
    ];
    for n in SERVICES {
        v.push(("Service", "boss", n));
    }
    v
}

/// Twelve Services in `boss`, one manifest each — enough files and
/// objects that the derivation's "the scrape broke" floors (at least 10
/// files, at least 20 objects) are cleared by the fixture rather than
/// relaxed for it.
const SERVICES: [&str; 12] = [
    "svc-a", "svc-b", "svc-c", "svc-d", "svc-e", "svc-f", "svc-g", "svc-h", "svc-i", "svc-j",
    "svc-k", "svc-l",
];

/// The manifest tree, laid out like the repo.
fn fixture_tree(root: &Path) -> PathBuf {
    let tree = root.join("tree");
    let dir = tree.join("infra/cluster/manifests");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(tree.join("infra/gate-runner")).unwrap();

    let write = |name: &str, body: String| std::fs::write(dir.join(name), body).unwrap();
    write(
        "namespaces.yaml",
        format!(
            "{}\n{}\n",
            doc("Namespace", "", "boss"),
            doc("Namespace", "", "boss-dev")
        ),
    );
    write(
        "cluster-scoped.yaml",
        format!(
            "{}\n{}\n",
            doc("ClusterRole", "", "boss-reader"),
            doc("StorageClass", "", "longhorn-r1")
        ),
    );
    write(
        "configmaps.yaml",
        format!("{}\n", doc("ConfigMap", "boss", "boss-config")),
    );
    write(
        "deployment.yaml",
        format!("{}\n", doc("Deployment", "boss", "boss")),
    );
    write(
        "cronjob.yaml",
        format!("{}\n", doc("CronJob", "boss", "boss-backup")),
    );
    write(
        "serviceaccount.yaml",
        format!("{}\n", doc("ServiceAccount", "boss", "boss")),
    );
    write(
        "pvc.yaml",
        format!("{}\n", doc("PersistentVolumeClaim", "boss", "boss-auth")),
    );
    write(
        "dev-access.yaml",
        format!(
            "{}\n{}\n",
            doc("Role", "boss-dev", "dev-session"),
            doc("RoleBinding", "boss-dev", "dev-session")
        ),
    );
    write(
        "dev.yaml",
        format!(
            "{}\n{}\n",
            doc("Service", "boss-dev", "boss-dev"),
            doc("Deployment", "boss-dev", "boss-dev")
        ),
    );
    for n in SERVICES {
        write(
            &format!("{n}.yaml"),
            format!("{}\n", doc("Service", "boss", n)),
        );
    }
    tree
}

/// The stubbed `kubectl`, answering the shapes the derivation uses and
/// reading the "cluster" from files:
///
///   $STUB_LIVE/<Kind>.<ns>  one line per live object: name<TAB>ownerKind
///   $STUB_FORBID            "<Kind> <ns>" per line — not listable
///   $STUB_FORBID_GET        "<Kind> <ns> <name>" per line — not readable
///   $STUB_BADPARSE          a manifest basename that will not parse
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
listed_in() { # file key
    [ -n "${1:-}" ] && [ -f "$1" ] && grep -qxF "$2" "$1"
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
    ns=""
    while [ $# -gt 0 ]; do
        case "$1" in
            -n) ns="$2"; shift 2 ;;
            -o) shift 2 ;;
            *) shift ;;
        esac
    done
    if listed_in "${STUB_FORBID:-}" "$kind $ns"; then
        echo "Error from server (Forbidden): cannot list $kind in $ns with this credential" >&2
        exit 1
    fi
    if [ -z "$name" ]; then
        live "$kind" "$ns"
        exit 0
    fi
    if listed_in "${STUB_FORBID_GET:-}" "$kind $ns $name"; then
        echo "Error from server (Forbidden): cannot get $kind $name in $ns with this credential" >&2
        exit 1
    fi
    row=$(live "$kind" "$ns" | awk -F'\t' -v n="$name" '$1 == n')
    if [ -z "$row" ]; then
        echo "Error from server (NotFound): $kind \"$name\" not found" >&2
        exit 1
    fi
    printf '%s\n' "$row" | cut -f2 ;;
*)
    echo "stub kubectl: unexpected invocation: $*" >&2
    exit 64 ;;
esac
"#,
    );
    path
}

struct Case {
    root: PathBuf,
    tree: PathBuf,
    kubectl: PathBuf,
    live: PathBuf,
}

impl Case {
    /// A case whose cluster is the fixture's declared set plus `extra`
    /// (`kind, ns, name, ownerKind`).
    fn new(name: &str, extra: &[(&str, &str, &str, &str)]) -> Self {
        let root = scratch(name);
        let tree = fixture_tree(&root);
        let kubectl = stub_kubectl(&root);
        let live = root.join("live");
        std::fs::create_dir_all(&live).unwrap();
        let mut rows: Vec<(&str, &str, &str, &str)> = declared_in_namespaces()
            .into_iter()
            .map(|(k, ns, n)| (k, ns, n, ""))
            .collect();
        rows.extend_from_slice(extra);
        for (kind, ns, name, owner) in rows {
            let f = live.join(format!("{kind}.{ns}"));
            let mut body = std::fs::read_to_string(&f).unwrap_or_default();
            body.push_str(&format!("{name}\t{owner}\n"));
            std::fs::write(&f, body).unwrap();
        }
        Case {
            root,
            tree,
            kubectl,
            live,
        }
    }

    /// `(exit code, stdout, stdout+stderr)`. stdout is kept apart
    /// because in `--list` it IS the answer: a refusal must leave it
    /// empty, not merely exit nonzero.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> (i32, String, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join("infra/cluster/undeclared-objects.sh"))
            .args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("BOSS_KUBECTL", &self.kubectl)
            .env("BOSS_CLUSTER_TREE", &self.tree)
            .env("STUB_LIVE", &self.live);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("undeclared-objects.sh runs");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let all = format!("{stdout}{}", String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), stdout, all)
    }

    /// Declare `Kind <ns>` unlistable by this credential.
    fn forbid(&self, pairs: &[(&str, &str)]) -> (&'static str, String) {
        let p = self.root.join("forbid");
        let body: String = pairs.iter().map(|(k, ns)| format!("{k} {ns}\n")).collect();
        std::fs::write(&p, body).unwrap();
        ("STUB_FORBID", p.display().to_string())
    }

    /// Declare one object unreadable by this credential.
    fn forbid_get(&self, kind: &str, ns: &str, name: &str) -> (&'static str, String) {
        let p = self.root.join("forbid-get");
        std::fs::write(&p, format!("{kind} {ns} {name}\n")).unwrap();
        ("STUB_FORBID_GET", p.display().to_string())
    }

    fn badparse(&self, basename: &str) -> (&'static str, String) {
        ("STUB_BADPARSE", basename.to_string())
    }
}

fn names_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(
            text.contains(n),
            "{what}: the output never names {n:?}:\n{text}"
        );
    }
}

// ---------------------------------------------------------------------------
// The answer, when the derivation can reach one.
// ---------------------------------------------------------------------------

/// The baseline every refusal below is measured against: a tree and a
/// cluster that agree, and an empty orphan set reported as such. Here so
/// that "it refuses" can never be satisfied by refusing everything.
#[test]
fn list_answers_cleanly_when_the_tree_and_the_cluster_agree() {
    let c = Case::new("clean", &[]);
    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "a clean tree did not answer:\n{all}");
    assert_eq!(stdout, "", "a clean tree named an orphan:\n{all}");
    names_all(&all, &["0 undeclared object(s)"], "the clean case");
}

/// A live object no manifest declares IS the finding, and it rides on
/// stdout so a consumer can read it.
#[test]
fn list_names_a_genuine_orphan_on_stdout() {
    let c = Case::new("orphan", &[("Service", "boss", "boss-docs-internal", "")]);
    let (rc, stdout, all) = c.run(&["--list"]);
    assert_eq!(rc, 0, "the orphan case did not answer:\n{all}");
    assert_eq!(
        stdout.trim(),
        "Service\tboss\tboss-docs-internal",
        "the orphan set is not what the cluster holds:\n{all}"
    );
}

// ---------------------------------------------------------------------------
// A manifest that will not parse. The defect this file was written for.
// ---------------------------------------------------------------------------

/// `svc-a.yaml` declares `Service/boss/svc-a` and the object is live, so
/// the tree and the cluster agree about it. Make the file unparseable
/// and its declaration drops out of the declared set — at which point a
/// DECLARED object reads as undeclared, and the verb that derives its
/// authority from this computation would delete it.
///
/// So the refusal has to be a refusal on all three counts: it names the
/// file, it says nothing about any object, and its exit code is the
/// cannot-answer one rather than the 0 that "clean" and "found an
/// orphan" both use.
#[test]
fn list_refuses_and_names_the_manifest_that_would_not_parse() {
    let c = Case::new("list-badparse", &[]);
    let bad = c.badparse("svc-a.yaml");
    let (rc, stdout, all) = c.run_env(&["--list"], &[bad]);
    assert_eq!(
        rc, CANNOT_ANSWER,
        "a partial scrape did not exit CANNOT_ANSWER ({CANNOT_ANSWER}):\n{all}"
    );
    assert_eq!(
        stdout, "",
        "the derivation reported an orphan set derived from a partial scrape:\n{all}"
    );
    assert!(
        !all.contains("svc-a\tboss")
            && !all.contains("0 undeclared")
            && !all.contains("1 undeclared"),
        "a partial scrape still produced a verdict about objects:\n{all}"
    );
    names_all(
        &all,
        &["svc-a.yaml", "cannot parse", "broken fixture"],
        "the unparseable-manifest case",
    );
}

/// `--declared` is the other half the lint reads, and it has the same
/// obligation: a declared set missing a file is not a smaller declared
/// set, it is no answer.
#[test]
fn declared_refuses_and_names_the_manifest_that_would_not_parse() {
    let c = Case::new("declared-badparse", &[]);
    let bad = c.badparse("deployment.yaml");
    let (rc, stdout, all) = c.run_env(&["--declared"], &[bad]);
    assert_eq!(
        rc, CANNOT_ANSWER,
        "--declared answered from a partial scrape:\n{all}"
    );
    assert_eq!(
        stdout, "",
        "--declared emitted a partial declared set:\n{all}"
    );
    names_all(
        &all,
        &["deployment.yaml", "cannot parse"],
        "the --declared unparseable case",
    );
}

/// The property a consumer needs and `-ne 0` cannot give it: in `--list`
/// a clean run and a run that found an orphan BOTH exit 0, so the only
/// way to read "I could not answer" is a code of its own. Pinned as one
/// comparison because the three codes are one contract.
#[test]
fn cannot_answer_is_distinguishable_from_clean_and_from_an_orphan() {
    let clean = Case::new("codes-clean", &[]).run(&["--list"]);
    let found = Case::new("codes-orphan", &[("Service", "boss", "ghost-svc", "")]).run(&["--list"]);
    let refused = {
        let c = Case::new("codes-refused", &[]);
        let bad = c.badparse("svc-b.yaml");
        c.run_env(&["--list"], &[bad])
    };
    assert_eq!(clean.0, 0, "clean:\n{}", clean.2);
    assert_eq!(found.0, 0, "found an orphan:\n{}", found.2);
    assert_ne!(
        refused.0, clean.0,
        "a refusal is indistinguishable from a clean run by exit code alone:\n{}",
        refused.2
    );
    assert_ne!(
        refused.0, found.0,
        "a refusal is indistinguishable from a finding by exit code alone:\n{}",
        refused.2
    );
    // And the two answers are told apart by stdout, which is why a
    // refusal must leave it empty.
    assert_eq!(clean.1, "");
    assert_eq!(found.1.trim(), "Service\tboss\tghost-svc");
    assert_eq!(refused.1, "");
}

// ---------------------------------------------------------------------------
// What a refusal must CARRY. A reason reduced to a count is the record
// thrown away before the reader sees it.
// ---------------------------------------------------------------------------

/// `--list` already declines to claim an unreadable pair clean, and says
/// UNVERIFIED with the pair named — the right shape. What it dropped was
/// the server's own words: the error text went to one scratch file that
/// the next pair overwrote, and nothing ever read it. Two unreadable
/// pairs must leave two reasons in the output, not a count.
#[test]
fn list_keeps_the_reason_each_unreadable_pair_gave() {
    let c = Case::new("unreadable", &[]);
    let forbid = c.forbid(&[("Service", "boss"), ("ConfigMap", "boss")]);
    let (rc, _stdout, all) = c.run_env(&["--list"], &[forbid]);
    assert_eq!(
        rc, 0,
        "a partly-readable cluster is an answer about the part it read:\n{all}"
    );
    names_all(
        &all,
        &[
            "UNVERIFIED",
            "Service in boss",
            "ConfigMap in boss",
            "cannot list Service in boss",
            "cannot list ConfigMap in boss",
        ],
        "the unreadable-pairs case",
    );
}

/// The last place a read error was swallowed whole. When an object is
/// not in the pair's listing the derivation asks about it directly, to
/// tell a controller-owned object from an absent one — and read a
/// FAILURE of that read as "it is not live". A credential that cannot
/// get the object says nothing about whether it exists, so this is
/// CANNOT ANSWER with the server's words, not a verdict.
#[test]
fn check_cannot_answer_when_the_object_itself_cannot_be_read() {
    let c = Case::new("forbidden-get", &[]);
    let deny = c.forbid_get("Service", "boss", "mystery-svc");
    let (rc, _stdout, all) = c.run_env(&["--check", "Service/boss/mystery-svc"], &[deny]);
    assert_eq!(
        rc, CANNOT_ANSWER,
        "an unreadable object was given a verdict:\n{all}"
    );
    names_all(
        &all,
        &["CANNOT ANSWER", "cannot get Service mystery-svc in boss"],
        "the unreadable-object case",
    );
    assert!(
        !all.contains("not live"),
        "'could not read it' was reported as 'it is not live':\n{all}"
    );
}

/// An object that really is absent keeps the verdict it had: NotFound is
/// a fact about the object, not about the credential. Here so the test
/// above cannot be satisfied by giving up on every failed read.
#[test]
fn check_still_says_not_live_for_an_object_that_is_absent() {
    let c = Case::new("absent", &[]);
    let (rc, _stdout, all) = c.run(&["--check", "Service/boss/boss-ghost"]);
    assert_eq!(rc, 3, "an absent object was not refused as absent:\n{all}");
    names_all(&all, &["boss-ghost", "not live"], "the absent case");
}

// ---------------------------------------------------------------------------
// The consumer. Moving the silence one layer up is not a fix.
// ---------------------------------------------------------------------------

/// `infra/lint/a-deleted-manifest-leaves-no-object.sh` is the other
/// reader of this question, and the one a human reads. Before this car
/// it derived its own declared set and sent the parse's stderr to
/// /dev/null, so the fixture above made it print:
///
///     1 live object(s) in the managed namespaces that no manifest declares:
///         Service/svc-a (ns boss)
///
/// — a DECLARED object named as an orphan, under advice to `kubectl
/// delete` it, with no mention anywhere that a manifest would not parse.
/// Exit 1, the same code a genuine orphan produces.
///
/// The lint is run out of a COPY of the tree, because it reads the
/// manifests beside itself; the scripts it runs are the repository's
/// own, so the refusal under test is the real one.
#[test]
fn the_consuming_lint_refuses_rather_than_calling_a_declared_object_an_orphan() {
    let c = Case::new("lint", &[]);
    // The two exemptions the lint expects to find live; absent, it
    // reports them stale and the run fails for an unrelated reason.
    for (kind, ns, name) in [
        ("ConfigMap", "boss", "step-plugins"),
        ("ConfigMap", "boss-dev", "gate-runner-script"),
    ] {
        let f = c.live.join(format!("{kind}.{ns}"));
        let mut body = std::fs::read_to_string(&f).unwrap_or_default();
        body.push_str(&format!("{name}\t\n"));
        std::fs::write(&f, body).unwrap();
    }
    let bin = c.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::copy(&c.kubectl, bin.join("kubectl")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(bin.join("kubectl"), std::fs::Permissions::from_mode(0o755)).unwrap();

    let lint_rel = "infra/lint/a-deleted-manifest-leaves-no-object.sh";
    for rel in [lint_rel, "infra/cluster/undeclared-objects.sh"] {
        let dst = c.tree.join(rel);
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(repo_root().join(rel), &dst).unwrap();
    }
    // The lint's property A walks git history for deleted manifests, so
    // the fixture is a real repository with nothing deleted in it.
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&c.tree)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&["-c", "commit.gpgsign=false", "commit", "-qm", "fixture"]);

    let run = |extra: &[(&str, String)]| -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(c.tree.join(lint_rel))
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_LIVE", &c.live);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("the lint runs");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };

    // The baseline, so the refusal below cannot be "it always fails".
    let (rc, out) = run(&[]);
    assert_eq!(rc, 0, "the lint did not pass a clean fixture:\n{out}");

    // The lint keeps its own sweep (the derivation answers the DECLARED
    // set, not this), so it carries the same obligation for a pair it
    // cannot read: the server's words, not a count.
    let forbid = c.forbid(&[("Service", "boss")]);
    let (rc, out) = run(&[forbid]);
    assert_eq!(
        rc, 0,
        "a partly-readable cluster is still an answer about the part read:\n{out}"
    );
    names_all(
        &out,
        &["UNVERIFIED", "cannot list Service in boss"],
        "the lint's unreadable-pair case",
    );

    let (rc, out) = run(&[c.badparse("svc-a.yaml")]);
    assert_ne!(rc, 0, "the lint reported a partial scrape as clean:\n{out}");
    names_all(
        &out,
        &[
            "svc-a.yaml",
            "parse",
            // The derivation's OWN code, carried through. Reported as
            // `exit 0` by the first draft, which read `$?` inside the
            // `then` of an `if !` — where `!` has already made it 0.
            &format!("exit {CANNOT_ANSWER}"),
        ],
        "the lint's unparseable-manifest case",
    );
    assert!(
        !out.contains("Service/svc-a"),
        "the lint named a DECLARED object as an orphan because its manifest would not parse:\n{out}"
    );
}
