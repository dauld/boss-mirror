//! Phase 0 — pre-flight the locomotive.

use super::*;

// ---------------------------------------------------------------------------
// Phase 0 — pre-flight the locomotive
//
// The 2026-08-10 18:01 window crashed before boarding: a sudo probe had
// left root-owned objects in the clone, and the conductor's fetch died
// at the moment the window opened. The consist had been rehearsed; the
// locomotive had not. Every entry (including the 10-minute reconcile,
// which is thereby the early-warning cadence) proves the clone healthy
// before touching train state, and a sick locomotive exits 3 — loud in
// the unit's status — instead of surfacing at departure time.
// ---------------------------------------------------------------------------

/// The conductor's effective uid. std exposes no geteuid, and the
/// workspace carries no libc-level dependency worth adding for one
/// call; POSIX `id -u` prints exactly this.
fn euid() -> Result<u32> {
    let out = sh(&["id", "-u"])?;
    stdout_str(&out).trim().parse().context("parsing `id -u`")
}

/// Collect files under `dir` not owned by uid `me` — the recursive
/// half of python's os.walk. A directory that refuses a read is
/// skipped (os.walk's default); a file gone before lstat is skipped
/// too — gc'd mid-walk; ownership of what remains is what matters.
fn walk_foreign(dir: &Path, me: u32, foreign: &mut Vec<PathBuf>) -> Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let meta = match path.symlink_metadata() {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).context(format!("lstat {}", path.display())),
        };
        if meta.is_dir() {
            walk_foreign(&path, me, foreign)?;
        } else if meta.uid() != me {
            foreign.push(path);
        }
    }
    Ok(())
}

/// Host of an http(s) URL — scheme, userinfo, port, and path all
/// stripped. Enough to ask "is this loopback?" without a URL crate.
fn url_host(url: &str) -> &str {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    }
}

/// The drift sentinel (split-brain incident c4b4a6b0): BOSS_JOBS_URL
/// defaulted to localhost on a cutover box and the conductor silently
/// booked a whole window's trains on the wrong instance. A loopback
/// jobs URL is a preflight problem unless the box declares that a
/// local jobs-api is the point — BOSS_TRAIN_ALLOW_LOCAL_JOBS=1, set
/// deliberately by test harnesses and demo boxes.
pub(crate) fn local_jobs_problem(jobs_url: &str, allow_local: bool) -> Option<String> {
    if allow_local {
        return None;
    }
    let host = url_host(jobs_url);
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    loopback.then(|| {
        format!(
            "BOSS_JOBS_URL resolves to loopback ({jobs_url}) — bookkeeping must target \
             the jobs system of record, not this box (split-brain incident c4b4a6b0); \
             set BOSS_TRAIN_ALLOW_LOCAL_JOBS=1 only where a local jobs-api is the point"
        )
    })
}

/// Return the list of problems; empty means the locomotive is fit.
pub(super) fn preflight(cfg: &Config) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    // Every git command below carries the forge credential on itself
    // (git_auth::command) — nothing to configure first, nothing written
    // to this or any other user's git config.
    // The drift sentinel runs first, clone or no clone: a conductor
    // whose bookkeeping would land on this box instead of the system
    // of record must not pull at all.
    if let Some(p) = local_jobs_problem(&cfg.jobs, cfg.allow_local_jobs) {
        problems.push(p);
    }
    // The invariant is OWNERSHIP, not uid zero: the conductor must run
    // as the clone's owner. The original flat refuse-root check said
    // the same thing only on the box where the service user is not
    // root — in a CI container every process IS root and the fixture
    // clone is root-owned, which is perfectly consistent. The
    // foreign-owned walk below enforces the real rule in both worlds:
    // root over the service user's clone still fails (every object is
    // foreign to euid 0), and the poisoning incident this guards
    // against stays guarded.
    let git_dir = Path::new(&cfg.clone).join(".git");
    if !git_dir.is_dir() {
        log("preflight: no clone yet — first boarding will create it");
        return Ok(problems);
    }
    let me = euid()?;
    let mut foreign = Vec::new();
    walk_foreign(&git_dir, me, &mut foreign)?;
    if !foreign.is_empty() {
        let shown = foreign
            .iter()
            .take(3)
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        problems.push(format!(
            "{} object(s) in the clone not owned by uid {me} (e.g. {shown}) — \
             a foreign-uid run has poisoned {}",
            foreign.len(),
            cfg.clone
        ));
    }
    for remote in ["origin", "fork"] {
        let r = sh_unchecked(&[
            "git",
            "-C",
            &cfg.clone,
            "fetch",
            remote,
            "--prune",
            "--dry-run",
        ])?;
        if !r.status.success() {
            let stderr = String::from_utf8_lossy(&r.stderr);
            let stderr = stderr.trim();
            let detail = if stderr.is_empty() {
                format!("rc={}", r.status.code().unwrap_or(-1))
            } else {
                stderr.lines().last().unwrap_or_default().to_string()
            };
            problems.push(format!("dry fetch of {remote} failed: {detail}"));
        }
    }
    // THE ADAPTER MUST MATCH THE REMOTE IT WILL BE POINTED AT.
    //
    // `BOSS_TRAIN_FORGE` defaults to `github`, so a conductor verb run
    // without the systemd unit's environment selects the GitHub adapter
    // over a clone whose remotes are the internal forge. Nothing says
    // so: the command runs, and `gh pr close http://10.20.0.15:3000/...`
    // fails at the END with "none of the git remotes ... point to a
    // known GitHub host" — after `boss train cancel` has already
    // released every car. Two trains were left half-cancelled that way
    // on 2026-08-27 (b9801aff), and preflight is where the packet's own
    // correction says the assertion belongs.
    let origin = sh_unchecked(&["git", "-C", &cfg.clone, "remote", "get-url", "origin"]);
    if let Ok(o) = origin
        && o.status.success()
        && let Some(p) = forge_mismatch(&cfg.forge_kind, String::from_utf8_lossy(&o.stdout).trim())
    {
        problems.push(p);
    }
    Ok(problems)
}

/// Does the selected forge adapter match the remote it will act on?
///
/// PURE, because the refusal has to be exactly right: a false positive
/// here stops the conductor entirely. Only a definite contradiction
/// counts — the GitHub adapter over a non-GitHub origin, or the Forgejo
/// adapter over github.com. Anything unrecognised is left alone.
pub(crate) fn forge_mismatch(forge_kind: &str, origin_url: &str) -> Option<String> {
    // A LOCAL PATH IS NOT A FORGE, so it cannot contradict one. The
    // first version of this check refused any non-GitHub origin, which
    // failed `healthy_clone_passes` — that fixture points origin at
    // /tmp/…/upstream.git with no forge configured, and there is nothing
    // wrong with it. The gate caught it, which is the outcome this
    // function's own doc comment asks for: a false positive here stops
    // every train, so it is worse than the bug.
    let addressable = origin_url.contains("://") || origin_url.contains('@');
    if !addressable {
        return None;
    }
    let is_github = origin_url.contains("github.com");
    match forge_kind {
        "github" if !is_github && !origin_url.is_empty() => Some(format!(
            "forge adapter is `github` (BOSS_TRAIN_FORGE unset defaults to it) but origin is \
             {origin_url}, which is not a GitHub host. Every forge call would fail — and a \
             cancel fails only AFTER releasing its cars. Set the conductor's environment: \
             BOSS_TRAIN_FORGE=forgejo BOSS_TRAIN_FORGE_URL=http://10.20.0.15:3000 \
             BOSS_TRAIN_FORGE_REPO=david/boss \
             BOSS_TRAIN_FORGE_TOKEN_FILE=/etc/boss-train/forge.token"
        )),
        "forgejo" if is_github => Some(format!(
            "forge adapter is `forgejo` but origin is {origin_url}, a GitHub host — the \
             adapter would post to a forge that does not hold this repository."
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // The adapter must match the remote (b9801aff).
    // ---------------------------------------------------------------

    /// THE EXACT MISCONFIGURATION. `BOSS_TRAIN_FORGE` unset defaults to
    /// `github`, and the conductor clone's origin is the internal forge.
    /// Two trains were left half-cancelled because this was only
    /// discovered by `gh` failing AFTER the cars were released.
    #[test]
    fn the_github_adapter_over_a_forge_origin_is_refused() {
        let p =
            forge_mismatch("github", "http://10.20.0.15:3000/david/boss.git").expect("must refuse");
        assert!(p.contains("BOSS_TRAIN_FORGE=forgejo"), "{p}");
        assert!(p.contains("AFTER releasing its cars"), "{p}");
    }

    /// The mirror image, so the check is not just a github-shaped grep.
    #[test]
    fn the_forgejo_adapter_over_github_is_refused() {
        assert!(forge_mismatch("forgejo", "https://github.com/algedonic-dev/boss.git").is_some());
    }

    /// AND THE FALSE POSITIVES THAT WOULD STOP THE CONDUCTOR. Each of
    /// these is a working configuration; refusing any of them would be
    /// worse than the bug, because preflight gates every train.
    #[test]
    fn matching_configurations_are_left_alone() {
        assert_eq!(
            forge_mismatch("forgejo", "http://10.20.0.15:3000/david/boss.git"),
            None
        );
        assert_eq!(
            forge_mismatch("github", "https://github.com/algedonic-dev/boss.git"),
            None
        );
        assert_eq!(
            forge_mismatch("github", "git@github.com:david/boss.git"),
            None
        );
        // THE FALSE POSITIVE THE GATE CAUGHT. `healthy_clone_passes`
        // points origin at a local bare repo with no forge configured,
        // and the first version of this check called that a
        // misconfiguration — failing a fixture that is entirely healthy.
        // A filesystem path addresses no host, so it cannot contradict
        // an adapter.
        //
        // shared-tmp-ok: an expectation string about a remote URL, not a
        // path anything builds — nothing here touches the filesystem.
        assert_eq!(
            forge_mismatch("github", "/tmp/boss-preflight-102054-healthy/upstream.git"),
            None
        );
        assert_eq!(forge_mismatch("forgejo", "/srv/git/boss.git"), None);
        assert_eq!(forge_mismatch("github", "../fixtures/upstream.git"), None);
        // An unreadable origin is not a contradiction, and an unknown
        // adapter is make_forge's error to raise, not preflight's.
        assert_eq!(forge_mismatch("github", ""), None);
        assert_eq!(
            forge_mismatch("gitlab", "http://10.20.0.15:3000/x.git"),
            None
        );
    }

    #[test]
    fn a_loopback_jobs_url_is_a_preflight_problem() {
        for url in [
            "http://127.0.0.1:7900",
            "http://localhost:7900",
            "http://LOCALHOST:7900",
            "http://[::1]:7900",
            "http://127.9.9.9/api",
        ] {
            let p = local_jobs_problem(url, false)
                .unwrap_or_else(|| panic!("{url} must trip the sentinel"));
            assert!(p.contains("BOSS_JOBS_URL"), "names the env var: {p}");
            assert!(
                p.contains("BOSS_TRAIN_ALLOW_LOCAL_JOBS"),
                "names the override: {p}"
            );
            assert!(
                p.contains("system of record"),
                "names the incident class: {p}"
            );
        }
    }

    #[test]
    fn the_allowance_and_remote_jobs_urls_pass_the_sentinel() {
        // The allowance is the deliberate test/demo-box escape hatch.
        assert!(local_jobs_problem("http://127.0.0.1:7900", true).is_none());
        assert!(local_jobs_problem("http://10.20.0.15:7900", false).is_none());
        assert!(local_jobs_problem("https://jobs.boss.internal/api", false).is_none());
    }
}
