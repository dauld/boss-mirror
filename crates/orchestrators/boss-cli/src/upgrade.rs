use anyhow::{Context, Result, bail};
use std::env;
use tokio::process::Command;

/// GitHub repo to pull releases from. Build-time override via
/// `BOSS_REPO` env var so a fork / downstream build can self-update
/// from its own release stream without forking the source.
const REPO: &str = match option_env!("BOSS_REPO") {
    Some(r) => r,
    None => "algedonic-dev/boss",
};

fn artifact_name(os: &str, arch: &str) -> Result<String> {
    let os_part = match os {
        "linux" => "linux",
        "macos" => "darwin",
        _ => bail!("unsupported OS: {os}"),
    };
    let arch_part = match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => bail!("unsupported architecture: {arch}"),
    };
    Ok(format!("boss-{os_part}-{arch_part}"))
}

fn detect_platform() -> Result<String> {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;
    artifact_name(os, arch)
}

pub async fn run() -> Result<()> {
    println!("Boss Upgrade");
    println!("──────────────");

    let current_version = env!("CARGO_PKG_VERSION");
    println!("\n  Current version: {current_version}");

    let artifact = detect_platform()?;
    println!("  Platform:        {artifact}");

    print!("  Downloading latest release...");
    let tmp_dir = env::temp_dir();
    let tmp_path = tmp_dir.join(&artifact);

    let output = Command::new("gh")
        .args([
            "release",
            "download",
            "--repo",
            REPO,
            "--pattern",
            &artifact,
            "--dir",
            tmp_dir.to_str().unwrap_or("/tmp"),
            "--clobber",
        ])
        .output()
        .await
        .context("failed to run gh CLI — is it installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("download failed: {stderr}");
    }
    println!(" done");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // mode-bits-ok: a downloaded binary renamed over the CLI, never exec'd by this process
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))
            .context("failed to set executable permission")?;
    }

    let current_exe = env::current_exe().context("failed to determine current executable path")?;
    println!("  Installing to:   {}", current_exe.display());

    // mv atomically replaces the inode, avoiding "text file busy" on a running binary.
    // Try directly first, fall back to sudo if permission denied.
    let direct = std::process::Command::new("mv")
        .arg("-f")
        .arg(&tmp_path)
        .arg(&current_exe)
        .status();

    match direct {
        Ok(s) if s.success() => {}
        _ => {
            println!("  Elevating with sudo...");
            let status = Command::new("sudo")
                .arg("mv")
                .arg("-f")
                .arg(&tmp_path)
                .arg(&current_exe)
                .status()
                .await
                .context("failed to run sudo mv")?;
            if !status.success() {
                bail!(
                    "could not install to {} — check permissions",
                    current_exe.display()
                );
            }
        }
    }

    let version_output = Command::new(&current_exe)
        .arg("--version")
        .output()
        .await
        .context("failed to verify new version")?;

    let new_version = String::from_utf8_lossy(&version_output.stdout);
    println!("  New version:     {}", new_version.trim());
    println!("\n  Upgrade complete.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_name_linux_x86() {
        assert_eq!(
            artifact_name("linux", "x86_64").unwrap(),
            "boss-linux-amd64"
        );
    }

    #[test]
    fn artifact_name_linux_arm() {
        assert_eq!(
            artifact_name("linux", "aarch64").unwrap(),
            "boss-linux-arm64"
        );
    }

    #[test]
    fn artifact_name_macos_arm() {
        assert_eq!(
            artifact_name("macos", "aarch64").unwrap(),
            "boss-darwin-arm64"
        );
    }

    #[test]
    fn artifact_name_macos_x86() {
        assert_eq!(
            artifact_name("macos", "x86_64").unwrap(),
            "boss-darwin-amd64"
        );
    }

    #[test]
    fn artifact_name_unsupported_os() {
        assert!(artifact_name("windows", "x86_64").is_err());
    }

    #[test]
    fn artifact_name_unsupported_arch() {
        assert!(artifact_name("linux", "mips").is_err());
    }

    #[test]
    fn detect_platform_succeeds() {
        // Should succeed on any CI/dev machine we support
        assert!(detect_platform().is_ok());
    }
}
