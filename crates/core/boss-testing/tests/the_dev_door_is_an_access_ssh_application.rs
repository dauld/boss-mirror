//! The dev door is a Cloudflare Access SSH application with short-lived
//! certificates (design 5fc71f03, David 2026-09-18, all three as
//! proposed; backlog e4cedb46).
//!
//! Until this, the only way into the dev workspace was `ssh
//! root@10.20.0.35` — a LAN address on the MetalLB VIP, reachable from
//! outside only through the boss-gcp WireGuard bastion, authorised by
//! one long-lived ed25519 key an operator had loaded into a Secret by
//! hand. The door now runs where every other public name runs: a
//! hostname on the zone, a route on the in-cluster tunnel, an Access
//! application in front of it, and a certificate the edge issues for
//! the session rather than a key that lives forever.
//!
//! Pinned by reading the four declarations, because each of them is
//! the definition of one half of the door and the halves only work
//! together: a hostname with no route answers 530 (fd75c641, the IdP),
//! a route with no Access application exposes the workspace, and an
//! sshd that does not trust the Access CA refuses the certificate the
//! edge issues.

use boss_testing::repo_root;

/// The one hostname, spelled once here and compared against every file
/// that must agree about it.
const DOOR: &str = "dev.algedonic.dev";

fn read(path: &str) -> String {
    std::fs::read_to_string(repo_root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn the_pod_runs_openssh_with_the_access_ca_as_its_trusted_user_ca() {
    let m = read("infra/cluster/manifests/boss-dev.yaml");
    // The history stays in the manifest's prose — it is the reason the
    // capability set reads the way it does. What must be gone is the
    // running door: Dropbear cannot verify a certificate against a
    // user CA, which is the whole of the Access SSH flow.
    assert!(
        !m.contains("dropbear-bin") && !m.contains("dropbear -r"),
        "the pod must neither install nor start Dropbear any more"
    );
    assert!(
        m.contains("--no-install-recommends openssh-server"),
        "the door needs OpenSSH sshd; its postinst is also what creates the privsep user the invocation needs"
    );
    assert!(
        m.contains("/usr/sbin/sshd -f /work/ssh/sshd_config -E /work/ssh/sshd.log"),
        "the measured invocation: its own config and its own log on the PVC, both outside the read-only image"
    );
    assert!(
        m.contains("TrustedUserCAKeys /etc/access-ssh-ca/ca.pub"),
        "sshd must trust the Access SSH CA, or the short-lived certificate the edge issues is refused"
    );
}

#[test]
fn the_capability_set_carries_the_chroot_the_privsep_child_makes() {
    let m = read("infra/cluster/manifests/boss-dev.yaml");
    // Measured in the live pod 2026-09-20: CapEff 0x00...c0 — CAP_SETGID
    // and CAP_SETUID and nothing else. sshd's preauth privsep child
    // chroots to /run/sshd, which is why Dropbear was chosen in
    // 2026-08-30 and why the swap costs exactly this one capability.
    assert!(
        m.contains(r#"add: ["SETGID", "SETUID", "SYS_CHROOT", "AUDIT_WRITE"]"#),
        "sshd's preauth privsep child chroots; without SYS_CHROOT it dies there, which is the history this manifest already carried"
    );
}

#[test]
fn the_capability_set_carries_the_audit_write_a_pty_login_makes() {
    // Measured 2026-09-23 against a debug sshd in the live pod: every
    // interactive login authenticated, allocated /dev/pts/N, then died
    // at "linux_audit_write_entry failed: Operation not permitted" —
    // Debian's sshd records a pty login to the kernel audit subsystem
    // and treats EPERM there as fatal. `ssh -T` allocates no pty, so it
    // worked, and that is all the swap from Dropbear was tried with.
    let m = read("infra/cluster/manifests/boss-dev.yaml");
    let caps = m
        .lines()
        .find(|l| l.contains("add: [\"SETGID\""))
        .unwrap_or_default();
    assert!(
        caps.contains("\"AUDIT_WRITE\""),
        "a pty login writes an audit record and sshd ends the session when it cannot: {caps}"
    );
}

#[test]
fn the_access_ca_is_a_named_secret_the_root_ceremony_fills() {
    let m = read("infra/cluster/manifests/boss-dev.yaml");
    assert!(
        m.contains("secretName: boss-dev-access-ssh-ca"),
        "the CA public key is mounted from a Secret by name — values never live in the tree"
    );
    assert!(
        m.contains("{name: access-ssh-ca, mountPath: /etc/access-ssh-ca, readOnly: true}"),
        "mounted where the sshd_config's TrustedUserCAKeys names it"
    );
}

#[test]
fn the_tunnel_routes_the_door_to_the_pods_ssh_service() {
    let origins = read("infra/cluster/tunnel-origins.toml");
    assert!(
        origins.contains(&format!("hostname = \"{DOOR}\"")),
        "the door is not an instance, so its route is declared in tunnel-origins.toml"
    );
    assert!(
        origins.contains("service = \"ssh://boss-dev-ssh.boss-dev.svc.cluster.local:22\""),
        "the origin is the pod's own ssh Service, in-cluster, by name"
    );
    // The committed connector config is the render of that file
    // (render-tunnel-config.sh, held equal by
    // the_tunnel_connector_runs_in_the_cluster.rs) — so the route has
    // to be IN it, not merely declared beside it.
    let rendered = read("infra/cluster/manifests/cloudflared-config.yaml");
    assert!(
        rendered.contains(&format!("- hostname: {DOOR}")),
        "re-render with infra/cluster/render-tunnel-config.sh --write — a declared route the connector does not carry answers 404 at the edge"
    );
}

#[test]
fn the_zone_declares_the_door_as_a_cname_interlocked_on_the_tunnel() {
    let zone = read("infra/cluster/dns/algedonic.dev.toml");
    let entry = zone
        .split("[[record]]")
        .find(|r| r.contains(&format!("name = \"{DOOR}\"")))
        .expect("algedonic.dev.toml must declare the door's record");
    assert!(
        entry.contains("type = \"CNAME\"") && entry.contains("target = \"tunnel:"),
        "the door follows the tunnel the way id. does, by reference, so a rotation cannot strand it"
    );
    assert!(
        entry.contains(r#"interlock = "tunnel""#),
        "applied only once the converge records the route — a CNAME ahead of its route is the 530 of 2026-09-16"
    );
}

#[test]
fn access_declares_an_ssh_application_in_front_of_the_door() {
    let access = read("infra/cluster/dns/access.toml");
    let app = access
        .split("[[application]]")
        .find(|a| a.contains(&format!("domain = \"{DOOR}\"")))
        .expect("access.toml must declare the application in front of the door");
    assert!(
        app.contains(r#"type = "ssh""#),
        "an SSH application, not a self_hosted one: the type is what makes the edge issue a certificate rather than a cookie"
    );
    assert!(
        app.contains(r#"decision = "allow""#),
        "the interlock in front of every other name asks for an allow policy; the door is no different"
    );
}

#[test]
fn the_estate_page_spells_the_same_hostname_as_the_route() {
    // CLAUDE.md §9a: the hostname lives in the tree twice — the route
    // the connector serves and the block the page prints — because the
    // jobs API serves no read of either. Collapsing it needs the
    // estate reader (d471a8ce); until then it is pinned here, and this
    // test names the file that is wrong when they drift.
    let page = read("apps/web/src/it/estate/estate.ts");
    assert!(
        page.contains(&format!("'{DOOR}'")),
        "apps/web/src/it/estate/estate.ts must name the same hostname infra/cluster/tunnel-origins.toml routes"
    );
    assert!(
        !page.contains("10.20.0.35"),
        "the hardcoded LAN door goes with this car: the page prints the Access terminal block instead"
    );
    assert!(
        !page.contains("bastionRoutes"),
        "the bastion card goes too — the way in from outside is the door itself now, not a jump through boss-gcp"
    );
}

#[test]
fn the_host_key_is_moded_before_sshd_reads_it() {
    // Incident 55d001b0, 2026-09-22: the pod's fsGroup (1500) re-modes
    // every file on the PVC to 0660 at mount, OpenSSH refuses a group-
    // readable host key ("no hostkeys available -- exiting"), and both
    // doors were dark from the first boot after Dropbear left — which
    // never checked the mode. The boot path already owned this for the
    // bastion key in /work/.ssh; the host key in /work/ssh is the same
    // fact, and the mode must be set BEFORE sshd reads the key.
    let m = read("infra/cluster/manifests/boss-dev.yaml");
    let sshd = m
        .find("/usr/sbin/sshd -f /work/ssh/sshd_config")
        .expect("the sshd invocation");
    let chmod = m
        .find("chmod 600 /work/ssh/ssh_host_ed25519_key")
        .expect("the boot path must set the host key's mode — the fsGroup re-modes it to 0660 at every mount");
    assert!(
        chmod < sshd,
        "the host key's chmod must run before sshd reads the key, not after"
    );
}

#[test]
fn the_boot_log_says_whether_sshd_came_up() {
    // The same incident's boot log printed "the LAN key door still
    // works" before sshd had started, and sshd then exited. A door
    // claim is a measurement of the running daemon, not of the config:
    // a refused start must say so in the log and carry sshd's own words.
    let m = read("infra/cluster/manifests/boss-dev.yaml");
    assert!(
        !m.contains("the LAN key door still works"),
        "the CA check runs before sshd starts, so it cannot know whether any door works"
    );
    assert!(
        m.contains("ssh door: CLOSED"),
        "a refused sshd start must be named in the boot log"
    );
    assert!(
        m.contains("tail -n 20 /work/ssh/sshd.log"),
        "the refusal carries sshd's own words, not a paraphrase"
    );
}

#[test]
fn the_access_principal_may_log_in_as_root() {
    // Cloudflare issues the short-lived certificate for the user's EMAIL
    // PREFIX (david@algedonic.dev → principal `david`), and with no
    // principals file sshd requires that principal to equal the login
    // name — so `ssh root@dev.algedonic.dev`, what /it/estate prints,
    // was refused by construction (incident 55d001b0). The mapping lists
    // exactly the prefixes access.toml's policy lets through the edge:
    // two spellings of one fact, pinned here (CLAUDE.md §9a). The file
    // lives under /etc/ssh, not the PVC: StrictModes refuses a
    // principals file below a group-writable directory, and the pod's
    // fsGroup makes every PVC directory one.
    let m = read("infra/cluster/manifests/boss-dev.yaml");
    assert!(
        m.contains("AuthorizedPrincipalsFile /etc/ssh/authorized_principals/%u"),
        "sshd must map certificate principals to a login name"
    );
    let line = m
        .lines()
        .map(str::trim)
        .find(|l| l.ends_with("> /etc/ssh/authorized_principals/root"))
        .expect("the boot path must write root's principals file");
    let mut written: Vec<&str> = line
        .trim_start_matches("printf '%s\\n'")
        .trim_end_matches("> /etc/ssh/authorized_principals/root")
        .split_whitespace()
        .collect();
    written.sort_unstable();

    let access = read("infra/cluster/dns/access.toml");
    let app = access
        .split("[[application]]")
        .find(|a| a.contains(&format!("domain = \"{DOOR}\"")))
        .expect("the door's Access application");
    let emails = app
        .lines()
        .find(|l| l.trim_start().starts_with("include.emails"))
        .expect("the door's allow policy names its emails");
    let mut prefixes: Vec<&str> = emails
        .split('"')
        .filter(|s| s.contains('@'))
        .filter_map(|e| e.split('@').next())
        .collect();
    prefixes.sort_unstable();
    assert_eq!(
        written, prefixes,
        "root's principals (infra/cluster/manifests/boss-dev.yaml) must be the email prefixes infra/cluster/dns/access.toml admits to {DOOR}"
    );
}

#[test]
fn the_estate_page_asks_for_a_short_lived_certificate() {
    // Without --short-lived-cert the stanza cloudflared writes carries
    // no CertificateFile, and ssh offers no certificate at all.
    let page = read("apps/web/src/it/estate/estate.ts");
    assert!(
        page.contains("cloudflared access ssh-config --hostname ${host} --short-lived-cert"),
        "apps/web/src/it/estate/estate.ts must print the ssh-config command with --short-lived-cert"
    );
}
