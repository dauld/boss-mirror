//! infra/seed-tenant.sh — the launcher's generic tenant publish:
//! `boss tenant publish <dir> [--take <registries>]` retried while the
//! stack binds, a REFUSED verdict stopping at once, the output kept.
//! Exercised under a stub `boss`, the way seed-estate.sh is.
//!
//! The seam this pins is one word. The script cannot tell a stack
//! still binding from a flag that will never parse except by the verb
//! saying `REFUSED`, so a `--take` naming an unknown registry — whose
//! refusal lacked the word — was retried 30 x 5 s, 150 s to learn a
//! typo (backlog 6ad63e09, 2026-09-18). The verb's side of the seam is
//! pinned in boss-cli (`Take::parse`); this is the script's side.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec};
use std::path::PathBuf;
use std::process::Command;

const SCRIPT: &str = "infra/seed-tenant.sh";

struct Fixture {
    bin: PathBuf,
    calls: PathBuf,
    tenant: PathBuf,
}

impl Fixture {
    /// A stub `boss` that records argv and answers `exit_seq` in
    /// order (the last entry repeats): 1 = connection refused, 0 = ok,
    /// 2 = the take refusal on the line the real verb prints.
    fn new(name: &str, exit_seq: &str) -> Self {
        let root = scratch_dir(&format!("seed-tenant-sh-{name}"));
        let bin = root.join("bin");
        create_dir(&bin);
        let calls = root.join("calls");
        let tenant = root.join("tenant");
        create_dir(&tenant);
        write_exec(
            &bin.join("boss"),
            &format!(
                "#!/usr/bin/env bash\n\
                 n=0; [[ -f \"{calls}.n\" ]] && n=$(<\"{calls}.n\"); n=$((n+1)); echo $n >\"{calls}.n\"\n\
                 echo \"boss $*\" >>\"{calls}\"\n\
                 seq=({exit_seq}); i=$((n-1)); [[ $i -ge ${{#seq[@]}} ]] && i=$((${{#seq[@]}}-1))\n\
                 case ${{seq[$i]}} in\n\
                   0) echo \"tenant acme: 3 steps, 0 refused\"; exit 0;;\n\
                   2) echo \"Error: REFUSED: --take nope: no door overwrites that registry; the registries a take can name are classes, calendars\" >&2; exit 1;;\n\
                   *) echo \"attempt $n: connection refused\" >&2; exit 1;;\n\
                 esac\n",
                calls = calls.display()
            ),
        );
        Self { bin, calls, tenant }
    }

    fn run(&self, attempts: &str, take: Option<&str>) -> (i32, String) {
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
            .env("BOSS_TENANT_DIR", &self.tenant)
            .env_remove("BOSS_TENANT_TAKE");
        if let Some(take) = take {
            cmd.env("BOSS_TENANT_TAKE", take);
        }
        let out = cmd.output().expect("run seed-tenant.sh");
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

fn publish_calls(out: &str) -> usize {
    out.lines()
        .filter(|l| l.starts_with("boss tenant publish"))
        .count()
}

#[test]
fn it_retries_until_the_publish_lands_and_hands_the_take_down() {
    let fx = Fixture::new("retries", "1 1 0");
    let (rc, out) = fx.run("5", Some("agents,workflows"));
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        publish_calls(&out),
        3,
        "two refused connections, then the publish:\n{out}"
    );
    assert!(
        out.contains("--take agents,workflows"),
        "the take rides the argv:\n{out}"
    );
    assert!(out.contains("tenant published"), "{out}");
}

#[test]
fn an_unknown_take_registry_is_refused_at_once_and_not_retried() {
    let fx = Fixture::new("bad-take", "2");
    let (rc, out) = fx.run("5", Some("nope"));
    assert_eq!(rc, 1, "{out}");
    assert_eq!(
        publish_calls(&out),
        1,
        "a REFUSED take is not retried — waiting cannot change a flag:\n{out}"
    );
    assert!(
        out.contains("not retrying") && out.contains("--take nope"),
        "the refusal is printed with the verb's reason:\n{out}"
    );
}
