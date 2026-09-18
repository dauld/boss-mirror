//! infra/seed-estate.sh — the launcher's estate declaration (backlog
//! ee368d0c, 2026-09-18): `boss estate declare <estate.toml>` retried
//! while the stack binds, a refusal stopping at once, the output kept.
//! Exercised under a stub `boss`, the way seed-tenant.sh is.
//!
//! Also pins what the image must carry for it: the script and the
//! estate source beside it, since the gate never builds the image and
//! the launcher invokes both by absolute path under /opt/boss/infra.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/seed-estate.sh";

struct Fixture {
    root: PathBuf,
    bin: PathBuf,
    calls: PathBuf,
    source: PathBuf,
}

impl Fixture {
    /// A stub `boss` that records argv and answers `exit_seq` in
    /// order (the last entry repeats): 1 = connection refused, 0 = ok,
    /// 2 = the door's 422 on the line the real verb prints.
    fn new(name: &str, exit_seq: &str) -> Self {
        let root = scratch_dir(&format!("seed-estate-sh-{name}"));
        let bin = root.join("bin");
        create_dir(&bin);
        let calls = root.join("calls");
        let source = root.join("estate.toml");
        write_file(&source, "[[node]]\nid = \"cp-1\"\n");
        write_exec(
            &bin.join("boss"),
            &format!(
                "#!/usr/bin/env bash\n\
                 n=0; [[ -f \"{calls}.n\" ]] && n=$(<\"{calls}.n\"); n=$((n+1)); echo $n >\"{calls}.n\"\n\
                 echo \"boss $*\" >>\"{calls}\"\n\
                 seq=({exit_seq}); i=$((n-1)); [[ $i -ge ${{#seq[@]}} ]] && i=$((${{#seq[@]}}-1))\n\
                 case ${{seq[$i]}} in\n\
                   0) echo \"estate: 1 nodes → POST http://stub/api/estate/nodes/batch\"; echo \"  received 1, inserted 1, roles inserted 0\"; exit 0;;\n\
                   2) echo \"Error: POST http://stub/api/estate/nodes/batch → 422 Unprocessable Entity node cp-1: address is required\" >&2; exit 1;;\n\
                   *) echo \"attempt $n: connection refused\" >&2; exit 1;;\n\
                 esac\n",
                calls = calls.display()
            ),
        );
        Self {
            root,
            bin,
            calls,
            source,
        }
    }

    fn run(&self, attempts: &str, source: Option<&Path>) -> (i32, String) {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .env("PATH", path)
            .env("BOSS_PUBLISH_ATTEMPTS", attempts)
            .env("BOSS_PUBLISH_RETRY_SECONDS", "0")
            .env("BOSS_INFRA_DIR", &self.root);
        match source {
            Some(p) => {
                cmd.env("BOSS_ESTATE_SOURCE", p);
            }
            None => {
                cmd.env("BOSS_ESTATE_SOURCE", &self.source);
            }
        }
        let out = cmd.output().expect("run seed-estate.sh");
        let calls = std::fs::read_to_string(&self.calls).unwrap_or_default();
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{calls}--- stdout\n{}--- stderr\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }
}

#[test]
fn it_retries_until_the_declaration_lands_and_keeps_the_verbs_output() {
    let fx = Fixture::new("retries", "1 1 0");
    let (rc, out) = fx.run("5", None);
    assert_eq!(rc, 0, "{out}");
    let declares = out
        .lines()
        .filter(|l| *l == format!("boss estate declare {}", fx.source.display()))
        .count();
    assert_eq!(declares, 3, "two refusals then success:\n{out}");
    assert!(
        out.contains("received 1, inserted 1"),
        "the verb's own receipt is kept, not discarded:\n{out}"
    );
    assert!(out.contains("estate declared"), "{out}");
}

#[test]
fn a_refused_row_stops_at_once_and_the_refusal_is_printed() {
    let fx = Fixture::new("refused", "2");
    let (rc, out) = fx.run("5", None);
    assert_eq!(rc, 1, "{out}");
    assert_eq!(
        out.lines()
            .filter(|l| l.starts_with("boss estate declare"))
            .count(),
        1,
        "a 422 is not retried — waiting cannot change a file:\n{out}"
    );
    assert!(
        out.contains("REFUSED") && out.contains("address is required"),
        "{out}"
    );
}

#[test]
fn it_gives_up_after_the_budget_naming_the_last_output() {
    let fx = Fixture::new("budget", "1");
    let (rc, out) = fx.run("2", None);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("failed after 2 attempts"), "{out}");
    assert!(
        out.contains("connection refused"),
        "the last output is printed:\n{out}"
    );
}

#[test]
fn a_missing_source_is_a_refusal_naming_the_path_not_an_empty_estate() {
    let fx = Fixture::new("no-source", "0");
    let missing = fx.root.join("nowhere/estate.toml");
    let (rc, out) = fx.run("1", Some(&missing));
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("nowhere/estate.toml"), "{out}");
    assert!(
        !out.contains("boss estate declare"),
        "nothing is sent for a source that is not there:\n{out}"
    );
}

#[test]
fn the_source_defaults_to_the_estate_file_beside_the_infra_dir() {
    let text = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    assert!(
        text.contains("${BOSS_INFRA_DIR:-/opt/boss/infra}/estate/estate.toml"),
        "the default source is the tree's one estate file, under the image's infra dir"
    );
}

#[test]
fn the_image_carries_the_script_and_the_estate_source_beside_it() {
    let df = std::fs::read_to_string(repo_root().join("infra/oss-quickstart/Dockerfile")).unwrap();
    assert!(
        df.contains("COPY infra/seed-estate.sh /opt/boss/infra/seed-estate.sh"),
        "Dockerfile must COPY infra/seed-estate.sh to /opt/boss/infra"
    );
    assert!(
        df.contains("COPY infra/estate/estate.toml /opt/boss/infra/estate/estate.toml"),
        "Dockerfile must COPY the estate source where the script's default reads it"
    );
    assert!(
        df.contains("/opt/boss/infra/seed-estate.sh\n")
            || df.contains("/opt/boss/infra/seed-estate.sh \\"),
        "the chmod list must include /opt/boss/infra/seed-estate.sh"
    );
    let launcher =
        std::fs::read_to_string(repo_root().join("infra/oss-quickstart/tenant-launch.sh")).unwrap();
    assert!(
        launcher.contains("\"$BOSS_INFRA_DIR/seed-estate.sh\" || return $?"),
        "publish_tenant runs the estate declaration and its failure is the verdict"
    );
}
