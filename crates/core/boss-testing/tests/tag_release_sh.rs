//! `infra/forge/tag-release.sh` is RUN, not read — against a REAL bare
//! git repository standing in for the forge (the fixture checkout's
//! `origin`), and a stub `boss-sor-read` whose answer is a file. A tag
//! is a tag the read-back can observe on the remote itself, and a
//! refusal's "nothing written" is measured on the remote and the
//! checkout, not believed.
//!
//! WHY THE VERB EXISTS (backlog 05d301be, left by the cut-a-release
//! tenant car, 2026-09-18): no ops verb writes a git ref on the forge —
//! 31 verbs, none tags — so the tenant's cut-a-release protocol leaves
//! its `tag` step to the founder by hand, the act David's 2026-09-16
//! rule says must not scale. This verb is the door: an annotated tag at
//! a LANDED train's merge commit, pushed to the forge as the checkout's
//! owner, read back, and answered on one line the dispatcher rule
//! complete-release-tag-on-tag-release-answered reads to complete the
//! release packet's `tag` step.
//!
//! What each case pins:
//!
//!   * SUCCESS: an annotated tag whose message carries the release
//!     packet id and the version lands on the forge at exactly the sha
//!     given; the script reads it back with `ls-remote --tags` and the
//!     LAST line of its output is the answer line, in the shape the
//!     rule's `verdict_pattern` reads (the two are held equal here —
//!     one fact, two files, CLAUDE.md §9a).
//!   * EACH REFUSAL is named on stdout, exits 1, and writes nothing —
//!     no tag on the remote, no tag in the checkout: a malformed
//!     version; a tag already on the forge; a sha that is not an
//!     ancestor of the converged main (the checkout's HEAD); a sha no
//!     CLOSED pr-train's `merged` step names as its `merge_ref` — an
//!     open train's is not proof, and an empty listing is a failed
//!     read, not a fact.
//!   * THE VERB FILE serves the forge, is MUTATING with its
//!     authorization named, takes version / sha / release with patterns
//!     that refuse a short sha, a bare number and a short id, and
//!     declares a timeout.

use boss_testing::{dispatcher_rules_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/forge/tag-release.sh";
const RULE: &str = "complete-release-tag-on-tag-release-answered";
const RELEASE: &str = "7d3a9c1e-2b4f-4e6a-9c8d-1f2e3a4b5c6d";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "f@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "f@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// One fixture: a bare "forge" whose main is A -> B (B is "the train
/// that landed") with a side branch S off A that never merged; a
/// checkout cloned from it (HEAD = main = B, `origin` = the forge); a
/// stub `boss-sor-read` that prints the trains listing in a file.
struct Case {
    forge: PathBuf,
    tree: PathBuf,
    sor_read: PathBuf,
    trains: PathBuf,
    a: String,
    b: String,
    s: String,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("tag_release_sh_{name}"));
        let forge = root.join("forge.git");
        let seed = root.join("seed");
        let tree = root.join("tree");
        git(&root, &["init", "-q", "--bare", "-b", "main", "forge.git"]);
        git(&root, &["init", "-q", "-b", "main", "seed"]);
        write_file(&seed.join("a.txt"), "a\n");
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-q", "-m", "a"]);
        let a = git(&seed, &["rev-parse", "HEAD"]);
        git(&seed, &["checkout", "-q", "-b", "side"]);
        write_file(&seed.join("s.txt"), "s\n");
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-q", "-m", "side"]);
        let s = git(&seed, &["rev-parse", "HEAD"]);
        git(&seed, &["checkout", "-q", "main"]);
        write_file(&seed.join("b.txt"), "b\n");
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-q", "-m", "train: 2026-09-18 (#458)"]);
        let b = git(&seed, &["rev-parse", "HEAD"]);
        git(
            &seed,
            &[
                "push",
                "-q",
                forge.to_str().unwrap(),
                "refs/heads/main:refs/heads/main",
                "refs/heads/side:refs/heads/side",
            ],
        );
        git(
            &root,
            &[
                "clone",
                "-q",
                forge.to_str().unwrap(),
                tree.to_str().unwrap(),
            ],
        );
        // The stub reader prints whatever the trains file holds for any
        // path it is asked — the path is recorded so a test can pin it.
        let trains = root.join("trains.json");
        let sor_read = root.join("boss-sor-read");
        write_exec(
            &sor_read,
            &format!(
                "#!/usr/bin/env bash\nprintf '%s\\n' \"$1\" >> '{}'\ncat '{}'\n",
                root.join("sor-read.paths").display(),
                trains.display()
            ),
        );
        let case = Self {
            forge,
            tree,
            sor_read,
            trains,
            a,
            b,
            s,
        };
        case.trains_listing(&[("closed", &case.b.clone())]);
        case
    }

    /// The trains listing the stub answers: one pr-train per entry, of
    /// the given status, whose `merged` step records `merge_ref` = the
    /// TWELVE-char prefix the conductor writes (train/conductor.rs takes
    /// 12 of the forge's merge oid).
    fn trains_listing(&self, trains: &[(&str, &str)]) {
        let data: Vec<serde_json::Value> = trains
            .iter()
            .enumerate()
            .map(|(i, (status, sha))| {
                serde_json::json!({
                    "id": format!("{i:08x}-0000-4000-8000-000000000000"),
                    "kind": "pr-train",
                    "status": status,
                    "steps": [
                        {"spec_slug": "assemble", "status": "completed",
                         "metadata": {"train_ref": format!("train/x@{}", &sha[..7])}},
                        {"spec_slug": "merged", "status": "completed",
                         "metadata": {"merged": "true", "merge_ref": &sha[..12]}}
                    ]
                })
            })
            .collect();
        let total = data.len();
        write_file(
            &self.trains,
            &serde_json::json!({"data": data, "total": total}).to_string(),
        );
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new("bash")
            .arg(repo_root().join(SCRIPT))
            .args(args)
            .env("BOSS_TAG_RELEASE_TREE", &self.tree)
            .env("BOSS_TAG_RELEASE_REMOTE", "origin")
            .env("BOSS_TAG_RELEASE_SOR_READ", &self.sor_read)
            .env("OPS_REQUEST_ID", "0f0f0f0f-0f0f-4f0f-8f0f-0f0f0f0f0f0f")
            .env_remove("BOSS_JOBS_URL")
            .output()
            .expect("the script runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    fn forge_tags(&self) -> String {
        git(&self.forge, &["tag", "-l"])
    }

    fn tree_tags(&self) -> String {
        git(&self.tree, &["tag", "-l"])
    }

    /// Nothing written: no tag on the forge, none in the checkout.
    fn assert_untouched(&self, text: &str) {
        assert_eq!(self.forge_tags(), "", "the forge got a tag:\n{text}");
        assert_eq!(self.tree_tags(), "", "the checkout got a tag:\n{text}");
    }
}

fn verb_file() -> serde_json::Value {
    let path = repo_root().join("infra/ops/verbs/tag-release.json");
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// The rule's `verdict_pattern`, off its file — the regex the handler
/// reads the answer line with.
fn rule_pattern() -> regex::Regex {
    let path = dispatcher_rules_dir().join(format!("{RULE}.toml"));
    let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let args = &doc["rule"][0]["do"][0]["args"];
    // Every arg is a boss-expr source: a double-quoted string literal.
    let src = args["verdict_pattern"].as_str().expect("verdict_pattern");
    let inner = src
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or_else(|| panic!("verdict_pattern is not an expr string literal: {src}"));
    regex::Regex::new(inner).unwrap_or_else(|e| panic!("{RULE}'s verdict_pattern: {e}"))
}

#[test]
fn a_landed_sha_gets_an_annotated_tag_pushed_read_back_and_answered() {
    let c = Case::new("success");
    let (code, text) = c.run(&["v1.2.3", &c.b, RELEASE]);
    assert_eq!(code, 0, "{text}");

    // On the forge, at the sha, annotated, carrying packet and version.
    assert_eq!(c.forge_tags(), "v1.2.3");
    assert_eq!(git(&c.forge, &["rev-parse", "v1.2.3^{commit}"]), c.b);
    assert_eq!(git(&c.forge, &["cat-file", "-t", "v1.2.3"]), "tag");
    let msg = git(&c.forge, &["tag", "-l", "--format=%(contents)", "v1.2.3"]);
    assert!(
        msg.contains(RELEASE),
        "the tag message names the release packet:\n{msg}"
    );
    assert!(
        msg.contains("v1.2.3"),
        "the tag message names the version:\n{msg}"
    );

    // The answer line is LAST, and it is what the rule reads.
    let last = text.lines().last().unwrap_or("");
    assert_eq!(
        last,
        format!("tag-release: v1.2.3 at {} (packet {RELEASE})", c.b)
    );
    let caps = rule_pattern().captures(last).unwrap_or_else(|| {
        panic!("{RULE}'s verdict_pattern does not read the answer line: {last}")
    });
    assert_eq!(&caps["tag"], "v1.2.3");
    assert_eq!(&caps["sha"], c.b);
    // …and not the refusal shape, which shares the prefix.
    assert!(
        rule_pattern()
            .captures("tag-release: REFUSED — version 'x' is malformed")
            .is_none()
    );
}

#[test]
fn a_malformed_version_is_refused_before_anything_is_read() {
    let c = Case::new("malformed_version");
    for bad in ["1.2.3", "v1.2", "v1.2.3-rc1", "latest"] {
        let (code, text) = c.run(&[bad, &c.b, RELEASE]);
        assert_eq!(code, 1, "{bad}: {text}");
        assert!(text.contains("REFUSED"), "{bad}: {text}");
        assert!(
            text.contains("version"),
            "{bad}: the refusal names the version:\n{text}"
        );
        c.assert_untouched(&text);
    }
    // A short sha and a short packet id are refused the same way — the
    // allowlist refuses them first, and the script does not rely on it.
    let (code, text) = c.run(&["v1.2.3", &c.b[..12], RELEASE]);
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("REFUSED") && text.contains("sha"), "{text}");
    let (code, text) = c.run(&["v1.2.3", &c.b, "7d3a9c1e"]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("REFUSED") && text.contains("packet"),
        "{text}"
    );
    c.assert_untouched(&text);
}

#[test]
fn a_tag_already_on_the_forge_is_refused_and_left_alone() {
    let c = Case::new("existing_tag");
    // v1.2.3 already on the forge, at A — a release someone cut.
    git(&c.tree, &["tag", "-a", "v1.2.3", &c.a, "-m", "earlier"]);
    git(&c.tree, &["push", "-q", "origin", "refs/tags/v1.2.3"]);
    git(&c.tree, &["tag", "-d", "v1.2.3"]);
    let (code, text) = c.run(&["v1.2.3", &c.b, RELEASE]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("REFUSED") && text.contains("already"),
        "{text}"
    );
    assert_eq!(
        git(&c.forge, &["rev-parse", "v1.2.3^{commit}"]),
        c.a,
        "the existing tag still points where it did"
    );
    assert_eq!(c.tree_tags(), "", "the checkout got a tag:\n{text}");
}

#[test]
fn a_sha_that_is_not_an_ancestor_of_the_converged_main_is_refused() {
    let c = Case::new("not_ancestor");
    // The listing vouches for S as a merge (a lying record), so the
    // ancestry check is the one that refuses.
    c.trains_listing(&[("closed", &c.s.clone())]);
    let (code, text) = c.run(&["v1.2.3", &c.s, RELEASE]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("REFUSED") && text.contains("ancestor"),
        "{text}"
    );
    c.assert_untouched(&text);
}

#[test]
fn a_sha_no_closed_train_merged_is_refused() {
    let c = Case::new("not_a_train");
    // A is an ancestor of main but the only closed train merged B.
    let (code, text) = c.run(&["v1.2.3", &c.a, RELEASE]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("REFUSED") && text.contains("pr-train"),
        "{text}"
    );
    c.assert_untouched(&text);

    // An OPEN train naming A is not proof it landed.
    c.trains_listing(&[("open", &c.a.clone()), ("closed", &c.b.clone())]);
    let (code, text) = c.run(&["v1.2.3", &c.a, RELEASE]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("REFUSED") && text.contains("pr-train"),
        "{text}"
    );
    c.assert_untouched(&text);

    // An empty listing is a failed read (an unidentified reader is
    // handed a smaller world), never "no train exists".
    c.trains_listing(&[]);
    let (code, text) = c.run(&["v1.2.3", &c.b, RELEASE]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("REFUSED") && text.contains("listed no"),
        "{text}"
    );
    c.assert_untouched(&text);

    // The listing the script asks for: CLOSED trains, newest first.
    let asked = std::fs::read_to_string(c.trains.parent().unwrap().join("sor-read.paths")).unwrap();
    assert!(
        asked
            .lines()
            .all(|p| p.starts_with("/api/jobs?kind=pr-train&status=closed&limit=")),
        "every read is the closed-trains listing:\n{asked}"
    );
}

#[test]
fn the_verb_file_serves_the_forge_mutates_and_takes_three_bounded_params() {
    let v = verb_file();
    assert_eq!(v["hosts"], serde_json::json!(["forge"]));
    assert_eq!(v["argv"], serde_json::json!([SCRIPT, "{1}", "{2}", "{3}"]));
    let about = v["about"].as_str().unwrap();
    assert!(
        about.contains("MUTATING"),
        "a verb that writes a ref says so"
    );
    assert!(!about.contains("READ-ONLY"));
    assert!(about.contains("David"), "the authorization is named");
    let params = v["params"].as_array().unwrap();
    assert_eq!(params.len(), 3);
    let pat = |i: usize, name: &str| {
        assert_eq!(params[i]["name"], name);
        assert!(params[i].get("optional").is_none(), "{name} is required");
        regex::Regex::new(params[i]["pattern"].as_str().unwrap()).unwrap()
    };
    let version = pat(0, "version");
    assert!(version.is_match("v1.2.3") && version.is_match("v10.0.42"));
    assert!(
        !version.is_match("1.2.3") && !version.is_match("v1.2") && !version.is_match("v1.2.3-rc1")
    );
    let sha = pat(1, "sha");
    assert!(
        sha.is_match(
            &"0123456789abcdef"
                .repeat(3)
                .chars()
                .take(40)
                .collect::<String>()
        )
    );
    assert!(
        !sha.is_match("2683908"),
        "a short sha cannot name a release"
    );
    let release = pat(2, "release");
    assert!(release.is_match(RELEASE));
    assert!(
        !release.is_match("7d3a9c1e"),
        "a short id cannot name a packet"
    );
    assert!(
        v["timeout"].as_i64().is_some_and(|t| t >= 60),
        "a push to the forge and a read-back declare their own timeout"
    );
}
