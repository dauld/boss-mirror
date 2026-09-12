//! boss-codebase-metrics.service runs as the owner of the checkout it
//! reads. With no `User=` the unit ran as root, and root reading a
//! checkout owned by another user is what git calls "dubious ownership":
//! on 2026-09-12 05:14Z the timer's first-ever firing exited 3 with
//! `fatal: detected dubious ownership in repository at '/opt/boss'`,
//! the cadence stayed silent (436a2e91), the unit read unhealthy
//! (fdd10ec8), and two landed cars waited on a packet that could not
//! exist. Read off the packet the ops-runner answered (988bb127), not
//! an ssh session. The siblings that run git against /opt/boss
//! (boss-search-reindex, boss-forge-token-audit) already say User=david;
//! this pins that this one does too, and says why in the unit.

use boss_testing::repo_root;

#[test]
fn the_codebase_metrics_unit_runs_as_the_checkout_owner() {
    let unit = std::fs::read_to_string(repo_root().join("infra/boss-codebase-metrics.service"))
        .expect("infra/boss-codebase-metrics.service");
    assert!(
        unit.lines().any(|l| l.trim() == "User=david"),
        "boss-codebase-metrics.service must run as the checkout owner (User=david): \
         root reading /opt/boss is 'dubious ownership' and the measurement exits 3"
    );
    assert!(
        unit.contains("dubious ownership"),
        "the unit names the refusal the User= line prevents, so the next reader does not remove it"
    );
}
