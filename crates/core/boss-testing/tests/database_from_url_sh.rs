//! boss-init reads its database name off DATABASE_URL — the Secret's
//! value every service opens — through
//! `infra/oss-quickstart/database-from-url.sh`, and the manifest carries
//! no PGDATABASE literal beside it. Until 2026-09-16 it did (`boss`),
//! and the day the database is repointed (Option 3, design e652c7c6;
//! switch-instance-database 063dba4e) the schema would have converged
//! into one database while the services opened the other.

use boss_testing::repo_root;
use std::process::Command;

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new("sh")
        .arg(repo_root().join("infra/oss-quickstart/database-from-url.sh"))
        .args(args)
        .output()
        .expect("sh runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_database_is_the_urls_path_query_and_slash_stripped() {
    let (rc, out, _) = run(&["postgres://boss:s3cret@postgres:5432/algedonic"]);
    assert_eq!((rc, out.as_str()), (0, "algedonic\n"));
    let (rc, out, _) = run(&["postgres://boss:s3cret@postgres:5432/boss?sslmode=disable"]);
    assert_eq!((rc, out.as_str()), (0, "boss\n"));
}

#[test]
fn an_empty_path_or_no_url_refuses_with_ex_config_and_never_guesses() {
    for args in [vec!["postgres://boss:s3cret@postgres:5432/"], vec![]] {
        let (rc, out, err) = run(&args);
        assert_eq!(rc, 78, "{err}");
        assert!(out.is_empty(), "nothing printed on a refusal: {out:?}");
        assert!(err.contains("database-from-url"), "{err}");
    }
}

#[test]
fn boss_init_derives_the_database_and_the_manifest_carries_no_literal() {
    let init = std::fs::read_to_string(repo_root().join("infra/oss-quickstart/init.sh")).unwrap();
    assert!(
        init.contains("database-from-url.sh\" \"$DATABASE_URL\""),
        "init.sh derives PGDATABASE from DATABASE_URL through the helper"
    );
    let boss_yaml =
        std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss.yaml")).unwrap();
    assert!(
        !boss_yaml.contains("name: PGDATABASE"),
        "the manifest must not name the database twice: DATABASE_URL (the Secret) is the one source"
    );
    assert!(
        boss_yaml.contains("name: DATABASE_URL"),
        "boss-init still reads DATABASE_URL"
    );
    // The image carries the helper where init.sh calls it ($REPO/infra/...).
    let dockerfile =
        std::fs::read_to_string(repo_root().join("infra/oss-quickstart/Dockerfile")).unwrap();
    assert!(
        dockerfile.contains("COPY infra/oss-quickstart/database-from-url.sh /opt/boss/infra/oss-quickstart/database-from-url.sh"),
        "the helper rides the image beside init.sh's $REPO"
    );
}
