//! Every HTTP server in the tree mounts the machine gate, and every
//! probe a reader outside the token can send is exempt on the service
//! it targets (design 6805c764, car 1; backlog 2710c8fc).
//!
//! WHAT WAS MEASURED (triage of 2710c8fc, origin/main 165619f7,
//! 2026-09-25). 27 binaries serve HTTP and the machine gate was mounted
//! in ONE, the jobs API, where it was dormant besides. Anyone on the LAN
//! or the WireGuard mesh could assert `x-boss-user` at any of the other
//! 26 ports. David's decision is that every service port requires the
//! machine token; the design lands it in five cars, and this pin is
//! the first car's floor: a service that ships without the gate cannot
//! be moved to `report` or `enforce` by the mode file, so a 28th server
//! that forgets it is the hole this item was filed about, reopened in
//! silence.
//!
//! THE SHAPES THIS HOLDS.
//!
//!   1. Every file under `crates/` that calls `axum::serve(` outside its
//!      `#[cfg(test)]` region calls `machine_gate::mount(` at least as
//!      many times — or is named in [`UNGATED`] with the reason. A file
//!      its parent declares only as `#[cfg(test)] mod …;` is all test
//!      region (`test_only_modules`).
//!   2. Every mount names its service by its `boss-ports` name, so the
//!      probe check below can find the mount a port leads to.
//!   3. Every `httpGet` probe in `infra/cluster/manifests/` whose port is
//!      a `boss-ports` service, and every path the off-cluster watchdog
//!      (`infra/forge/cluster-watchdog.sh`) reads from the jobs API, is
//!      in that service's exact-path exemption list. A probe answered
//!      401 restarts pods; a watchdog answered 401 goes blind, and an arm
//!      that needs the patient is not an arm (CLAUDE.md §Diagnosis). This
//!      is a fact that lives twice — the probe in the manifest, the
//!      exemption in the binary — so it is pinned (CLAUDE.md §9a).

use boss_testing::repo_root;
use regex::Regex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Servers that deliberately do NOT mount the gate, each with the
/// reason. A stale row (the file no longer serves) fails the pin too.
const UNGATED: &[(&str, &str, &str)] = &[(
    "crates/core/boss-gateway/src/main.rs",
    "gateway",
    "the edge, not a machine door: its first act on every request is the edge strip \
     (role_headers.rs strip_boss_headers), which removes every inbound x-boss-* header \
     INCLUDING x-boss-machine-token, so it trusts no asserted identity and a gate on it \
     could only ever refuse the browsers it exists to serve. The gateway's side of the \
     design is stamping the token on what it forwards (car 2).",
)];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n == "target" || n == "node_modules" || n == "tests")
            {
                continue;
            }
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The part of a source file before its first `#[cfg(test)]` that opens
/// an inline module (`mod tests {`) — where a server is production code
/// rather than a test's fixture. A `#[cfg(test)] mod name;` declaration
/// does not open a region here (its file is judged by
/// `test_only_modules`): cutting at the first `#[cfg(test)]` of any kind
/// took the whole of the gateway's `main.rs` out of the scan the day it
/// gained two at its top (2026-09-25).
fn production_part(text: &str) -> &str {
    let inline = Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z0-9_]+\s*\{").unwrap();
    let mut offset = 0;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    for (i, line) in lines.iter().enumerate() {
        if let Some(rest) = line.trim().strip_prefix("#[cfg(test)]") {
            let opens = std::iter::once(rest.trim())
                .chain(lines[i + 1..].iter().map(|l| l.trim()))
                .find(|l| !l.is_empty() && !l.starts_with("#["))
                .is_some_and(|l| inline.is_match(l));
            if opens {
                return &text[..offset];
            }
        }
        offset += line.len();
    }
    text
}

/// Every module a parent declares only under `#[cfg(test)]`, as the
/// paths it can live at: `<name>.rs` and the `<name>/` directory its
/// own children live in (or the `#[path]` it names). A file there is a
/// test's fixture in the same sense as the region `production_part`
/// cuts off — the compiler never builds it into the binary. Found the
/// day the gateway's read-only-session tests landed as `#[cfg(test)]
/// mod` files that serve a recording upstream (car a159e1ee's dock
/// re-gate, 2026-09-25).
fn test_only_modules(files: &[PathBuf]) -> Vec<PathBuf> {
    let decl = Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z0-9_]+)\s*;").unwrap();
    let path_attr = Regex::new(r#"^#\[path\s*=\s*"([^"]+)"\]$"#).unwrap();
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        let parent = f.parent().unwrap_or(Path::new(""));
        // A mod.rs-style file declares children beside it; any other
        // file declares them in the directory named after it.
        let own_dir = match f.file_name().and_then(|n| n.to_str()) {
            Some("main.rs" | "lib.rs" | "mod.rs") => parent.to_path_buf(),
            _ => parent.join(f.file_stem().unwrap_or_default()),
        };
        for (i, line) in lines.iter().enumerate() {
            let Some(rest) = line.strip_prefix("#[cfg(test)]") else {
                continue;
            };
            let mut path = None;
            let after = std::iter::once(rest.trim()).chain(lines[i + 1..].iter().copied());
            for next in after {
                if let Some(c) = path_attr.captures(next) {
                    path = Some(c[1].to_string());
                    continue;
                }
                if next.is_empty() || next.starts_with("#[") {
                    continue;
                }
                if let Some(c) = decl.captures(next) {
                    let file = match &path {
                        Some(p) => parent.join(p),
                        None => own_dir.join(format!("{}.rs", &c[1])),
                    };
                    out.push(file.with_extension(""));
                    out.push(file);
                }
                break;
            }
        }
    }
    out
}

/// `f` is a test-only module file, or lives under one.
fn is_test_file(tests: &[PathBuf], f: &Path) -> bool {
    tests.iter().any(|t| f.starts_with(t))
}

fn mount_re() -> Regex {
    Regex::new(r#"machine_gate::mount\(\s*[^,"]+?\s*,\s*"([^"]+)"\s*,\s*&\[([^\]]*)\]"#).unwrap()
}

/// Every mount in the tree: service name → (file, exempt paths).
fn mounts(root: &Path) -> BTreeMap<String, (String, Vec<String>)> {
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    let re = mount_re();
    let lit = Regex::new(r#""([^"]+)""#).unwrap();
    let tests = test_only_modules(&files);
    let mut out = BTreeMap::new();
    for f in files.iter().filter(|f| !is_test_file(&tests, f)) {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let rel = f.strip_prefix(root).unwrap().to_string_lossy().to_string();
        for c in re.captures_iter(production_part(&text)) {
            let exempt = lit.captures_iter(&c[2]).map(|l| l[1].to_string()).collect();
            let prior = out.insert(c[1].to_string(), (rel.clone(), exempt));
            assert!(
                prior.is_none(),
                "service `{}` is mounted twice ({} and {rel}); one service, one gate",
                &c[1],
                prior.unwrap().0
            );
        }
    }
    out
}

#[test]
fn every_server_outside_a_test_mounts_the_machine_gate() {
    let root = repo_root();
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    let tests = test_only_modules(&files);
    let mut servers = 0;
    let mut offenders = Vec::new();
    let mut ungated_seen = Vec::new();
    for f in files.iter().filter(|f| !is_test_file(&tests, f)) {
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let prod = production_part(&text);
        let serves = prod.matches("axum::serve(").count();
        if serves == 0 {
            continue;
        }
        servers += 1;
        if UNGATED.iter().any(|(p, _, _)| *p == rel) {
            ungated_seen.push(rel);
            continue;
        }
        let gated = prod.matches("machine_gate::mount(").count();
        if gated < serves {
            offenders.push(format!(
                "{rel}: {serves} axum::serve call(s), {gated} machine_gate::mount call(s)"
            ));
        }
    }
    // A control: the scan found the servers it exists to judge. 27
    // binaries served HTTP when this landed; far fewer means the walk
    // or the test-region cut is broken, not that the servers left.
    assert!(
        servers >= 25,
        "found only {servers} production servers under crates/ — the scan is broken"
    );
    assert!(
        offenders.is_empty(),
        "an HTTP server that does not mount boss_core::machine_gate cannot be moved to \
         report or enforce (design 6805c764). Serve through \
         `boss_core::machine_gate::mount(app, \"<boss-ports name>\", &[<exact health paths>])`, \
         or add the file to UNGATED with the reason:\n  {}",
        offenders.join("\n  ")
    );
    for (p, _, _) in UNGATED {
        assert!(
            ungated_seen.iter().any(|s| s == p),
            "UNGATED names {p}, which no longer serves HTTP outside its tests — delete the row"
        );
    }
}

/// A module file whose parent declares it only under `#[cfg(test)]` is
/// a test's fixture, as much as the region after a `#[cfg(test)]`
/// inside a file is. Car a159e1ee's dock re-gate went red on exactly
/// this shape through five trains (2026-09-25): the gateway's
/// `a_read_only_session_cannot_write.rs` and
/// `a_path_cannot_climb_out_of_its_route.rs` each serve a recording
/// upstream, and `main.rs` declares both as `#[cfg(test)] mod …;`.
/// The control — a plain `mod` beside them — stays production, so the
/// cut cannot swallow a real server.
#[test]
fn a_module_declared_only_under_cfg_test_is_a_test_file() {
    let root = boss_testing::scratch_dir("machine-gate-test-modules");
    let src = root.join("crates/core/boss-demo/src");
    boss_testing::create_dir(&src.join("nested"));
    boss_testing::write_file(
        &src.join("main.rs"),
        "#[cfg(test)]\nmod fixture_server;\n#[cfg(test)]\n#[path = \"elsewhere.rs\"]\nmod renamed;\n\
         #[cfg(test)]\nmod nested;\nmod real_server;\n#[cfg(test)]\nmod tests {}\n",
    );
    for f in [
        "fixture_server.rs",
        "elsewhere.rs",
        "real_server.rs",
        "nested/mod.rs",
    ] {
        boss_testing::write_file(&src.join(f), "mod child;\n");
    }
    boss_testing::write_file(&src.join("nested/child.rs"), "");
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    let tests = test_only_modules(&files);
    let judged = |rel: &str| is_test_file(&tests, &src.join(rel));
    assert!(
        judged("fixture_server.rs"),
        "a #[cfg(test)] mod is a test file"
    );
    assert!(
        judged("elsewhere.rs"),
        "a #[cfg(test)] mod behind #[path] is a test file"
    );
    assert!(
        judged("nested/mod.rs"),
        "a #[cfg(test)] mod in a directory is a test file"
    );
    assert!(
        judged("nested/child.rs"),
        "a test module's children are test files"
    );
    assert!(
        !judged("real_server.rs"),
        "THE CONTROL: a plain mod stays production"
    );
    assert!(
        !judged("main.rs"),
        "the declaring file itself stays production"
    );
}

/// The test region of a file starts at its inline `#[cfg(test)] mod …
/// {`, not at the first `#[cfg(test)]` it carries. The gateway's
/// `main.rs` opens with two `#[cfg(test)] mod …;` declarations since
/// 2026-09-25, and cutting there left none of it production: its
/// `axum::serve` vanished from the scan and the UNGATED row read stale.
#[test]
fn a_cfg_test_module_declaration_does_not_end_the_production_part() {
    let text = "#[cfg(test)]\nmod fixture;\n#[cfg(test)] mod other;\n\
                async fn main() { axum::serve(listener, app).await; }\n\
                #[cfg(test)]\n#[allow(dead_code)]\nmod tests {\n    fn f() { axum::serve(l, a); }\n}\n";
    let prod = production_part(text);
    assert_eq!(prod.matches("axum::serve(").count(), 1, "{prod}");
    assert!(!prod.contains("mod tests"), "{prod}");
    assert_eq!(production_part("fn main() {}\n"), "fn main() {}\n");
}

#[test]
fn every_mount_names_a_port_roster_service() {
    let root = repo_root();
    let m = mounts(&root);
    assert!(m.len() >= 25, "found only {} mounts", m.len());
    let names: Vec<&str> = boss_ports::all().map(|s| s.name).collect();
    let unknown: Vec<String> = m
        .iter()
        .filter(|(svc, _)| !names.contains(&svc.as_str()))
        .map(|(svc, (file, _))| format!("{file}: `{svc}`"))
        .collect();
    assert!(
        unknown.is_empty(),
        "a mount names a service boss-ports does not know, so no probe can be matched to it:\n  {}",
        unknown.join("\n  ")
    );
    for path in m.values().flat_map(|(_, e)| e) {
        assert!(
            path.starts_with('/') && !path.ends_with('*'),
            "exemptions are exact paths (design 6805c764 choice 5), not `{path}`"
        );
    }
}

/// `(manifest, path, port)` for every httpGet probe in the manifests.
fn probes(root: &Path) -> Vec<(String, String, String)> {
    let dir = root.join("infra/cluster/manifests");
    let flow = Regex::new(r"httpGet:\s*\{\s*path:\s*([^,}\s]+)\s*,\s*port:\s*([^,}\s]+)").unwrap();
    let block_path = Regex::new(r"^\s*path:\s*(\S+)").unwrap();
    let block_port = Regex::new(r"^\s*port:\s*(\S+)").unwrap();
    let mut out = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for f in entries {
        if !f.extension().is_some_and(|x| x == "yaml" || x == "yml") {
            continue;
        }
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(&f).unwrap();
        for c in flow.captures_iter(&text) {
            out.push((name.clone(), c[1].to_string(), c[2].to_string()));
        }
        let lines: Vec<&str> = text.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            if l.trim() != "httpGet:" {
                continue;
            }
            let window = &lines[i + 1..lines.len().min(i + 4)];
            let path = window
                .iter()
                .find_map(|w| block_path.captures(w).map(|c| c[1].to_string()));
            let port = window
                .iter()
                .find_map(|w| block_port.captures(w).map(|c| c[1].to_string()));
            match (path, port) {
                (Some(p), Some(q)) => out.push((name.clone(), p, q)),
                _ => panic!("{name}:{}: an httpGet probe this pin cannot read", i + 1),
            }
        }
    }
    out
}

fn port_number(root: &Path, manifest: &str, port: &str) -> u16 {
    if let Ok(n) = port.parse() {
        return n;
    }
    let text =
        std::fs::read_to_string(root.join("infra/cluster/manifests").join(manifest)).unwrap();
    let re = Regex::new(&format!(
        r"containerPort:\s*(\d+)\s*,\s*name:\s*{}\b",
        regex::escape(port)
    ))
    .unwrap();
    re.captures(&text)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or_else(|| panic!("{manifest}: probe port `{port}` names no containerPort"))
}

#[test]
fn every_probe_and_the_watchdog_read_are_exempt_where_they_land() {
    let root = repo_root();
    let m = mounts(&root);
    let mut needs: Vec<(String, String, String)> = Vec::new(); // (service, path, from)

    let found = probes(&root);
    assert!(
        !found.is_empty(),
        "no httpGet probe found in infra/cluster/manifests — the gateway's readiness probe \
         existed when this landed, so the parse is broken"
    );
    for (manifest, path, port) in found {
        let n = port_number(&root, &manifest, &port);
        if let Some(spec) = boss_ports::all().find(|s| s.prod == n || s.scratch == Some(n)) {
            needs.push((
                spec.name.to_string(),
                path,
                format!("{manifest} httpGet port {port}"),
            ));
        }
    }

    let watchdog = "infra/forge/cluster-watchdog.sh";
    let text = std::fs::read_to_string(root.join(watchdog)).unwrap();
    let re = Regex::new(r"\$\{?JOBS_API\}?(/[A-Za-z0-9/_.-]+)").unwrap();
    let reads: Vec<String> = re.captures_iter(&text).map(|c| c[1].to_string()).collect();
    assert!(
        !reads.is_empty(),
        "{watchdog} reads no $JOBS_API path — it read /api/jobs/health when this landed, so \
         the parse is broken"
    );
    for path in reads {
        needs.push(("jobs".to_string(), path, watchdog.to_string()));
    }

    let mut missing = Vec::new();
    for (svc, path, from) in needs {
        if UNGATED.iter().any(|(_, s, _)| *s == svc) {
            continue;
        }
        match m.get(&svc) {
            Some((_, exempt)) if exempt.contains(&path) => {}
            Some((file, _)) => missing.push(format!(
                "{from} reads {path} on `{svc}`, which {file} does not exempt"
            )),
            None => missing.push(format!(
                "{from} reads {path} on `{svc}`, and no machine_gate::mount names `{svc}`"
            )),
        }
    }
    assert!(
        missing.is_empty(),
        "a reader that holds no token would be refused once the mode leaves `off` \
         (design 6805c764 choice 5):\n  {}",
        missing.join("\n  ")
    );
}
