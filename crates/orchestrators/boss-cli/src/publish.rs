//! `boss publish <branch>` — getting a branch onto the forge is one
//! verb, not a two-hop dance retyped per car.
//!
//! WHY THIS EXISTS (filed 61ab94dc). Every car needs this: `boss gate`
//! resolves the head it tests from the FORGE, so a branch that only
//! exists on a workstation cannot be gated. A workstation has no forge
//! credential, so the branch travels in two hops — push to the
//! conductor's clone under a temp ref, then push that to the forge from
//! there, then delete the temp ref.
//!
//! IT IS DONE BY HAND TODAY, AND IT IS EXACTLY THE SHAPE THAT BREAKS.
//! On 2026-08-28 one of two publishes was silently corrupted: in zsh,
//! `$B:refs/heads/$B` applies `:r` as a PARAMETER MODIFIER rather than
//! reading the colon literally, so the refspec became
//! `refs/tmp/<branch>efs/heads/<branch>` and git rejected it with a
//! confusing "does not match any". Worse, the `&&` chain deleted the
//! temp ref anyway and reported success, so the whole thing had to be
//! redone. A verb has no argv for a shell to mangle.
//!
//! WHAT IT IS NOT. `train::publish_car_branch` already publishes a
//! branch that is ALREADY IN the conductor's clone, and the conductor
//! calls it during assembly. That is the second hop only. This verb
//! covers the first hop as well — the one a person or agent actually
//! has to perform — and then verifies.
//!
//! THE VERIFICATION IS THE POINT, not the typing. A push's exit code
//! says the transfer succeeded, not that the forge now holds the commit
//! you meant. So the verb finishes by asking the forge what it has and
//! comparing it to the local head, and REFUSES loudly when they differ.
//! That is the check the by-hand version skipped, and skipping it is
//! how a gate ends up testing a commit nobody intended.

use anyhow::{Result, bail};

/// Where the conductor's clone lives, derived from a git remote URL so
/// the host and path are stated once rather than duplicated in config.
///
/// A fact that lives twice drifts; the remote already knows both halves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Clone {
    /// ssh host, e.g. `boss-gcp`.
    pub host: String,
    /// Path to the bare/working clone on that host.
    pub path: String,
}

/// Parse `ssh://host/path/to/repo` into its two halves.
///
/// Only the ssh form is accepted. A workstation reaching the conductor
/// over anything else is a setup this verb has never been tested
/// against, and guessing would push a branch somewhere unintended.
pub(crate) fn parse_clone(url: &str) -> Result<Clone> {
    let rest = url.strip_prefix("ssh://").ok_or_else(|| {
        anyhow::anyhow!(
            "the conductor remote is {url:?}, which is not an ssh:// URL. This verb pushes to \
             the conductor's clone and then runs git THERE over ssh, so it needs a host it can \
             reach; it will not guess one."
        )
    })?;
    let (host, path) = rest.split_once('/').ok_or_else(|| {
        anyhow::anyhow!("the conductor remote {url:?} names a host but no repository path")
    })?;
    if host.is_empty() || path.is_empty() {
        bail!("the conductor remote {url:?} is missing a host or a path");
    }
    Ok(Clone {
        host: host.to_string(),
        // strip_prefix ate the separator; put it back.
        path: format!("/{path}"),
    })
}

/// The temp ref a branch travels under. One place, so the two hops and
/// the cleanup cannot disagree about the name.
pub(crate) fn temp_ref(branch: &str) -> String {
    format!("refs/tmp/{branch}")
}

/// The refspec for the second hop, built as one string.
///
/// This exists as a named function with a test because building it by
/// string interpolation in a shell is what broke on 2026-08-28. Rust
/// has no `:r` modifier, but the value still has to be RIGHT, and a
/// test is cheaper than rediscovering it from a git error.
pub(crate) fn forge_refspec(branch: &str) -> String {
    format!("{}:refs/heads/{branch}", temp_ref(branch))
}

/// Refuse branch names that would make a refspec ambiguous or a shell
/// argument dangerous. `boss gate` and the conductor both take this
/// name straight into git plumbing.
pub(crate) fn check_branch(branch: &str) -> Result<()> {
    if branch.is_empty() {
        bail!("no branch given");
    }
    let bad = [
        ' ', '\t', '\n', ':', '\\', '\'', '"', '`', '$', ';', '&', '|',
    ];
    if let Some(c) = branch.chars().find(|c| bad.contains(c)) {
        bail!(
            "branch name {branch:?} contains {c:?}, which git refspecs and shells both read as \
             punctuation. Rename the branch."
        );
    }
    if branch.starts_with('-') {
        bail!("branch name {branch:?} would be read as a flag by git");
    }
    Ok(())
}

/// Did the forge end up holding exactly the commit we meant?
///
/// Takes the two shas rather than fetching them, so the comparison is
/// testable and the fetching stays at the edge.
pub(crate) fn verify(local: &str, on_forge: &str, branch: &str) -> Result<()> {
    if on_forge.is_empty() {
        bail!(
            "pushed {branch}, but the forge reports no such branch. The push claimed success and \
             the forge disagrees; do not gate this branch until that is explained."
        );
    }
    if local != on_forge {
        bail!(
            "pushed {branch}, but the forge holds {} while this clone is at {}.\n  \
             A gate launched now would test the forge's commit, not yours. This is what a \
             mangled refspec or a concurrent push looks like.",
            &on_forge[..12.min(on_forge.len())],
            &local[..12.min(local.len())],
        );
    }
    Ok(())
}

fn git(args: &[&str]) -> Result<std::process::Output> {
    crate::git_auth::command()
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("could not run git {args:?}: {e}"))
}

fn out(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// Publish `branch` (at the local head) to the forge, and verify it.
pub(crate) async fn run(branch: &str, remote: &str, dry: bool) -> Result<()> {
    check_branch(branch)?;

    let url = git(&["remote", "get-url", remote])?;
    if !url.status.success() {
        bail!(
            "no git remote named {remote:?} in this clone. That remote is the conductor's clone \
             — the only route a workstation has to the forge, since it holds no forge credential."
        );
    }
    let clone = parse_clone(&out(&url))?;

    let head = git(&["rev-parse", "HEAD"])?;
    if !head.status.success() {
        bail!("could not resolve HEAD in this clone");
    }
    let head = out(&head);

    let tmp = temp_ref(branch);
    let refspec = forge_refspec(branch);

    if dry {
        println!(
            "boss publish: DRY would push {} -> {}:{tmp}, then {}:{refspec}, then delete {tmp}",
            &head[..12.min(head.len())],
            remote,
            clone.host
        );
        return Ok(());
    }

    // Hop 1: workstation -> conductor clone, under a temp ref.
    println!(
        "boss publish: {branch} @ {} -> {remote}:{tmp}",
        &head[..12.min(head.len())]
    );
    let push1 = git(&["push", "-f", remote, &format!("HEAD:{tmp}")])?;
    if !push1.status.success() {
        bail!(
            "could not push to the conductor clone:\n  {}",
            String::from_utf8_lossy(&push1.stderr).trim()
        );
    }

    // Hop 2: conductor clone -> forge. Run git there over ssh. The
    // temp ref is deleted whether or not the push worked, so a failed
    // publish does not leave refs/tmp litter behind — but the push's
    // status is captured FIRST so cleanup cannot mask a failure, which
    // is precisely what the by-hand `&&` chain got wrong.
    let remote_cmd = format!(
        "cd {} && git push -f origin '{}'; rc=$?; git update-ref -d '{}'; exit $rc",
        clone.path, refspec, tmp
    );
    let push2 = std::process::Command::new("ssh")
        .args([&clone.host, &remote_cmd])
        .output()
        .map_err(|e| anyhow::anyhow!("could not ssh to {}: {e}", clone.host))?;
    if !push2.status.success() {
        bail!(
            "the conductor clone could not push {branch} to the forge:\n  {}",
            String::from_utf8_lossy(&push2.stderr).trim()
        );
    }

    // Verify by effect. The push said it worked; ask the forge.
    let ls = std::process::Command::new("ssh")
        .args([
            &clone.host,
            &format!("cd {} && git ls-remote origin '{branch}'", clone.path),
        ])
        .output()
        .map_err(|e| anyhow::anyhow!("could not ssh to {}: {e}", clone.host))?;
    let on_forge = String::from_utf8_lossy(&ls.stdout)
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or("")
        .to_string();
    verify(&head, &on_forge, branch)?;

    println!(
        "boss publish: forge holds {branch} @ {} — verified, not assumed",
        &on_forge[..12.min(on_forge.len())]
    );
    println!(
        "boss publish: next — boss gate {branch} --wait   (run it on {}, where origin is the forge)",
        clone.host
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ssh_remote_splits_into_host_and_path() {
        let c = parse_clone("ssh://boss-gcp/var/lib/boss-train/repo").unwrap();
        assert_eq!(c.host, "boss-gcp");
        assert_eq!(c.path, "/var/lib/boss-train/repo");
    }

    #[test]
    fn a_non_ssh_remote_is_refused_rather_than_guessed() {
        let e = parse_clone("http://10.20.0.15:3000/david/boss.git")
            .unwrap_err()
            .to_string();
        assert!(e.contains("not an ssh:// URL"), "{e}");
    }

    /// THE BUG THIS VERB EXISTS TO REMOVE. In zsh `$B:refs/heads/$B`
    /// applies `:r` as a parameter modifier and mangles the refspec.
    /// Rust interpolation cannot do that — this pins the correct value
    /// so the fix cannot silently regress.
    #[test]
    fn the_refspec_is_not_mangled() {
        assert_eq!(
            forge_refspec("feat/proof-is-a-receipt-not-a-claim"),
            "refs/tmp/feat/proof-is-a-receipt-not-a-claim:refs/heads/feat/proof-is-a-receipt-not-a-claim"
        );
        // The 2026-08-28 corruption ATE THE COLON, yielding
        // `refs/tmp/feat/xefs/heads/feat/x`. So the signature to pin is
        // that the destination is introduced by exactly one colon —
        // NOT the absence of "efs/heads", which is a substring of the
        // correct "refs/heads" and made this assertion fail against
        // perfectly good output.
        let spec = forge_refspec("feat/x");
        assert_eq!(
            spec.matches(':').count(),
            1,
            "one colon separates src from dst: {spec}"
        );
        assert!(
            spec.contains(":refs/heads/"),
            "dst must be a full ref: {spec}"
        );
    }

    #[test]
    fn the_temp_ref_and_the_refspec_agree() {
        let b = "fix/a-thing";
        assert!(forge_refspec(b).starts_with(&format!("{}:", temp_ref(b))));
    }

    #[test]
    fn a_branch_name_with_shell_punctuation_is_refused() {
        for bad in ["a b", "a:b", "a;rm -rf /", "a$b", "a`b`", "a'b", "a|b"] {
            assert!(check_branch(bad).is_err(), "should refuse {bad:?}");
        }
        assert!(check_branch("feat/a-normal-name_1.2").is_ok());
    }

    #[test]
    fn a_branch_name_that_looks_like_a_flag_is_refused() {
        assert!(check_branch("--force").is_err());
    }

    /// The verification the by-hand version skipped.
    #[test]
    fn a_forge_holding_a_different_commit_is_refused() {
        let e = verify("a".repeat(40).as_str(), "b".repeat(40).as_str(), "feat/x")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("would test the forge's commit, not yours"),
            "{e}"
        );
    }

    #[test]
    fn a_forge_missing_the_branch_is_refused() {
        let e = verify("a".repeat(40).as_str(), "", "feat/x")
            .unwrap_err()
            .to_string();
        assert!(e.contains("no such branch"), "{e}");
    }

    #[test]
    fn matching_shas_verify() {
        let sha = "a".repeat(40);
        assert!(verify(&sha, &sha, "feat/x").is_ok());
    }
}
