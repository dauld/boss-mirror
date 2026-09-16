//! The Cloudflare tunnel revoke re-fires on its own (5e8efcf5).
//!
//! `broker-revokes-the-cloudflare-tunnel-daily` is a clock rule that
//! runs the rotation handler's revoke phase — and only that — over
//! every open rotation packet parked at `revoke`. Two things about it
//! are data the handler cannot check for itself and a test must:
//!
//! 1. THE SHAPE. It is scheduled (no `on_event`), its one do-step is the
//!    tunnel rotation handler, and it says `phase = "revoke"` — the
//!    handler's contract that a day has nothing to mint for. A clock
//!    firing without that arg is refused by the handler; a rule that
//!    lost it would refuse every day, quietly, on the dispatcher's
//!    dead-letter shelf.
//!
//! 2. THE PIN. Its args are the revoke SUBSET of the rotation rule's
//!    declaration (zone, Secret, prefix, credential) and live in a second
//!    file — a fact that lives twice (CLAUDE.md §9a). This test names
//!    the key when the two drift: a sweep reading a different Secret
//!    would take a different tunnel for "current", and the current
//!    tunnel is the one thing the revoke phase must never touch.

use boss_core::calendar::Cadence;
use boss_dispatcher::rules::registry::{RawRegistry, RawRule, parse_raw_path};
use boss_testing::dispatcher_rules_dir;

const ROTATION: &str = "broker-rotates-the-cloudflare-tunnel";
const REFIRE: &str = "broker-revokes-the-cloudflare-tunnel-daily";
const HANDLER: &str = "credential.rotate.cloudflare-tunnel";

fn rules() -> RawRegistry {
    parse_raw_path(dispatcher_rules_dir()).expect("parse the rule directory")
}

fn rule<'a>(reg: &'a RawRegistry, name: &str) -> &'a RawRule {
    reg.rules
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("rule {name} is in the authored directory"))
}

#[test]
fn the_refire_is_a_daily_clock_rule_running_only_the_revoke_phase() {
    let reg = rules();
    let refire = rule(&reg, REFIRE);
    assert!(
        refire.on_event.is_none(),
        "the re-fire is clock-driven, not event-driven: {:?}",
        refire.on_event
    );
    let schedule = refire
        .schedule
        .as_ref()
        .expect("the re-fire declares a schedule");
    assert_eq!(
        schedule.cadence,
        Cadence::Daily,
        "the dispatcher fires on sim-day boundaries, so daily is the finest cadence it \
         honours; a deferred revoke costs a day, not a hand"
    );
    assert_eq!(refire.do_steps.len(), 1, "one handler, one phase");
    let step = &refire.do_steps[0];
    assert_eq!(step.handler, HANDLER);
    assert_eq!(
        step.args.get("phase").map(String::as_str),
        Some("\"revoke\""),
        "phase = \"revoke\" is the handler's contract that a clock firing mints nothing"
    );
    for absent in ["hostnames", "verify_hostname", "restart_deployment"] {
        assert!(
            !step.args.contains_key(absent),
            "{absent} is the install/verify phases' business, not the revoke sweep's"
        );
    }
}

#[test]
fn the_refire_reads_the_same_zone_secret_prefix_and_credential_as_the_rotation() {
    let reg = rules();
    let rotation = &rule(&reg, ROTATION).do_steps[0];
    let refire = &rule(&reg, REFIRE).do_steps[0];
    assert_eq!(rotation.handler, HANDLER);
    for key in [
        "zone",
        "secret_namespace",
        "secret_name",
        "secret_key",
        "tunnel_name_prefix",
        "credential_id",
    ] {
        assert_eq!(
            refire.args.get(key),
            rotation.args.get(key),
            "{REFIRE} and {ROTATION} disagree on `{key}`: the sweep would take a different \
             tunnel for the current one, which is the one tunnel the revoke phase must never \
             delete"
        );
    }
    // Both spellings, or neither: an account_id override on one file
    // and not the other is the same drift by another name.
    assert_eq!(
        refire.args.get("account_id"),
        rotation.args.get("account_id"),
        "account_id override differs between {REFIRE} and {ROTATION}"
    );
}
