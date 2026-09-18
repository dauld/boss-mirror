//! Every tenant reaches an instance through ONE door — `boss tenant
//! publish <dir>` (infra/seed-tenant.sh) — and a tenant with an engine
//! runs its engine's prepare AFTER that, for what only the engine does
//! (backlog b644d727, 2026-09-17; ee7b62bb before it).
//!
//! Until ee7b62bb, `publish_tenant` in infra/oss-quickstart/tenant-launch.sh
//! ran seed-brewery-tenant.sh unconditionally — `boss-brewery-sim
//! prepare` plus the sim's reset-baseline stamp — so a deployment whose
//! BOSS_TENANT_DIR pointed at Algedonic, LLC would still have seeded
//! the brewery. That car chose by `[meta] tenant_id`: the brewery kept
//! its engine script and everything else ran the generic publish.
//! Measured 2026-09-17 (backlog b644d727): the engine's prepare POSTs
//! classes.json but never seeds/locations.toml nor
//! seeds/chart_of_accounts.toml — the playground inherited both from
//! the migrations — and nothing published the brewery's own
//! seeds/rules.toml at all. Now the operator baseline runs FIRST for
//! every tenant, handed the tenant dir (backlog 1ee28274, 2026-09-18:
//! it creates the platform-admin the publish's first platform Job
//! needs, and reads the tenant's declared roster file so a real
//! tenant's founder is not injected ahead of the publish); the generic
//! publish follows it, brewery included; and the brewery's engine
//! script runs LAST, for the sim data, the reset baseline and the rows
//! only it seeds (its own classes post and workflow walk become
//! no-ops: insert-if-absent, and a kind an authoring Job already
//! published is skipped). Exercised here under stubs, the way
//! infra/lint/a-failed-prepare-degrades-the-pod.sh exercises the
//! degrade contract — no API, no binaries, no /opt/boss.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const LAUNCH_LIB: &str = "infra/oss-quickstart/tenant-launch.sh";
const SEED_TENANT: &str = "infra/seed-tenant.sh";

struct Fixture {
    root: PathBuf,
    /// Stands in for /opt/boss/infra: three stub seed scripts, each
    /// appending its name and the tenant dir it was handed to `log`.
    /// `STUB_EXIT` is every stub's exit; `STUB_FAIL=<script>` makes
    /// that one script alone exit 3.
    infra: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("launcher-publishes-by-id-{name}"));
        let infra = root.join("infra");
        create_dir(&infra);
        let log = root.join("log");
        for script in [
            "seed-estate.sh",
            "seed-operator-baseline.sh",
            "seed-brewery-tenant.sh",
            "seed-tenant.sh",
        ] {
            write_exec(
                &infra.join(script),
                &format!(
                    "#!/usr/bin/env bash\n\
                     echo \"{script} tenant_dir=${{BOSS_TENANT_DIR:-unset}}\" >>\"{}\"\n\
                     [[ \"${{STUB_FAIL:-}}\" == \"{script}\" ]] && exit 3\n\
                     exit \"${{STUB_EXIT:-0}}\"\n",
                    log.display()
                ),
            );
        }
        Self { root, infra, log }
    }

    /// A tenant directory with its manifest at `rel` (root or seeds/).
    fn tenant(&self, name: &str, rel: &str, tenant_id: &str) -> PathBuf {
        let dir = self.root.join(name);
        let manifest = dir.join(rel);
        create_dir(manifest.parent().unwrap());
        write_file(
            &manifest,
            &format!("[meta]\ntenant_id = \"{tenant_id}\"\ndisplay_name = \"{name}\"\n"),
        );
        dir
    }

    /// Source the launcher lib and call publish_tenant under `env`.
    fn publish(&self, env: &[(&str, &str)]) -> (i32, String) {
        let _ = std::fs::remove_file(&self.log);
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(format!(
                ". \"{}\"; publish_tenant",
                repo_root().join(LAUNCH_LIB).display()
            ))
            .env("BOSS_INFRA_DIR", &self.infra)
            .env_remove("BOSS_TENANT_DIR")
            .env_remove("BOSS_TENANT_MANIFEST_TOML");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run bash");
        let log = std::fs::read_to_string(&self.log).unwrap_or_default();
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{log}--- stdout\n{}--- stderr\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }
}

fn s(p: &Path) -> String {
    p.display().to_string()
}

#[test]
fn the_brewery_is_published_through_the_generic_door_before_its_engine_seeds_the_rest() {
    let fx = Fixture::new("brewery");
    // The N-1 deployment shape: BOSS_TENANT_MANIFEST_TOML at the seeds/
    // spelling, no BOSS_TENANT_DIR — the tenant dir is one above.
    let brewery = fx.tenant("brewery", "seeds/tenant.toml", "brewery");
    let manifest = brewery.join("seeds/tenant.toml");
    let (rc, out) = fx.publish(&[("BOSS_TENANT_MANIFEST_TOML", &s(&manifest))]);
    assert_eq!(rc, 0, "{out}");
    // THE BASELINE GOES FIRST (backlog 1ee28274, 2026-09-18): it is
    // what creates the only platform-admin on an example instance, and
    // Q7 owner resolution needs one before the publish can open ANY
    // platform Job — publish-first left the playground DEGRADED at
    // seeds/workflows.toml ("no responsible human resolvable for owner
    // automation:bootstrap"). It is handed the tenant dir so it can
    // read the declared roster and skip the bootstrap row a real
    // tenant is about to land itself (the 0d2d7daa constraint). Then
    // the same door every tenant takes, handed the brewery's directory:
    // locations, the chart, the rules and everything else the contract
    // names reach the instance through it, not through the engine. The
    // engine runs LAST, for the sim data and the reset-baseline stamp.
    //
    // THE ESTATE GOES BEFORE ALL OF IT (backlog ee368d0c, 2026-09-18):
    // the instance's machines are the tree's declaration, published
    // through the estate door with no tenant dir — it is not the
    // tenant's — before the baseline and the publish.
    let scripts: Vec<&str> = out.lines().take_while(|l| !l.starts_with("---")).collect();
    let baseline = format!("seed-operator-baseline.sh tenant_dir={}", s(&brewery));
    let generic = format!("seed-tenant.sh tenant_dir={}", s(&brewery));
    assert_eq!(
        scripts,
        vec![
            "seed-estate.sh tenant_dir=unset",
            baseline.as_str(),
            generic.as_str(),
            "seed-brewery-tenant.sh tenant_dir=unset",
        ],
        "the estate, then the baseline, then the publish, then the engine for what only it does:\n{out}"
    );
}

#[test]
fn a_failed_estate_declaration_stops_before_the_baseline_and_is_the_verdict() {
    // The DEGRADED loop retries publish_tenant whole: an estate that did
    // not land is the verdict, and the converges reading roles off an
    // empty registry are not papered over by a tenant that published.
    let fx = Fixture::new("estate-fails");
    let acme = fx.tenant("acme", "tenant.toml", "acme");
    let (rc, out) = fx.publish(&[
        ("BOSS_TENANT_DIR", &s(&acme)),
        ("STUB_FAIL", "seed-estate.sh"),
    ]);
    assert_eq!(rc, 3, "the estate's exit is the verdict:\n{out}");
    assert!(out.starts_with("seed-estate.sh"), "{out}");
    assert!(
        !out.contains("seed-operator-baseline.sh") && !out.contains("seed-tenant.sh"),
        "nothing runs after a failed estate declaration:\n{out}"
    );
}

#[test]
fn a_failed_brewery_publish_stops_before_the_engine() {
    // The degrade contract is the same for the brewery: a publish that
    // did not land is the verdict, and the engine's prepare does not
    // run against the rows the publish was about to declare — the
    // engine counts a 409 as "already there" (its people posts predate
    // backlog 0d2d7daa's GET-after-409), so a location no door seeded
    // would read as a duplicate and the sim would start on a
    // half-published tenant.
    let fx = Fixture::new("brewery-fails");
    let brewery = fx.tenant("brewery", "seeds/tenant.toml", "brewery");
    let (rc, out) = fx.publish(&[
        ("BOSS_TENANT_DIR", &s(&brewery)),
        ("STUB_FAIL", "seed-tenant.sh"),
    ]);
    assert_eq!(rc, 3, "the generic publish's exit is the verdict:\n{out}");
    assert!(
        out.contains(&format!("\nseed-tenant.sh tenant_dir={}", s(&brewery))),
        "{out}"
    );
    assert!(
        !out.contains("seed-brewery-tenant.sh"),
        "the engine does not run after a failed publish:\n{out}"
    );
}

#[test]
fn a_failed_baseline_stops_before_the_publish_and_is_the_verdict() {
    // The baseline is what the publish needs (the platform-admin Q7
    // owner resolution names), so a baseline that did not land is the
    // verdict and nothing publishes against an instance that cannot
    // open a platform Job; the DEGRADED loop retries the whole
    // function.
    let fx = Fixture::new("baseline-fails");
    let brewery = fx.tenant("brewery", "seeds/tenant.toml", "brewery");
    let (rc, out) = fx.publish(&[
        ("BOSS_TENANT_DIR", &s(&brewery)),
        ("STUB_FAIL", "seed-operator-baseline.sh"),
    ]);
    assert_eq!(rc, 3, "the baseline's exit is the verdict:\n{out}");
    assert!(
        out.contains(&format!(
            "\nseed-operator-baseline.sh tenant_dir={}",
            s(&brewery)
        )),
        "{out}"
    );
    assert!(
        !out.contains("seed-tenant.sh") && !out.contains("seed-brewery-tenant.sh"),
        "nothing runs after a failed baseline:\n{out}"
    );
}

#[test]
fn any_other_tenant_runs_the_generic_publish_with_its_directory() {
    let fx = Fixture::new("other");
    let acme = fx.tenant("acme", "tenant.toml", "acme");
    let (rc, out) = fx.publish(&[("BOSS_TENANT_DIR", &s(&acme))]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains(&format!("seed-tenant.sh tenant_dir={}", s(&acme))),
        "tenant_id acme → seed-tenant.sh handed BOSS_TENANT_DIR:\n{out}"
    );
    assert!(!out.contains("seed-brewery-tenant.sh"), "{out}");
    // THE BASELINE GOES FIRST for every tenant, handed the tenant dir
    // (backlog 1ee28274, 2026-09-18). Tenant-first (0d2d7daa) existed
    // so a real company's founder, declared with the bootstrap email,
    // was not refused on the LOWER(email) unique index behind an
    // injected emp-bootstrap-admin — but it left a fresh instance with
    // no platform-admin at the moment the publish needed one. Now the
    // baseline reads the declared roster from the tenant's seed file
    // and skips the injection when the email is declared there, so
    // the founder still lands once and the publish still finds its
    // owner.
    assert!(
        out.contains(&format!(
            "\nseed-operator-baseline.sh tenant_dir={}",
            s(&acme)
        )),
        "the operator baseline runs after the estate, handed the tenant dir:\n{out}"
    );
    assert!(
        out.contains("\nseed-tenant.sh"),
        "the tenant is published after the baseline:\n{out}"
    );
}

#[test]
fn a_failed_generic_baseline_stops_before_the_publish_and_is_the_verdict() {
    // The DEGRADED loop retries publish_tenant whole, so a baseline
    // that did not land must not be followed by a publish whose first
    // platform Job has no owner to resolve.
    let fx = Fixture::new("generic-fails");
    let acme = fx.tenant("acme", "tenant.toml", "acme");
    let (rc, out) = fx.publish(&[("BOSS_TENANT_DIR", &s(&acme)), ("STUB_EXIT", "3")]);
    assert_eq!(rc, 3, "the baseline's exit is the verdict:\n{out}");
    assert!(
        !out.contains("seed-tenant.sh"),
        "the publish did not run after a failed baseline:\n{out}"
    );
}

#[test]
fn the_directory_falls_back_to_the_manifest_path_at_either_spelling() {
    let fx = Fixture::new("fallback");
    // Root spelling: dirname IS the tenant dir.
    let acme = fx.tenant("acme", "tenant.toml", "acme");
    let (rc, out) = fx.publish(&[("BOSS_TENANT_MANIFEST_TOML", &s(&acme.join("tenant.toml")))]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains(&format!("seed-tenant.sh tenant_dir={}", s(&acme))),
        "{out}"
    );
    // seeds/ spelling: the tenant dir is one above.
    let beta = fx.tenant("beta", "seeds/tenant.toml", "beta");
    let (rc, out) = fx.publish(&[(
        "BOSS_TENANT_MANIFEST_TOML",
        &s(&beta.join("seeds/tenant.toml")),
    )]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains(&format!("seed-tenant.sh tenant_dir={}", s(&beta))),
        "{out}"
    );
    // BOSS_TENANT_DIR wins when both are set (the manifest env stays
    // for N-1 and may still name the brewery).
    let brewery = fx.tenant("brewery", "seeds/tenant.toml", "brewery");
    let (rc, out) = fx.publish(&[
        ("BOSS_TENANT_DIR", &s(&acme)),
        (
            "BOSS_TENANT_MANIFEST_TOML",
            &s(&brewery.join("seeds/tenant.toml")),
        ),
    ]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("seed-tenant.sh"), "{out}");
    assert!(!out.contains("seed-brewery-tenant.sh"), "{out}");
}

#[test]
fn the_chosen_scripts_exit_is_the_publish_verdict() {
    // The degrade contract reads publish_tenant's status; a failing
    // seed must surface as non-zero, not be masked by the baseline.
    let fx = Fixture::new("verdict");
    let acme = fx.tenant("acme", "tenant.toml", "acme");
    let (rc, out) = fx.publish(&[("BOSS_TENANT_DIR", &s(&acme)), ("STUB_EXIT", "3")]);
    assert_ne!(rc, 0, "{out}");
}

// ---------------------------------------------------------------------------
// infra/seed-tenant.sh itself: `boss tenant publish <dir>` with
// retries, output kept, no baseline stamp.
// ---------------------------------------------------------------------------

struct SeedFixture {
    root: PathBuf,
    bin: PathBuf,
    calls: PathBuf,
}

impl SeedFixture {
    /// A stub `boss` that records argv and fails until `succeed_on`.
    /// A stub `psql` that records if it is ever run.
    fn new(name: &str, succeed_on: u32) -> Self {
        let root = scratch_dir(&format!("seed-tenant-sh-{name}"));
        let bin = root.join("bin");
        create_dir(&bin);
        let calls = root.join("calls");
        write_exec(
            &bin.join("boss"),
            &format!(
                "#!/usr/bin/env bash\n\
                 n=0; [[ -f \"{calls}.n\" ]] && n=$(<\"{calls}.n\"); n=$((n+1)); echo $n >\"{calls}.n\"\n\
                 echo \"boss $*\" >>\"{calls}\"\n\
                 if [[ $n -ge {succeed_on} ]]; then echo \"published on attempt $n\"; exit 0; fi\n\
                 echo \"attempt $n: connection refused\" >&2; exit 1\n",
                calls = calls.display()
            ),
        );
        write_exec(
            &bin.join("psql"),
            &format!(
                "#!/usr/bin/env bash\necho \"psql $*\" >>\"{}\"\nexit 0\n",
                calls.display()
            ),
        );
        Self { root, bin, calls }
    }

    fn run(&self, tenant_dir: &Path, attempts: &str) -> (i32, String) {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new("bash")
            .arg(repo_root().join(SEED_TENANT))
            .env("PATH", path)
            .env("BOSS_TENANT_DIR", tenant_dir)
            .env("BOSS_PUBLISH_ATTEMPTS", attempts)
            .env("BOSS_PUBLISH_RETRY_SECONDS", "0")
            .env("BOSS_POSTGRES_URL", "postgres://never/used")
            .output()
            .expect("run seed-tenant.sh");
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
fn seed_tenant_retries_the_publish_until_it_lands_and_stamps_no_baseline() {
    let fx = SeedFixture::new("retries", 3);
    let tenant = fx.root.join("acme");
    create_dir(&tenant);
    let (rc, out) = fx.run(&tenant, "5");
    assert_eq!(rc, 0, "{out}");
    let publishes = out
        .lines()
        .filter(|l| *l == format!("boss tenant publish {}", tenant.display()))
        .count();
    assert_eq!(publishes, 3, "two refusals then success:\n{out}");
    assert!(
        out.contains("published on attempt 3"),
        "the publish's own output is kept, not discarded:\n{out}"
    );
    assert!(
        !out.contains("psql"),
        "a tenant with no engine gets NO sim baseline stamp:\n{out}"
    );
}

#[test]
fn seed_tenant_fails_loudly_with_the_last_output_when_the_publish_never_lands() {
    let fx = SeedFixture::new("fails", 99);
    let tenant = fx.root.join("acme");
    create_dir(&tenant);
    let (rc, out) = fx.run(&tenant, "2");
    assert_ne!(rc, 0, "{out}");
    assert!(
        out.contains("attempt 2: connection refused"),
        "the last attempt's own words are printed:\n{out}"
    );
    assert!(out.contains("after 2 attempts"), "{out}");
}

#[test]
fn seed_tenant_refuses_without_a_tenant_dir_and_names_the_variable() {
    let fx = SeedFixture::new("refuses", 1);
    let out = Command::new("bash")
        .arg(repo_root().join(SEED_TENANT))
        .env(
            "PATH",
            format!(
                "{}:{}",
                fx.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("BOSS_TENANT_DIR")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("BOSS_TENANT_DIR"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_image_carries_the_generic_seed_script_beside_the_brewerys() {
    // The launcher invokes it by absolute path under /opt/boss/infra;
    // the gate never builds the image, so the pairing is pinned here
    // the way the brewery script's is.
    let df = std::fs::read_to_string(repo_root().join("infra/oss-quickstart/Dockerfile")).unwrap();
    assert!(
        df.contains("COPY infra/seed-tenant.sh /opt/boss/infra/seed-tenant.sh"),
        "Dockerfile must COPY infra/seed-tenant.sh to /opt/boss/infra"
    );
    assert!(
        df.contains("/opt/boss/infra/seed-tenant.sh\n")
            || df.contains("/opt/boss/infra/seed-tenant.sh \\"),
        "the chmod list must include /opt/boss/infra/seed-tenant.sh"
    );
}
