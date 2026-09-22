//! `infra/cluster/dns/check-declared.sh` is RUN, not read — against
//! synthetic `GET /zones/{id}/dns_records` bodies shaped from the zone
//! as measured on 2026-09-16 (backlog 5e58922c: boss.algedonic.dev A
//! 10.20.0.33 grey-cloud, playground.algedonic.dev a proxied CNAME to
//! the tunnel) and as declared since 198c5fe9 (boss. a proxied CNAME to
//! the same tunnel, behind an `interlock = "access"` the dns.observe
//! handler honours), so every verdict below is one the comparator
//! actually reached. Nothing here touches Cloudflare or needs a credential: the
//! live records are INPUT, and the tunnel reference the declaration
//! makes is resolved from a fixture credentials file or a `--tunnel`
//! argument, the two ways the operator and the `dns.observe` handler
//! hand it in.
//!
//! WHY THIS FILE EXISTS. Until this car every record of the zone was
//! hand-set in the Cloudflare UI and nothing recorded what the zone
//! should say. `infra/cluster/dns/algedonic.dev.toml` is now the
//! declaration and the comparator reads the live zone against it in the
//! vocabulary design 16115a17 decided: MATCH / DRIFT (both values
//! printed) / ABSENT / UNDECLARED — the last reported, never a failure,
//! because the first honest read of a zone nobody measured whole is the
//! list of what it holds that nobody declared.

use boss_testing::{feed_stdin, repo_root};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(case: &str) -> PathBuf {
    boss_testing::scratch_dir(&format!("dns-check-declared-{case}"))
}

fn script() -> PathBuf {
    repo_root().join("infra/cluster/dns/check-declared.sh")
}

/// The tunnel id the fixture Secret names — the shape cloudflared's
/// credentials.json carries, with a secret that is NOT the real one.
const TUNNEL_ID: &str = "d8a8ef3b-0a6e-4a05-839d-1d910f01fef6";
const CREDENTIAL: &str = "cloudflare-tunnel-credentials";

fn credentials_json() -> String {
    format!(
        r#"{{"AccountTag":"acct-fixture","TunnelSecret":"Zml4dHVyZS1ub3QtYS1yZWFsLXNlY3JldC0wMDAwMDAwMDA=","TunnelID":"{TUNNEL_ID}"}}"#
    )
}

/// One live record as the Cloudflare v4 API lists it (the fields the
/// comparator reads plus the noise a real row carries).
fn record(name: &str, rtype: &str, content: &str, proxied: bool, ttl: u32) -> serde_json::Value {
    serde_json::json!({
        "id": format!("rec-{name}-{rtype}"),
        "zone_id": "zone-1",
        "zone_name": "algedonic.dev",
        "name": name,
        "type": rtype,
        "content": content,
        "proxied": proxied,
        "proxiable": true,
        "ttl": ttl,
        "comment": null,
        "tags": [],
        "created_on": "2026-09-16T07:44:00.000000Z",
        "modified_on": "2026-09-16T07:44:00.000000Z",
    })
}

/// The zone exactly as measured on 2026-09-16 — BEFORE the flip: boss.
/// still the grey A record to the LAN ingress.
fn as_measured() -> Vec<serde_json::Value> {
    vec![
        record("boss.algedonic.dev", "A", "10.20.0.33", false, 300),
        record(
            "playground.algedonic.dev",
            "CNAME",
            &format!("{TUNNEL_ID}.cfargotunnel.com"),
            true,
            1,
        ),
        // The IdP already on this tunnel — the zone after fd75c641 moved
        // it; last, so the positional fixtures keep their meaning.
        record(
            "id.algedonic.dev",
            "CNAME",
            &format!("{TUNNEL_ID}.cfargotunnel.com"),
            true,
            1,
        ),
    ]
}

/// The zone as the declaration says it should be since 198c5fe9: both
/// hostnames proxied CNAMEs to the tunnel — what the first observation
/// after the flip reads.
fn as_declared() -> Vec<serde_json::Value> {
    vec![
        record(
            "boss.algedonic.dev",
            "CNAME",
            &format!("{TUNNEL_ID}.cfargotunnel.com"),
            true,
            1,
        ),
        record(
            "playground.algedonic.dev",
            "CNAME",
            &format!("{TUNNEL_ID}.cfargotunnel.com"),
            true,
            1,
        ),
        // The IdP (fd75c641) — after the two doors, so the positional
        // fixtures above it keep their meaning.
        record(
            "id.algedonic.dev",
            "CNAME",
            &format!("{TUNNEL_ID}.cfargotunnel.com"),
            true,
            1,
        ),
        // The company website (b64c4377) — after them, same reason.
        record(
            "www.algedonic.dev",
            "CNAME",
            &format!("{TUNNEL_ID}.cfargotunnel.com"),
            true,
            1,
        ),
        // The dev workspace's ssh door (5fc71f03) — last, for the same
        // reason: the positional fixtures above it keep their meaning.
        record(
            "dev.algedonic.dev",
            "CNAME",
            &format!("{TUNNEL_ID}.cfargotunnel.com"),
            true,
            1,
        ),
    ]
}

/// The declared records, as (name, type), sorted — what the shipped
/// declaration must name exactly.
const DECLARED: &[(&str, &str)] = &[
    ("boss.algedonic.dev", "CNAME"),
    ("dev.algedonic.dev", "CNAME"),
    ("id.algedonic.dev", "CNAME"),
    ("playground.algedonic.dev", "CNAME"),
    ("www.algedonic.dev", "CNAME"),
];

/// The body `GET /zones/{id}/dns_records` answers: the v4 envelope.
fn envelope(records: &[serde_json::Value]) -> String {
    serde_json::json!({
        "result": records,
        "success": true,
        "errors": [],
        "messages": [],
        "result_info": {"page": 1, "per_page": 100, "count": records.len(), "total_count": records.len()},
    })
    .to_string()
}

struct Run {
    args: Vec<String>,
    stdin: String,
    declarations: Option<PathBuf>,
    path_env: Option<PathBuf>,
}

impl Run {
    fn new(args: &[&str], stdin: String) -> Self {
        Self {
            args: args.iter().map(|s| s.to_string()).collect(),
            stdin,
            declarations: None,
            path_env: None,
        }
    }
    fn declarations(mut self, dir: &Path) -> Self {
        self.declarations = Some(dir.to_path_buf());
        self
    }
    fn path_env(mut self, dir: &Path) -> Self {
        self.path_env = Some(dir.to_path_buf());
        self
    }
    fn go(self) -> Output {
        use std::process::Stdio;
        // `/bin/bash` by absolute path: the no-python case empties PATH,
        // and the shell must still be found for the script to refuse in.
        let mut cmd = Command::new("/bin/bash");
        cmd.arg(script())
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(d) = &self.declarations {
            cmd.env("BOSS_DNS_DECLARATIONS", d);
        }
        if let Some(p) = &self.path_env {
            cmd.env("PATH", p);
        }
        let mut child = cmd.spawn().expect("spawn check-declared.sh");
        // A refusal exits before it reads stdin; the closed pipe is its
        // verdict, read from the exit status (boss_testing::feed_stdin).
        feed_stdin(&mut child, self.stdin.as_bytes());
        child.wait_with_output().unwrap()
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn lines_with(out: &Output, verdict: &str) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with(verdict))
        .map(str::to_string)
        .collect()
}

/// A scratch dir holding the fixture credentials file; returns the
/// `--tunnel-credentials` argument that names it.
fn credentials_arg(case: &str) -> String {
    let dir = scratch(case);
    let path = dir.join("credentials.json");
    boss_testing::write_file(&path, &credentials_json());
    format!("{CREDENTIAL}={}", path.display())
}

// ---- the shipped declaration ----------------------------------------

/// The declaration puts boss. behind the tunnel — a proxied CNAME BY
/// REFERENCE, never a hardcoded tunnel uuid (which changes on every
/// rotation) — and marks it `interlock = "access"`, the key the
/// dns.observe handler honours: applied only once the Access
/// application access.toml declares for the name reads present with an
/// allow policy (198c5fe9). playground. carries no interlock: the
/// rotation handler owns that record.
#[test]
fn the_shipped_declaration_puts_boss_behind_the_tunnel_behind_an_interlock() {
    let toml = std::fs::read_to_string(repo_root().join("infra/cluster/dns/algedonic.dev.toml"))
        .expect("the declaration exists");
    let doc: toml::Value = toml::from_str(&toml).expect("the declaration parses as TOML");
    assert_eq!(doc["zone"].as_str(), Some("algedonic.dev"));
    let records = doc["record"].as_array().expect("[[record]] entries");
    let mut names: Vec<(String, String)> = records
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap().to_string(),
                r["type"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        DECLARED
            .iter()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect::<Vec<_>>(),
        "exactly the two doors, the identity provider (fd75c641, 2026-09-16), the company \
         website (b64c4377, 2026-09-17) and the dev workspace's ssh door (5fc71f03, \
         2026-09-18): the apex is absent from the zone (measured 2026-09-16) and a record \
         declared before it exists reads ABSENT on every observation — which is why www rides \
         the Access interlock, and dev. the tunnel one, rather than being declared blind"
    );
    let idp = records
        .iter()
        .find(|r| r["name"].as_str() == Some("id.algedonic.dev"))
        .unwrap();
    assert_eq!(
        idp["interlock"].as_str(),
        Some("tunnel"),
        "the IdP follows the tunnel: applied once the converge routes it, never blind"
    );
    assert_eq!(
        idp["target"].as_str(),
        Some("tunnel:cloudflare-tunnel-credentials")
    );
    for r in records {
        for key in ["name", "type", "target", "proxied", "ttl", "why"] {
            assert!(
                r.get(key).is_some(),
                "record {} lacks `{key}` — every record carries type/target/proxied/ttl/why (design 4c565f8c)",
                r["name"]
            );
        }
        assert!(
            !r["why"].as_str().unwrap_or("").trim().is_empty(),
            "record {} has an empty why",
            r["name"]
        );
        assert_eq!(
            r["target"].as_str(),
            Some(&*format!("tunnel:{CREDENTIAL}")),
            "{}: the tunnel is declared by reference to its credential row, never by uuid",
            r["name"]
        );
        assert_eq!(r["proxied"].as_bool(), Some(true), "{}", r["name"]);
        assert_eq!(
            r["ttl"].as_integer(),
            Some(1),
            "{}: a proxied record's TTL is auto",
            r["name"]
        );
    }
    let boss = records
        .iter()
        .find(|r| r["name"].as_str() == Some("boss.algedonic.dev"))
        .unwrap();
    assert_eq!(
        boss["interlock"].as_str(),
        Some("access"),
        "boss. is applied only behind the Access interlock — never exposed without Access in front"
    );
    let playground = records
        .iter()
        .find(|r| r["name"].as_str() == Some("playground.algedonic.dev"))
        .unwrap();
    assert!(
        playground.get("interlock").is_none(),
        "playground. is the rotation handler's record: compared, never applied by the observer"
    );
    // The company website (b64c4377): behind Access until the public
    // launch, so its record is applied only once the application
    // access.toml declares for it reads present — the boss. shape.
    let www = records
        .iter()
        .find(|r| r["name"].as_str() == Some("www.algedonic.dev"))
        .expect("www is declared");
    assert_eq!(
        www["interlock"].as_str(),
        Some("access"),
        "www is applied only behind the Access interlock — never exposed without Access in front"
    );
    assert!(
        !toml
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .any(|l| l.contains("cfargotunnel.com")),
        "a literal <uuid>.cfargotunnel.com in the declaration drifts on the next rotation"
    );
    assert!(
        toml.contains("10.20.0.33") && toml.contains("HISTORY"),
        "the A record's why survives as history, so the reader can tell why the door was grey"
    );
}

// ---- verdicts -------------------------------------------------------

#[test]
fn the_zone_as_declared_matches_and_exits_0() {
    let out = Run::new(
        &[
            "algedonic.dev",
            "--tunnel-credentials",
            &credentials_arg("as-declared"),
        ],
        envelope(&as_declared()),
    )
    .go();
    let t = text(&out);
    assert_eq!(code(&out), 0, "{t}");
    let matches = lines_with(&out, "MATCH");
    assert_eq!(matches.len(), DECLARED.len(), "{t}");
    assert!(
        matches
            .iter()
            .any(|l| l.contains("boss.algedonic.dev CNAME")),
        "{t}"
    );
    assert!(
        matches
            .iter()
            .any(|l| l.contains("www.algedonic.dev CNAME")),
        "{t}"
    );
    assert!(
        matches
            .iter()
            .any(|l| l.contains("playground.algedonic.dev CNAME")),
        "{t}"
    );
    assert!(lines_with(&out, "DRIFT").is_empty(), "{t}");
    assert!(lines_with(&out, "ABSENT").is_empty(), "{t}");
    assert!(lines_with(&out, "UNDECLARED").is_empty(), "{t}");
    assert!(
        t.contains("check-declared: algedonic.dev: 5 match, 0 drift, 0 absent, 0 undeclared — every declared record matches"),
        "{t}"
    );
}

/// What the first observation after 198c5fe9 lands reads, BEFORE the
/// handler applies anything: the declared CNAME is ABSENT and the old A
/// record is UNDECLARED — two verdicts on one name, the honest shape of
/// a flip that has not happened yet. The handler turns the ABSENT into
/// an apply (or a HELD) off this reading.
#[test]
fn the_zone_as_measured_before_the_flip_reads_the_cname_absent_and_the_a_undeclared() {
    let out = Run::new(
        &[
            "algedonic.dev",
            "--tunnel",
            &format!("{CREDENTIAL}={TUNNEL_ID}"),
        ],
        envelope(&as_measured()),
    )
    .go();
    let t = text(&out);
    assert_eq!(code(&out), 1, "{t}");
    let absent = lines_with(&out, "ABSENT");
    // boss. (the flip), www (b64c4377) and dev. (5fc71f03) — each
    // declared before the zone holds it, the honest reading until its
    // own interlock applies it.
    assert_eq!(absent.len(), 3, "{t}");
    let boss = absent
        .iter()
        .find(|l| l.contains("boss.algedonic.dev CNAME"))
        .unwrap_or_else(|| panic!("{t}"));
    assert!(
        boss.contains(&format!("{TUNNEL_ID}.cfargotunnel.com")),
        "{boss}"
    );
    assert!(
        absent.iter().any(|l| l.contains("www.algedonic.dev CNAME")),
        "{t}"
    );
    let undeclared = lines_with(&out, "UNDECLARED");
    assert_eq!(undeclared.len(), 1, "{t}");
    assert!(
        undeclared[0].contains("boss.algedonic.dev A") && undeclared[0].contains("10.20.0.33"),
        "{}",
        undeclared[0]
    );
    assert!(
        t.contains("2 match, 0 drift, 3 absent, 1 undeclared"),
        "{t}"
    );
}

/// The handler resolves the reference itself (the Secret's TunnelID,
/// never the secret) and hands the comparator the uuid.
#[test]
fn a_tunnel_reference_resolves_from_a_bare_tunnel_argument_too() {
    let out = Run::new(
        &[
            "algedonic.dev",
            "--tunnel",
            &format!("{CREDENTIAL}={TUNNEL_ID}"),
        ],
        envelope(&as_declared()),
    )
    .go();
    assert_eq!(code(&out), 0, "{}", text(&out));
    assert_eq!(
        lines_with(&out, "MATCH").len(),
        DECLARED.len(),
        "{}",
        text(&out)
    );
}

#[test]
fn a_record_nobody_declared_is_undeclared_reported_and_not_a_failure() {
    let mut live = as_declared();
    live.push(record("id.algedonic.dev", "A", "203.0.113.7", false, 300));
    live.push(record("algedonic.dev", "MX", "mail.example.net", false, 1));
    let out = Run::new(
        &[
            "algedonic.dev",
            "--tunnel",
            &format!("{CREDENTIAL}={TUNNEL_ID}"),
        ],
        envelope(&live),
    )
    .go();
    let t = text(&out);
    assert_eq!(code(&out), 0, "UNDECLARED alone is exit 0: {t}");
    let undeclared = lines_with(&out, "UNDECLARED");
    assert_eq!(undeclared.len(), 2, "{t}");
    let id = undeclared
        .iter()
        .find(|l| l.contains("id.algedonic.dev A"))
        .unwrap_or_else(|| panic!("{t}"));
    assert!(
        id.contains("203.0.113.7") && id.contains("live, no declaration names it"),
        "an undeclared record is shown with its values, so the reader can declare it: {id}"
    );
    assert!(
        undeclared.iter().any(|l| l.contains("algedonic.dev MX")),
        "{t}"
    );
    assert!(
        t.contains("5 match, 0 drift, 0 absent, 2 undeclared"),
        "{t}"
    );
}

#[test]
fn a_cname_still_pointing_at_the_old_tunnel_is_drift_with_both_values_and_exits_1() {
    let mut live = as_declared();
    live[1] = record(
        "playground.algedonic.dev",
        "CNAME",
        "00000000-1111-4222-8333-444444444444.cfargotunnel.com",
        true,
        1,
    );
    let out = Run::new(
        &[
            "algedonic.dev",
            "--tunnel",
            &format!("{CREDENTIAL}={TUNNEL_ID}"),
        ],
        envelope(&live),
    )
    .go();
    let t = text(&out);
    assert_eq!(code(&out), 1, "{t}");
    let drift = lines_with(&out, "DRIFT");
    assert_eq!(drift.len(), 1, "{t}");
    assert!(
        drift[0].contains(&format!("{TUNNEL_ID}.cfargotunnel.com"))
            && drift[0].contains("00000000-1111-4222-8333-444444444444.cfargotunnel.com"),
        "a DRIFT line prints BOTH values: {}",
        drift[0]
    );
    assert!(
        t.contains("4 match, 1 drift, 0 absent, 0 undeclared"),
        "{t}"
    );
    assert!(t.contains("1 finding(s) against the declaration"), "{t}");
}

#[test]
fn the_boss_door_turned_grey_is_drift_on_proxied_not_content() {
    let mut live = as_declared();
    live[0] = record(
        "boss.algedonic.dev",
        "CNAME",
        &format!("{TUNNEL_ID}.cfargotunnel.com"),
        false,
        1,
    );
    let out = Run::new(
        &[
            "algedonic.dev",
            "--tunnel",
            &format!("{CREDENTIAL}={TUNNEL_ID}"),
        ],
        envelope(&live),
    )
    .go();
    let t = text(&out);
    assert_eq!(code(&out), 1, "{t}");
    let drift = lines_with(&out, "DRIFT");
    assert_eq!(drift.len(), 1, "{t}");
    assert!(
        drift[0].contains("boss.algedonic.dev CNAME")
            && drift[0].contains("proxied: false")
            && drift[0].contains("proxied: true"),
        "{}",
        drift[0]
    );
}

#[test]
fn a_declared_record_the_zone_lacks_is_absent_and_exits_1() {
    // Everything but boss.: playground and the IdP present, boss. absent.
    let live: Vec<serde_json::Value> = as_declared().into_iter().skip(1).collect();
    let out = Run::new(
        &[
            "algedonic.dev",
            "--tunnel",
            &format!("{CREDENTIAL}={TUNNEL_ID}"),
        ],
        envelope(&live),
    )
    .go();
    let t = text(&out);
    assert_eq!(code(&out), 1, "{t}");
    let absent = lines_with(&out, "ABSENT");
    assert_eq!(absent.len(), 1, "{t}");
    assert!(
        absent[0].contains("boss.algedonic.dev CNAME")
            && absent[0].contains(&format!("{TUNNEL_ID}.cfargotunnel.com")),
        "an ABSENT line shows what was declared: {}",
        absent[0]
    );
    assert!(
        t.contains("4 match, 0 drift, 1 absent, 0 undeclared"),
        "{t}"
    );
}

/// A bare result array is accepted as well as the v4 envelope, so a
/// `jq .result` on the way in does not change the answer.
#[test]
fn a_bare_result_array_reads_the_same_as_the_envelope() {
    let out = Run::new(
        &[
            "algedonic.dev",
            "--tunnel",
            &format!("{CREDENTIAL}={TUNNEL_ID}"),
        ],
        serde_json::Value::Array(as_declared()).to_string(),
    )
    .go();
    assert_eq!(code(&out), 0, "{}", text(&out));
    assert_eq!(
        lines_with(&out, "MATCH").len(),
        DECLARED.len(),
        "{}",
        text(&out)
    );
}

// ---- the machine-readable form and the reference listing -----------

#[test]
fn json_output_carries_one_verdict_per_record_and_the_counts() {
    let mut live = as_declared();
    live.push(record("id.algedonic.dev", "A", "203.0.113.7", false, 300));
    live[0] = record(
        "boss.algedonic.dev",
        "CNAME",
        "00000000-1111-4222-8333-444444444444.cfargotunnel.com",
        true,
        1,
    );
    let out = Run::new(
        &[
            "algedonic.dev",
            "--json",
            "--tunnel",
            &format!("{CREDENTIAL}={TUNNEL_ID}"),
        ],
        envelope(&live),
    )
    .go();
    let t = text(&out);
    assert_eq!(code(&out), 1, "{t}");
    let body: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("--json prints one JSON document on stdout ({e}): {t}"));
    assert_eq!(body["zone"], "algedonic.dev");
    assert_eq!(body["counts"]["MATCH"], 4);
    assert_eq!(body["counts"]["DRIFT"], 1);
    assert_eq!(body["counts"]["ABSENT"], 0);
    assert_eq!(body["counts"]["UNDECLARED"], 1);
    assert_eq!(body["hard"], 1);
    let verdicts = body["verdicts"].as_array().expect("verdicts array");
    assert_eq!(verdicts.len(), 6);
    let by_record = |rec: &str| {
        verdicts
            .iter()
            .find(|v| v["record"] == rec)
            .unwrap_or_else(|| panic!("no verdict for {rec}: {t}"))
            .clone()
    };
    let boss = by_record("boss.algedonic.dev CNAME");
    assert_eq!(boss["verdict"], "DRIFT");
    assert_eq!(
        boss["declared"]["content"],
        format!("{TUNNEL_ID}.cfargotunnel.com")
    );
    assert_eq!(
        boss["live"]["content"],
        "00000000-1111-4222-8333-444444444444.cfargotunnel.com"
    );
    assert_eq!(
        boss["why"],
        "the operating site, behind the Cloudflare Tunnel and the Access application declared for it"
    );
    // The interlock rides the verdict: the handler reads it here (not
    // from a second parse of the declaration) to know which records it
    // may apply, and under what condition.
    assert_eq!(
        boss["interlock"], "access",
        "the declared interlock is carried on the verdict for the handler to honour"
    );
    let pg = by_record("playground.algedonic.dev CNAME");
    assert_eq!(pg["verdict"], "MATCH");
    assert!(
        pg.get("interlock").is_none() || pg["interlock"].is_null(),
        "no interlock declared, none reported: {pg}"
    );
    assert_eq!(
        pg["declared"]["content"],
        format!("{TUNNEL_ID}.cfargotunnel.com"),
        "the reference is shown RESOLVED, so the packet names the tunnel the zone was compared against"
    );
    assert_eq!(pg["declared"]["target"], format!("tunnel:{CREDENTIAL}"));
    let id = by_record("id.algedonic.dev A");
    assert_eq!(id["verdict"], "UNDECLARED");
    assert_eq!(id["live"]["content"], "203.0.113.7");
    assert!(id.get("declared").is_none() || id["declared"].is_null());
    // Every verdict names its record and its verdict as non-empty
    // strings: the shape the packet's `verdicts` field requires
    // (item_keys = ["record", "verdict"]).
    for v in verdicts {
        for key in ["record", "verdict"] {
            assert!(
                v[key].as_str().is_some_and(|s| !s.trim().is_empty()),
                "{key} missing on {v}"
            );
        }
    }
}

#[test]
fn list_references_names_every_reference_the_declaration_makes_and_reads_no_input() {
    let out = Run::new(&["algedonic.dev", "--list-references"], String::new()).go();
    assert_eq!(code(&out), 0, "{}", text(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("tunnel:{CREDENTIAL}")
    );
}

// ---- refusals -------------------------------------------------------

#[test]
fn an_unresolved_tunnel_reference_is_a_refusal_naming_it_not_a_drift() {
    let out = Run::new(&["algedonic.dev"], envelope(&as_measured())).go();
    let t = text(&out);
    assert_eq!(code(&out), 2, "{t}");
    assert!(
        t.contains(&format!("tunnel:{CREDENTIAL}")) && t.contains("--tunnel"),
        "the refusal names the reference and how to resolve it: {t}"
    );
    assert!(lines_with(&out, "DRIFT").is_empty(), "{t}");
}

#[test]
fn a_zone_with_no_declaration_is_a_refusal() {
    let out = Run::new(&["example.org"], envelope(&as_measured())).go();
    let t = text(&out);
    assert_eq!(code(&out), 2, "{t}");
    assert!(
        t.contains("example.org") && t.contains("no declaration"),
        "{t}"
    );
}

#[test]
fn a_declaration_naming_one_record_twice_is_refused() {
    let dir = scratch("doubled-declaration");
    boss_testing::write_file(
        &dir.join("example.org.toml"),
        r#"zone = "example.org"
[[record]]
name = "www.example.org"
type = "A"
target = "192.0.2.1"
proxied = false
ttl = 300
why = "first"
[[record]]
name = "www.example.org"
type = "A"
target = "192.0.2.2"
proxied = false
ttl = 300
why = "second"
"#,
    );
    let out = Run::new(&["example.org"], "[]".to_string())
        .declarations(&dir)
        .go();
    let t = text(&out);
    assert_eq!(code(&out), 2, "{t}");
    assert!(t.contains("www.example.org") && t.contains("twice"), "{t}");
}

#[test]
fn an_interlock_the_handler_does_not_know_is_refused_naming_it() {
    let dir = scratch("unknown-interlock");
    boss_testing::write_file(
        &dir.join("example.org.toml"),
        r#"zone = "example.org"
[[record]]
name = "www.example.org"
type = "A"
target = "192.0.2.1"
proxied = false
ttl = 300
interlock = "moon-phase"
why = "x"
"#,
    );
    let out = Run::new(&["example.org"], "[]".to_string())
        .declarations(&dir)
        .go();
    let t = text(&out);
    assert_eq!(code(&out), 2, "{t}");
    assert!(
        t.contains("moon-phase") && t.contains("interlock"),
        "a record whose interlock nothing honours would be applied by nothing, silently: {t}"
    );
}

#[test]
fn a_declaration_whose_zone_disagrees_with_the_one_asked_for_is_refused() {
    let dir = scratch("wrong-zone");
    boss_testing::write_file(
        &dir.join("example.org.toml"),
        r#"zone = "example.net"
[[record]]
name = "www.example.org"
type = "A"
target = "192.0.2.1"
proxied = false
ttl = 300
why = "x"
"#,
    );
    let out = Run::new(&["example.org"], "[]".to_string())
        .declarations(&dir)
        .go();
    let t = text(&out);
    assert_eq!(code(&out), 2, "{t}");
    assert!(t.contains("example.net"), "{t}");
}

#[test]
fn input_that_is_not_a_record_list_is_a_refusal_not_a_clean_zone() {
    let out = Run::new(
        &[
            "algedonic.dev",
            "--tunnel",
            &format!("{CREDENTIAL}={TUNNEL_ID}"),
        ],
        r#"{"success": false, "errors": [{"code": 10000, "message": "Authentication error"}]}"#
            .to_string(),
    )
    .go();
    let t = text(&out);
    assert_eq!(code(&out), 2, "{t}");
    assert!(
        lines_with(&out, "ABSENT").is_empty(),
        "an error body must not read as two ABSENT records: {t}"
    );
}

#[test]
fn a_box_without_python3_refuses_with_78() {
    let empty = scratch("no-python");
    let out = Run::new(&["algedonic.dev"], envelope(&as_measured()))
        .path_env(&empty)
        .go();
    assert_eq!(code(&out), 78, "{}", text(&out));
}
