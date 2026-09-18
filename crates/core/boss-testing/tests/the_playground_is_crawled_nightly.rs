//! The playground is crawled nightly by a chore, and the dead live suite
//! stays dead (design 0e07ce64, backlog 8664f53e, 2026-09-18).
//!
//! MEASURED. `apps/web/tests/smoke/` held 35 Playwright specs against a
//! live backend — a scratch stack `playwright.config.ts` spawned and
//! `tests/globalSetup.ts` seeded. Last touched 2026-06-18; run by no CI
//! job (`.forgejo/workflows/ci.yml` runs `test:mocked`), no gate check
//! (`infra/gate.sh`'s web-suite is unit+build+mocked), no chore, and no
//! package.json script. A check nobody runs is a check that is not
//! running, and one whose config spawns a stack against `BOSS_SCRATCH`
//! ports nothing provides any more is worse: it reads as coverage.
//!
//! WHAT REPLACES IT. What only a real backend can show — a surface that
//! throws on a shape the backend actually produces — is ONE nightly
//! chore: a CronJob in the boss-ci image (the browser is baked in at
//! /opt/ms-playwright; the boss image has none) that clones main from
//! the forge, installs apps/web, and runs `bun run test:live` against
//! the playground's in-cluster gateway as a guest. The roster it crawls
//! is the mocked suite's `_routes.ts` — one list, both crawls — and its
//! verdict rides the `maintenance-playground-crawl` packet through
//! boss-chore.sh, one `RED <route> …` line per red surface.
//!
//! WHY boss-dev, and why `pipeline`. The forge repository is NOT
//! anonymously readable (measured 2026-09-18: `info/refs` answers 401),
//! so the clone needs the read credential, and that Secret
//! (`forge-read`) lives only in the pipeline's namespace beside the gate
//! runner that uses it the same way. The chore is instance-SHAPED — it
//! crawls one instance — but it is pinned to the pipeline for that
//! credential, files its packet on prod's system of record (IT's queue,
//! the same door the conductor uses), and names the playground by its
//! Service. instance-manifests.txt classifies it `pipeline` with that
//! reason; this file pins the same facts so the manifest, the roster,
//! the bundle, the script and the cadence roster cannot drift apart
//! (CLAUDE.md §9a).
//!
//! WHAT THIS PINS:
//!   * the live suite, its config and its directory are gone (the one
//!     file it held for a day, the mocked suite's `mountPage` helper, now
//!     lives beside its importers at tests/mocked/_helpers.ts), and
//!     apps/web carries exactly two Playwright configs: mocked (gated)
//!     and live (the chore);
//!   * `bun run test:live` is the live config, which reads the crawled
//!     instance from BOSS_E2E_BASE_URL and nothing else, and the crawl
//!     spec reads the mocked roster rather than its own copy;
//!   * the kind file exists with the chore shape and the CronJob runs
//!     it through boss-chore.sh, in boss-dev, in the boss-ci image, with
//!     the helpers copied in from the boss image, the read credential
//!     mounted, the playground's gateway as the target and prod's jobs
//!     door as the record — every name derived from instances.toml;
//!   * the roster classifies the manifest `pipeline`, and the
//!     cadence-silence sweep expects a packet daily.

use boss_testing::repo_root;
use std::path::Path;

const KIND: &str = "maintenance-playground-crawl";
const BUNDLE: &str = "infra/platform/workflows/maintenance-playground-crawl.toml";
const MANIFEST: &str = "infra/cluster/manifests/boss-playground-crawl.yaml";
const ROSTER: &str = "infra/cluster/instance-manifests.txt";
const INSTANCES: &str = "infra/cluster/instances.toml";
const CADENCE: &str = "infra/dispatcher/rules/cadence-silence-sweep-daily.toml";
const WEB: &str = "apps/web";

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn exists(rel: &str) -> bool {
    repo_root().join(rel).exists()
}

/// `[section]` → `key = "value"` from instances.toml, read the way the
/// renderer reads it: one `key = "value"` per line under a header.
fn instance_value(section: &str, key: &str) -> String {
    let toml = read(INSTANCES);
    // A header is a line of its own: the prose above the table names
    // `[prod]` too, and matching the first mention read the comment.
    let (_, body) = toml
        .split_once(&format!("\n[{section}]\n"))
        .unwrap_or_else(|| panic!("{INSTANCES}: no [{section}] section"));
    let body = body.split("\n[").next().unwrap_or(body);
    body.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            l.strip_prefix(&format!("{key} = "))
                .map(|v| v.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| panic!("{INSTANCES}: [{section}] declares no {key}"))
}

// --- the dead suite stays dead ------------------------------------------------

#[test]
fn the_live_suite_and_its_config_are_gone() {
    for gone in [
        "apps/web/playwright.config.ts",
        "apps/web/tests/globalSetup.ts",
        "apps/web/tests/smoke/COVERAGE.md",
    ] {
        assert!(
            !exists(gone),
            "{gone} is back — the live smoke suite ran under nothing from 2026-06-18 to \
             2026-09-18 and was deleted for it (design 0e07ce64); a live crawl is the nightly \
             chore, not a suite in the tree"
        );
    }
    // The directory itself is gone too. It survived one day for ONE
    // file — the mocked suite's `mountPage` helper, which twelve mocked
    // specs imported from `../smoke/_helpers` — until ac3270c7 moved the
    // helper beside its importers. A tests/smoke that comes back is a
    // live suite nothing runs.
    assert!(
        !exists("apps/web/tests/smoke"),
        "apps/web/tests/smoke is back — the mocked suite's helper lives at tests/mocked/_helpers.ts \
         and the live crawl at tests/live; a spec here is a live suite nothing runs"
    );
    assert!(
        exists("apps/web/tests/mocked/_helpers.ts"),
        "apps/web/tests/mocked/_helpers.ts holds mountPage, beside the specs that import it"
    );
    let mocked = repo_root().join("apps/web/tests/mocked");
    let specs: Vec<(String, String)> = std::fs::read_dir(&mocked)
        .unwrap_or_else(|e| panic!("{}: {e}", mocked.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".spec.ts"))
        .map(|e| {
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            (e.file_name().to_string_lossy().into_owned(), text)
        })
        .collect();
    let importers = specs
        .iter()
        .filter(|(_, text)| text.contains("from './_helpers'"))
        .count();
    assert!(
        importers >= 12,
        "the twelve mocked specs still import mountPage, now from './_helpers' (found {importers})"
    );
    for (name, text) in &specs {
        assert!(
            !text.contains("../smoke/"),
            "tests/mocked/{name} imports from ../smoke/, a directory that no longer exists"
        );
    }
}

#[test]
fn the_web_app_carries_exactly_the_mocked_and_live_playwright_configs() {
    let dir = repo_root().join(WEB);
    let mut configs: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("playwright") && n.ends_with(".config.ts"))
        .collect();
    configs.sort();
    assert_eq!(
        configs,
        ["playwright.live.config.ts", "playwright.mocked.config.ts"],
        "two Playwright configs: the gated mocked suite and the live crawl the chore runs — \
         a third is a suite nothing runs"
    );
}

// --- the live crawl ------------------------------------------------------------

#[test]
fn the_live_script_runs_the_live_config_which_needs_the_instance_named() {
    let pkg: serde_json::Value =
        serde_json::from_str(&read("apps/web/package.json")).expect("package.json parses");
    let script = pkg["scripts"]["test:live"]
        .as_str()
        .expect("apps/web/package.json declares scripts.test:live — the command the chore runs");
    assert!(
        script.contains("playwright test") && script.contains("-c playwright.live.config.ts"),
        "test:live runs Playwright with the live config: {script}"
    );
    let config = read("apps/web/playwright.live.config.ts");
    assert!(
        config.contains("process.env['BOSS_E2E_BASE_URL']"),
        "the live config reads the crawled instance from BOSS_E2E_BASE_URL"
    );
    assert!(
        config.contains("throw new Error(") && config.contains("BOSS_E2E_BASE_URL is not set"),
        "and refuses without it, naming the variable — a default would crawl something and \
         report on it as the playground"
    );
    assert!(
        config.contains("testDir: './tests/live'"),
        "the live specs live under tests/live, apart from the mocked suite"
    );
    assert!(
        !config.contains("webServer:"),
        "a live config spawns nothing: the instance it crawls is already up"
    );
}

#[test]
fn the_crawl_reads_the_mocked_roster_rather_than_its_own() {
    let spec = read("apps/web/tests/live/playground-crawl.spec.ts");
    assert!(
        spec.contains("import { ROUTES } from '../mocked/_routes'"),
        "the live crawl and the mocked crawls read ONE roster (CLAUDE.md §9a) — a second list \
         is a surface crawled by one and silently skipped by the other"
    );
    assert!(
        spec.contains("'/api/auth/guest'"),
        "the crawl's identity is the guest session the playground offers a stranger"
    );
    assert!(
        spec.contains("`RED ${i.route} ${i.kind}: "),
        "every red route is one `RED <route> <kind>: …` line, so the packet's output names them"
    );
    // The guest's refusals are listed, with the reason, and printed
    // apart from unexplained noise (ac3270c7 part 4: expected, not gated).
    assert!(
        spec.contains("const EXPECTED_CONSOLE_ERRORS")
            && spec.contains("a guest hitting an operator route is the product refusing correctly"),
        "the crawl declares the console.error lines it expects, each with the reason"
    );
}

#[test]
fn the_judge_rule_opens_one_item_per_red_route_on_the_failed_close() {
    let rule = read("infra/dispatcher/rules/file-backlog-items-on-playground-crawl-red.toml");
    assert!(
        rule.contains(&format!("kind = \"{KIND}\"")) && rule.contains("outcome = \"failed\""),
        "the rule fires on this chore's `failed` close and nothing else"
    );
    assert!(
        rule.contains("handler = \"maintenance.chore.file_reds\"")
            && rule.contains("step = \"\\\"run\\\"\"")
            && rule.contains("design = \"\\\"0e07ce64\\\"\""),
        "the RED lines are read off the `run` step and every item carries the design id"
    );
}

// --- the chore -----------------------------------------------------------------

#[test]
fn the_kind_is_a_chore_in_the_platform_bundle() {
    let bundle = read(BUNDLE);
    assert!(
        bundle.contains(&format!("kind = \"{KIND}\"")),
        "{BUNDLE} declares {KIND}"
    );
    for step in [
        "title = \"scheduled\"",
        "title = \"run\"",
        "title = \"completed\"",
        "title = \"failed\"",
    ] {
        assert!(
            bundle.contains(step),
            "{BUNDLE}: the chore shape carries {step}"
        );
    }
    assert!(
        bundle.contains("audience = { individual = \"automation:boss-step\" }"),
        "{BUNDLE}: `run` is the automation's step (platform_bundle_maintenance.rs pins the family)"
    );
}

#[test]
fn the_cronjob_runs_the_crawl_through_the_chore_wrapper_against_the_playground() {
    let yaml = read(MANIFEST);
    let play_ns = instance_value("playground", "namespace");
    let prod_ns = instance_value("prod", "namespace");

    assert!(
        yaml.lines().any(|l| l == "kind: CronJob"),
        "{MANIFEST} is a CronJob"
    );
    assert!(
        yaml.contains("\n  namespace: boss-dev\n"),
        "{MANIFEST}: the chore runs in the pipeline's namespace, where the forge read credential is"
    );
    // Code lines only: the header names the wrapper in prose.
    let chore = yaml
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find(|l| l.contains("boss-chore.sh "))
        .unwrap_or_else(|| panic!("{MANIFEST} runs its check through boss-chore.sh"));
    assert!(
        chore.split_whitespace().any(|w| w == KIND) && chore.contains(" -- "),
        "{MANIFEST}: boss-chore.sh <kind> \"<title>\" -- <check>: {chore}"
    );
    assert!(
        yaml.contains("bun run test:live"),
        "{MANIFEST}: the check is the live script, the one command a person runs by hand too"
    );
    assert!(
        yaml.contains("git clone --depth 1"),
        "{MANIFEST}: the crawl needs the tree (the roster and the spec), so the check clones main"
    );

    // Images: the crawl container is boss-ci (the browser), the helpers
    // come from the boss image, and nothing else.
    let images: Vec<&str> = yaml
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("image: "))
        .map(str::trim)
        .collect();
    assert!(
        images
            .iter()
            .any(|i| i.starts_with("10.20.0.15:3000/david/boss-ci:")),
        "{MANIFEST}: the crawl runs in the boss-ci image — the only one with the browser: {images:?}"
    );
    assert!(
        images
            .iter()
            .any(|i| i.starts_with("10.20.0.15:3000/david/boss:")),
        "{MANIFEST}: the boss image carries boss-chore.sh and its helpers: {images:?}"
    );
    assert_eq!(
        images.len(),
        2,
        "{MANIFEST}: two containers, two images: {images:?}"
    );
    for helper in [
        "boss-chore.sh",
        "boss-maintenance-wrap.sh",
        "boss-step.sh",
        "boss-api-curl.sh",
    ] {
        assert!(
            yaml.contains(&format!("/usr/local/bin/{helper}")),
            "{MANIFEST}: the init container copies {helper} from the boss image — the wrapper \
             resolves its helpers next to itself"
        );
    }

    // Targets: crawl the playground's gateway, record on prod's jobs door.
    assert!(
        yaml.contains(&format!(
            "value: http://boss-gateway.{play_ns}.svc.cluster.local"
        )),
        "{MANIFEST}: BOSS_E2E_BASE_URL is the playground's gateway Service ({play_ns}), the \
         origin the tunnel proxies to — the public hostname answers 302 to Access"
    );
    assert!(
        yaml.contains(&format!(
            "value: http://boss-jobs-internal.{prod_ns}.svc.cluster.local:7900"
        )),
        "{MANIFEST}: BOSS_JOBS_URL is prod's jobs door ({prod_ns}) — the packet is IT's, on the \
         system of record"
    );
    assert!(
        yaml.contains("- name: BOSS_JOBS_URL\n"),
        "{MANIFEST}: the wrapper's helpers refuse without BOSS_JOBS_URL (exit 78)"
    );
    assert!(
        yaml.contains("secretName: forge-read"),
        "{MANIFEST}: the clone reads the forge with the pipeline's read credential"
    );
    assert!(
        yaml.contains("{key: node-role.kubernetes.io/control-plane, operator: DoesNotExist}"),
        "{MANIFEST}: a clone + install + browser is a write sink; never a control-plane node"
    );
}

#[test]
fn the_roster_classifies_the_crawl_as_pipeline() {
    let roster = read(ROSTER);
    let name = Path::new(MANIFEST)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap();
    let line = roster
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(&format!("{name} ")))
        .unwrap_or_else(|| {
            panic!("{ROSTER} classifies {name} — an unclassified manifest refuses the render")
        });
    assert_eq!(
        line,
        format!("{name} pipeline"),
        "{ROSTER}: the crawl is pinned to the pipeline for the forge credential, and crawls the \
         playground by name — rendered per instance it would run once per namespace against one target"
    );
}

#[test]
fn the_cadence_silence_sweep_expects_the_crawl_daily() {
    let rule = read(CADENCE);
    assert!(
        rule.contains(&format!("\"interval_minutes.{KIND}\" = \"1440\"")),
        "{CADENCE}: a night the crawl did not run must be noticed — declare {KIND} at 1440"
    );
}
