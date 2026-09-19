//! `infra/ops/unit-cat.sh` prints a unit the way systemd holds it, with
//! the VALUE of every secret-shaped Environment assignment masked and
//! the name kept; refuses a malformed name; says "no such unit" as an
//! exit 4, not as an empty answer.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../infra/ops/unit-cat.sh")
}

/// A stub `systemctl` that prints a fixture unit for `known.service`
/// and systemd's own refusal for anything else.
fn with_stub_systemctl(case: &str) -> String {
    let bin = boss_testing::scratch_dir(&format!("unit-cat-{case}")).join("bin");
    fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("systemctl");
    boss_testing::write_exec(
        &stub,
        r#"#!/usr/bin/env bash
unit="${@: -1}"
if [ "$unit" = "known.service" ]; then
cat <<'U'
# /etc/systemd/system/known.service
[Service]
User=david
WorkingDirectory=/var/lib/known
Environment=FORGE_TOKEN=abc123 PLAIN=visible
Environment="REGISTRY_PASSWORD=hunter2"
Environment=BOSS_JOBS_URL=http://10.20.0.34:7900
ExecStart=/usr/local/bin/known daemon
U
exit 0
fi
if [ "$unit" = "tunnel.service" ]; then
cat <<'U'
# /etc/systemd/system/tunnel.service
[Service]
ExecStart=/usr/bin/cloudflared --no-autoupdate tunnel run --token eyJhIjoiYWJjZGVmMDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODkiLCJ0IjoiZmVkY2JhOTg3NjU0MzIxMGZlZGNiYTk4NzY1NDMyMTAiLCJzIjoiWkdWaFpHSmxaV1ptWldWa1ltVmxabVpsWldSaVpXVm1abVZsWkE9PSJ9
ExecStartPost=/usr/bin/other --api-key=sk-live-000111222333 --password hunter3 --verbose --name plain-name
U
exit 0
fi
echo "No files found for $unit." >&2
exit 1
"#,
    );
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn run(path: &str, unit: &str) -> std::process::Output {
    Command::new("bash")
        .arg(script())
        .arg(unit)
        .env("PATH", path)
        .output()
        .expect("bash runs")
}

#[test]
fn the_unit_is_printed_with_secret_values_masked_and_names_kept() {
    let path = with_stub_systemctl("masked");
    let out = run(&path, "known.service");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("User=david"), "{text}");
    assert!(text.contains("WorkingDirectory=/var/lib/known"), "{text}");
    assert!(
        text.contains("ExecStart=/usr/local/bin/known daemon"),
        "{text}"
    );
    assert!(
        text.contains("BOSS_JOBS_URL=http://10.20.0.34:7900"),
        "a plain URL is not a secret: {text}"
    );
    assert!(text.contains("PLAIN=visible"), "{text}");
    assert!(!text.contains("abc123"), "the token value leaked: {text}");
    assert!(
        !text.contains("hunter2"),
        "the password value leaked: {text}"
    );
    assert!(
        text.contains("FORGE_TOKEN=<masked by unit-cat>"),
        "the name must stay: {text}"
    );
    assert!(
        text.contains("REGISTRY_PASSWORD=<masked by unit-cat>"),
        "{text}"
    );
}

/// A secret passed as an ARGV FLAG is masked the same way an
/// Environment assignment is. On 2026-09-16 the operator read
/// boss-gcp's cloudflared.service through the door and the packet
/// (ops-request 0117de08) carried the tunnel token — `--token <v>` is
/// not `TOKEN=`, and the mask only knew the latter (backlog 9c760dd7).
/// The flag name stays; its value goes; a flag that is not a secret
/// keeps its value.
#[test]
fn a_secret_passed_as_a_flag_is_masked_and_the_flag_name_kept() {
    let path = with_stub_systemctl("flag");
    let out = run(&path, "tunnel.service");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("eyJhIjoi"),
        "the tunnel token leaked: {text}"
    );
    assert!(
        text.contains("tunnel run --token <masked by unit-cat>"),
        "the flag name must stay: {text}"
    );
    assert!(
        !text.contains("sk-live-000111222333"),
        "the api key leaked: {text}"
    );
    assert!(text.contains("--api-key=<masked by unit-cat>"), "{text}");
    assert!(!text.contains("hunter3"), "the password leaked: {text}");
    assert!(text.contains("--password <masked by unit-cat>"), "{text}");
    assert!(
        text.contains("--verbose --name plain-name"),
        "a flag that is not a secret keeps its value: {text}"
    );
}

#[test]
fn an_unknown_unit_is_exit_4_not_an_empty_answer() {
    let path = with_stub_systemctl("unknown");
    let out = run(&path, "nope.service");
    assert_eq!(
        out.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("No files found"));
}

#[test]
fn a_malformed_unit_name_is_refused_before_systemctl_runs() {
    let path = with_stub_systemctl("malformed");
    let out = run(&path, "x; rm -rf /");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
}
