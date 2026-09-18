//! Tenant-data seeding — POST the canonical brewery accounts,
//! vendors, employees, messages, bulletins, reservations, finished-
//! goods, raw materials, equipment catalog, and serialized assets to
//! the live API, idempotently. Each successful POST lands as an
//! event in audit_log so the projection rebuilders reconstruct the
//! rows alongside the jobs/commerce/inventory events. Emitting these
//! events is what keeps invoice account_ids from dangling: every
//! account an invoice references exists as a projected row.
//!
//! Generates 50 `acc-bigseed-{0..49}` accounts + 30 prospects + 13
//! vendors matching the subject-ids `seed_brewery_subjects` claims
//! in [`crate`]'s lib. Data is deterministic — same seed ids, same
//! names every run. Idempotent — uses exists/HEAD checks (or POST →
//! 409 swallow) to skip rows that already exist, and deterministic
//! `source_id`s on the opening-balance JEs.
//!
//! [`seed_tenant_data`] is the single entry point: prime the clock,
//! then seed in dependency order. The thin `boss-brewery-data-seed`
//! binary is a CLI shell over it; the converged prepare step calls
//! it with every service routed through the gateway
//! ([`SeedBases::all`]).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use boss_inventory::types::VendorBehavior;
use reqwest::blocking::Client;
use serde_json::json;
use tracing::{info, warn};

/// Per-service base URLs the tenant seed POSTs against. In the
/// converged-prepare path every field is the same gateway URL
/// ([`SeedBases::all`]); the standalone `boss-brewery-data-seed`
/// binary can point each at an individual service port, and a
/// partial stack (one without the solo, prod-only `content` /
/// `calendar` services) is skipped by the reachability probe rather
/// than failed. The Playwright scratch stack that first needed that
/// probe was deleted 2026-09-18 with the live smoke suite (design
/// 0e07ce64); the probe stays because the standalone binary still
/// runs against whatever is up.
pub struct SeedBases {
    pub people: String,
    pub accounts: String,
    pub inventory: String,
    pub messages: String,
    pub content: String,
    pub calendar: String,
    pub products: String,
    pub catalog: String,
    pub assets: String,
    pub ledger: String,
}

impl SeedBases {
    /// Route every service through one base URL — the gateway case
    /// (a deployment whose gateway proxies every `/api/*` prefix).
    pub fn all(base: &str) -> Self {
        Self {
            people: base.to_string(),
            accounts: base.to_string(),
            inventory: base.to_string(),
            messages: base.to_string(),
            content: base.to_string(),
            calendar: base.to_string(),
            products: base.to_string(),
            catalog: base.to_string(),
            assets: base.to_string(),
            ledger: base.to_string(),
        }
    }

    /// Per-service localhost ports from `boss_ports` — the no-gateway
    /// case. reset-to-baseline seeds with the gateway stopped (it
    /// holds a DB pool that would block the dropdb), so the converged
    /// prepare step routes directly to each service by default.
    pub fn from_ports() -> Self {
        Self {
            people: boss_ports::url("people"),
            accounts: boss_ports::url("accounts"),
            inventory: boss_ports::url("inventory"),
            messages: boss_ports::url("messages"),
            content: boss_ports::url("content"),
            calendar: boss_ports::url("calendar"),
            products: boss_ports::url("products"),
            catalog: boss_ports::url("catalog"),
            assets: boss_ports::url("assets"),
            ledger: boss_ports::url("ledger"),
        }
    }
}

const ACCOUNT_COUNT: u32 = 50;
// Vendor count is bounded by what the brewery sim actually transacts
// with over a 12-month run — seed vendors when a Workflow or
// counterparty spec reaches for them, not preemptively, so the SPA's
// vendor list reflects real procurement rather than noise.
//
// 13 because auto-restock resolves a category-appropriate supplier
// via `vendor_for(part_sku)` → `primary_vendor_for_part`, which
// matches the part's `vendor_category` (parts.toml) against
// `vendors.category`. The packaging parts (PKG-*) need a
// `packaging`-category vendor, the first of which sits at
// vendors.toml index 12.
//
// `pub` because `seed_brewery_subjects` mints one vendor Subject per
// vendor and must mint exactly this many — the auto-restock resolver
// can pick any seeded vendor, and one without a Subject makes the
// restock Job's subject fail to resolve. That used to be a second
// hardcoded `13` under a "keep this in sync" comment; one constant
// cannot drift from itself.
pub const VENDOR_COUNT: u32 = 13;

/// Pre-seeded prospect Accounts that the `sale` Workflow targets
/// (the wholesale-account-acquisition funnel: cold outreach →
/// qualification → tasting visit → quote → ops approval → first
/// order). Distinct from `acc-bigseed-*` (existing wholesale
/// customers placing recurring orders) — prospect accounts model
/// the new-account pipeline a real brewery sales team works.
/// 30 prospects keeps the /sales page populated with active Jobs
/// at the steady-state sale rate (~0.4/day) without depleting
/// the pool faster than turnover.
const PROSPECT_COUNT: u32 = 30;

// Account / vendor / message / bulletin fixtures live as data in
// `examples/brewery/seeds/*.toml` so editing the playground roster
// doesn't require a Rust rebuild. Loaded once at startup via
// `OnceLock` + `include_str!`. See the seed-vs-emergent doctrine
// in CLAUDE.md.
mod fixtures {
    use serde::Deserialize;
    use std::sync::OnceLock;

    const ACCOUNTS_TOML: &str = include_str!("../../../../../examples/brewery/seeds/accounts.toml");
    const VENDORS_TOML: &str = include_str!("../../../../../examples/brewery/seeds/vendors.toml");
    const MESSAGES_TOML: &str = include_str!("../../../../../examples/brewery/seeds/messages.toml");
    const BULLETINS_TOML: &str =
        include_str!("../../../../../examples/brewery/seeds/bulletins.toml");

    #[derive(Deserialize)]
    pub struct Accounts {
        pub names: Vec<String>,
        #[serde(rename = "city")]
        pub cities: Vec<City>,
        pub directors: Vec<String>,
    }
    #[derive(Deserialize)]
    pub struct City {
        pub name: String,
        pub state: String,
    }
    #[derive(Deserialize)]
    pub struct Vendors {
        #[serde(rename = "vendor")]
        pub vendors: Vec<Vendor>,
        pub payment_terms: Vec<String>,
    }
    #[derive(Deserialize)]
    pub struct Vendor {
        pub name: String,
        pub category: String,
    }
    #[derive(Deserialize)]
    pub struct Messages {
        #[serde(rename = "thread")]
        pub threads: Vec<Thread>,
    }
    #[derive(Deserialize)]
    pub struct Thread {
        pub from: String,
        pub to: String,
        pub subject: String,
        pub body: String,
    }
    #[derive(Deserialize)]
    pub struct Bulletins {
        #[serde(rename = "bulletin")]
        pub bulletins: Vec<Bulletin>,
    }
    #[derive(Deserialize)]
    pub struct Bulletin {
        pub title: String,
        pub body: String,
    }

    pub fn accounts() -> &'static Accounts {
        static CACHE: OnceLock<Accounts> = OnceLock::new();
        CACHE.get_or_init(|| {
            toml::from_str(ACCOUNTS_TOML).expect("accounts.toml ships with the binary")
        })
    }
    pub fn vendors() -> &'static Vendors {
        static CACHE: OnceLock<Vendors> = OnceLock::new();
        CACHE.get_or_init(|| {
            toml::from_str(VENDORS_TOML).expect("vendors.toml ships with the binary")
        })
    }
    pub fn messages() -> &'static Messages {
        static CACHE: OnceLock<Messages> = OnceLock::new();
        CACHE.get_or_init(|| {
            toml::from_str(MESSAGES_TOML).expect("messages.toml ships with the binary")
        })
    }
    pub fn bulletins() -> &'static Bulletins {
        static CACHE: OnceLock<Bulletins> = OnceLock::new();
        CACHE.get_or_init(|| {
            toml::from_str(BULLETINS_TOML).expect("bulletins.toml ships with the binary")
        })
    }
}

// Small enums stay inline — moving 3-4-entry tables to TOML buys
// nothing and the deserialization overhead is real.
const TIERS: &[&str] = &["platinum", "gold", "silver"];
const ACCOUNT_TYPES: &[&str] = &[
    "wholesale-distributor",
    "bar-restaurant",
    "chain-retail",
    "taproom-direct",
];

// Account-managers seeded in `examples/brewery/seeds/employees.json`
// (emp-aa-302..313 — 12 people with role=account-manager).
// Bigseed account creation rotates `territory_rep_id` through
// this pool so the brewery's 50 wholesale accounts distribute
// across the AM team at ~4 each, rather than every account
// pointing at the CTO (which would read as "the CTO owns the
// entire book of business").
const ACCOUNT_MANAGER_POOL: &[&str] = &[
    "emp-aa-302",
    "emp-aa-303",
    "emp-aa-304",
    "emp-aa-305",
    "emp-aa-306",
    "emp-aa-307",
    "emp-aa-308",
    "emp-aa-309",
    "emp-aa-310",
    "emp-aa-311",
    "emp-aa-312",
    "emp-aa-313",
];

// Vendor names, categories, and payment terms live in
// `examples/brewery/seeds/vendors.toml` (loaded via
// `fixtures::vendors()`), one row per supplier across the
// brewery's procurement categories — hops, grain/malt, yeast,
// packaging, sanitation, logistics, equipment, utilities,
// professional services. `VENDOR_COUNT` bounds how many of those
// rows get seeded.

const OPERATORS: &[&str] = &["emp-cto", "emp-coo", "emp-ceo"];

/// Seed the entire brewery tenant model through the public API.
///
/// Primes the clock to the sim epoch (so every seed-side write is
/// stamped at the epoch, not wallclock), then seeds in dependency
/// order: operator hires + employees first (territory_rep_id FKs to
/// employees), then accounts/prospects/vendors, then messages,
/// bulletins, reservations, finished-goods, raw materials, equipment
/// catalog, and serialized assets — each gated on a reachability
/// probe so a partial stack (the standalone seed binary pointed at
/// fewer services than prod runs) skips the services it doesn't run.
///
/// `x_boss_user` overrides the default `automation:brewery-seed`
/// platform-admin header when `Some`. Idempotent throughout.
pub fn seed_tenant_data(
    bases: &SeedBases,
    seeds_dir: &Path,
    x_boss_user: Option<&str>,
) -> Result<()> {
    // Tenant seed loaders run AS the public API like every other
    // actor, but the actor identity is a dedicated seed-loader
    // identity — NOT emp-bootstrap-admin.
    //
    // emp-bootstrap-admin is a platform identity; the brewery CTO
    // / COO / CEO / owner are tenant employees with their own
    // provenance. Marking them as "created by bootstrap-admin"
    // would imply a platform→tenant relationship that doesn't
    // exist. The seed-loader id is the right provenance: "the
    // brewery seed bundle landed these rows," distinct from any
    // human relationship in either layer.
    let user_header = x_boss_user.map(|s| s.to_string()).unwrap_or_else(|| {
        json!({
            "id": "automation:brewery-seed",
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "platform",
        })
        .to_string()
    });

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-boss-user",
        reqwest::header::HeaderValue::from_str(&user_header)
            .with_context(|| "x-boss-user header value")?,
    );

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    // Rebase the formula clock to the brewery epoch so opening-balance
    // JEs + account/vendor creates stamp at 2025-04-01 (or
    // BOSS_EPOCH_START) rather than wallclock today.
    configure_clock_to_epoch(&client)?;

    info!(
        accounts = ACCOUNT_COUNT,
        vendors = VENDOR_COUNT,
        message_threads = fixtures::messages().threads.len(),
        bulletins = fixtures::bulletins().bulletins.len(),
        "seeding brewery data"
    );

    // Employees first — accounts.create mirrors the
    // territory_rep_id into account_team_members, which FKs to
    // employees(id) and emits accounts.account.team-assigned
    // inside the create_account transaction, so the FK is enforced
    // immediately. Every territory rep's employee row must exist
    // before the account that references them lands.
    //
    // Brewery tenant operator hires (CTO/COO/CEO/owner) — loaded
    // FIRST so the messages step's OPERATORS constant references
    // exist; loaded before seed_employees (the 405-row brewery
    // roster) because the brewery exec team is referenced by
    // hardcoded id throughout the seed data. This is the tenant
    // equivalent of the platform operator-baseline file, kept in
    // the tenant's seed bundle instead.
    seed_brewery_operator_hires(&client, &bases.people, &headers, seeds_dir)?;

    seed_employees(&client, &bases.people, &headers)?;

    for i in 0..ACCOUNT_COUNT {
        let id = format!("acc-bigseed-{i:04}");
        ensure_account_with_id(&client, &bases.accounts, &headers, &id, i)?;
    }
    // Catch-all account for the brewery `/shop` direct-to-consumer
    // checkout flow. The direct-shop-order Workflow sets subject =
    // {kind=account, id=acc-direct-shop} regardless of which guest
    // checked out — per-customer Account creation is deferred to
    // the email-OTP follow-up. Seed once on every regen so the
    // FK from jobs.subject resolves.
    ensure_direct_shop_account(&client, &bases.accounts, &headers)?;

    // Prospect accounts for the `sale` Workflow (wholesale-account-
    // acquisition funnel). Seeded with account_type=
    // 'wholesale-prospect' so the Sales pipeline page can filter
    // them apart from `acc-bigseed-*` (existing wholesale
    // customers). Tier rotates so a few prospects are flagged
    // gold-tier (high-value targets).
    for i in 0..PROSPECT_COUNT {
        let id = format!("acc-prospect-{i:03}");
        ensure_prospect_account(&client, &bases.accounts, &headers, &id, i)?;
    }

    // Per-category vendor behavior templates (hand-set defaults the
    // simulator stamps onto each vendor and then drives its supply chain
    // from). Sourced from the brewery class seed (`classes.json`) — the same
    // file seed_classes posts to /api/classes.
    let vendor_templates = load_vendor_behavior_templates(seeds_dir);
    for i in 0..VENDOR_COUNT {
        ensure_vendor(&client, &bases.inventory, &headers, i, &vendor_templates)?;
    }
    ensure_messages(&client, &bases.messages, &headers)?;

    // Bulletins and reservations live on solo (prod-only)
    // services. When the standalone seed binary is pointed at a
    // partial stack, those base URLs will be unreachable. Skip with
    // a warn instead of failing the whole seed run — a partial stack
    // does not need bulletins / reservations seeded, and pointing
    // them at prod from another context would pollute the live DB.
    // (The Playwright scratch stack this was written for went with
    // the live smoke suite, 2026-09-18, design 0e07ce64.) Reachability
    // probe is a 2-second timeout against the service health
    // endpoint; on success the seed step runs as before.
    if base_reachable(&client, &bases.content, "/api/content/health") {
        ensure_bulletins(&client, &bases.content, &headers)?;
    } else {
        info!(base = %bases.content, "content base unreachable — skipping bulletin seed");
    }
    if base_reachable(&client, &bases.calendar, "/api/calendar/health") {
        ensure_reservations(&client, &bases.calendar, &headers)?;
    } else {
        info!(base = %bases.calendar, "calendar base unreachable — skipping reservation seed");
    }

    if base_reachable(&client, &bases.products, "/api/products/health") {
        ensure_finished_product_inventory(&client, &bases.products, &headers)?;
    } else {
        info!(base = %bases.products, "products base unreachable — skipping finished-product inventory seed");
    }

    // Raw-material opening balances. Symmetric to the FG block
    // above: the ledger /api/ledger/inventory-transferred endpoint
    // writes financial_facts directly without an audit_log event,
    // so the bundle round-trip (which projects financial_facts
    // from audit_log) doesn't preserve these JEs. Both flows
    // therefore re-emit at every reset. Without this the first
    // consume of opening raw stock credits 1300 with no matching
    // prior debit and account 1300 walks net-negative.
    // brewery-engine emits the same JEs at regen time so the live
    // audit_log shows the source — both paths land on the same
    // source_id (`opening-raw-{sku}`), colliding cleanly on the
    // (kind, source_table, source_id) unique index.
    if base_reachable(&client, &bases.inventory, "/api/inventory/health")
        && base_reachable(&client, &bases.ledger, "/api/ledger/health")
    {
        ensure_raw_inventory_opening_balances(&client, &bases.inventory, &headers, seeds_dir)?;
        // Cash opening balance — the brewery doesn't open its
        // doors with $0 in the bank. Without an opening JE,
        // every payroll run + tax payment + vendor settlement
        // walks 1000 Cash net-negative before any AR conversion
        // lands. Same shape as the raw + FG opening JEs: DR 1000
        // / CR 3000 Retained Earnings at the brewery's seed
        // working capital. Idempotent source_id so both this path
        // and any external emitter collide cleanly.
        ensure_cash_opening_balance(&client, &bases.ledger, &headers)?;
    } else {
        info!(base = %bases.ledger, "ledger base unreachable — skipping raw-material opening JEs");
    }

    // Graduated excise rate schedules (brewery-fidelity Q4): the
    // US-FEDERAL TTB curve lands as registry rows so accruals run the
    // real graduated math instead of the dispatcher rule's flat
    // fallback. Ledger-only — independent of the inventory-gated
    // opening-JE block above.
    if base_reachable(&client, &bases.ledger, "/api/ledger/health") {
        ensure_excise_rate_schedules(&client, &bases.ledger, &headers, seeds_dir)?;
    } else {
        info!(base = %bases.ledger, "ledger base unreachable — skipping excise rate schedules");
    }

    if base_reachable(&client, &bases.catalog, "/api/catalog/health") {
        ensure_equipment_catalog(&client, &bases.catalog, &headers, seeds_dir)?;
        ensure_marketing_assets(&client, &bases.catalog, &headers, seeds_dir)?;
    } else {
        info!(base = %bases.catalog, "catalog base unreachable — skipping equipment catalog + marketing assets seed");
    }

    if base_reachable(&client, &bases.assets, "/api/assets/health") {
        ensure_brewhouse_assets(&client, &bases.assets, &headers, seeds_dir)?;
        // Pair the assets seed with its PP&E opening JE — same
        // shape as raw + FG opening balances. Without this the
        // 67 seeded assets contribute zero to 1500 Fixed Assets
        // and the balance sheet under-reports total assets.
        ensure_asset_opening_balances(&client, &bases.ledger, &headers, seeds_dir)?;
        // Quarterly depreciation against the just-posted PP&E.
        // Posts 4 dated JEs spanning the 12-month sim window.
        ensure_asset_depreciation_schedule(&client, &bases.ledger, &headers, seeds_dir)?;
    } else {
        info!(base = %bases.assets, "assets base unreachable — skipping brewhouse assets seed");
    }

    info!("brewery data seed complete");
    Ok(())
}

/// The brewery `data/` bundle (catalog.json, assets.json, marketing-
/// assets.json) sits beside the `seeds/` dir under examples/brewery/.
/// Resolve it from the RUNTIME `seeds_dir`, never the compile-time
/// CARGO_MANIFEST_DIR: in the docker image the build tree (/build) is
/// gone at runtime (/opt/boss), so a path baked in at compile time reads
/// a directory that doesn't exist — prepare then aborts non-zero *after*
/// it has already seeded everything else, and the launcher reports
/// "prepare failed" on a demo that is actually fine but for these rows.
fn brewery_data_dir(seeds_dir: &Path) -> std::path::PathBuf {
    seeds_dir
        .parent()
        .map(|p| p.join("data"))
        .unwrap_or_else(|| seeds_dir.join("data"))
}

/// Rebase the deployed clock-api to the brewery's epoch so every
/// seed-side write (opening-balance JEs, account/vendor creates) is
/// stamped at the sim epoch instead of wallclock today. Reads
/// BOSS_EPOCH_START (default 2025-04-01) + BOSS_CLOCK_URL.
///
/// Best-effort against the clock POST itself: a non-2xx or transport
/// error only warns (opening JEs then land on wallclock if clock-api
/// is in wall mode). Hard-fails only when BOSS_EPOCH_START is set but
/// not parseable as YYYY-MM-DD.
fn configure_clock_to_epoch(client: &Client) -> Result<()> {
    // Pin every seed-side write to the brewery's epoch start so
    // opening-balance JEs (raw 1300, cash 1000, FG 1320 via
    // products PUT) get stamped at 2025-04-01 rather than wall-
    // clock today. POSTs `/api/clock/configure` to the deployed
    // clock-api so the receiving services see the epoch instant
    // via their ClockClient. Tenants with a different epoch
    // override via BOSS_EPOCH_START.
    let epoch_start =
        std::env::var("BOSS_EPOCH_START").unwrap_or_else(|_| "2025-04-01".to_string());

    let clock_api_url =
        std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    // Formula clock: rebase epoch_start via /configure so sim-time
    // = epoch_start at the moment of the call.
    let advance_url = format!(
        "{}/api/clock/configure",
        clock_api_url.trim_end_matches('/')
    );
    // Anchor every seed-phase emit at LA 06:00 of the day BEFORE
    // sim_start (= UTC 13:00 PDT of sim_start − 1). Anchoring at
    // LA 06:00 of the prior day reads as "operator provisioned the
    // system before operations began" — chronologically before
    // sim's first day, so audit_log ordering matches what an
    // executor would expect and the seed events don't cluster at a
    // single instant.
    //
    // Hardcoded +7h offset assumes PDT throughout the regen
    // window (sim epoch 2025-04-01 is in PDT). Sims that cross
    // into PST or non-LA tenants need an explicit tz hookup.
    let seed_anchor = {
        let day = chrono::NaiveDate::parse_from_str(&epoch_start, "%Y-%m-%d")
            .with_context(|| format!("BOSS_EPOCH_START={epoch_start} is not YYYY-MM-DD"))?;
        let day_before = day - chrono::Duration::days(1);
        format!("{day_before}T13:00:00Z")
    };
    // /configure with epoch_start rebases the formula's wall_anchor
    // to wall-now, so sim-time = epoch_start at the moment of the
    // call. set_to is the seed-anchor instant (LA 06:00 day-prior);
    // we extract the date.
    let epoch_start_date = chrono::DateTime::parse_from_rfc3339(&seed_anchor)
        .map(|d| d.date_naive().to_string())
        .unwrap_or_else(|_| epoch_start.clone());
    let advance_body = serde_json::json!({
        "epoch_start": epoch_start_date,
    });
    match client.post(&advance_url).json(&advance_body).send() {
        Ok(r) if r.status().is_success() => {
            info!(seed_anchor = %seed_anchor, %clock_api_url, "advanced clock-api to LA 06:00 of (sim_start − 1) — pre-sim provisioning window");
        }
        Ok(r) => {
            warn!(
                status = %r.status(),
                %advance_url,
                "clock-api /advance returned non-2xx; opening JEs will land on wallclock if clock-api is in wall mode"
            );
        }
        Err(e) => {
            warn!(
                error = %e,
                %advance_url,
                "clock-api /advance failed; opening JEs will land on wallclock if clock-api is in wall mode"
            );
        }
    }
    Ok(())
}

/// Catch-all `acc-direct-shop` Account for the /shop direct-to-
/// consumer flow. Distinct from `ensure_account_with_id` because
/// (a) the id + name + meta are fixed, not picked from the
/// rotating arrays, and (b) account_type='direct-consumer'
/// signals the operating-model semantics — this isn't a real
/// customer cluster, it's the placeholder Subject that pre-
/// email-OTP guest orders open against.
fn ensure_direct_shop_account(
    client: &Client,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
) -> Result<()> {
    let id = "acc-direct-shop";
    let exists_url = format!("{api_base}/api/people/accounts/{id}/exists");
    let resp = client.get(&exists_url).headers(headers.clone()).send();
    if let Ok(r) = resp
        && r.status().is_success()
    {
        let body: serde_json::Value = r.json().unwrap_or_default();
        if body.get("exists").and_then(|v| v.as_bool()) == Some(true) {
            return Ok(());
        }
    }

    let payload = json!({
        "id": id,
        "name": "Direct Shop — guest customers",
        "director": "(unassigned)",
        "city": "Brewhouse",
        "state": "OR",
        "tier": "silver",
        "customer_since": "2025-04-01",
        // Direct shop is a single catch-all account; assign the
        // first account-manager in the pool. Same rationale as
        // ensure_account_with_id — emp-cto isn't a realistic
        // owner of the brewery's online checkout flow.
        "territory_rep_id": ACCOUNT_MANAGER_POOL[0],
        "account_type": "direct-consumer",
        "contacts": [],
    });

    let url = format!("{api_base}/api/people/accounts");
    let resp = client
        .post(&url)
        .headers(headers.clone())
        .json(&payload)
        .send()
        .with_context(|| format!("POST {url} for {id}"))?;
    let status = resp.status();
    if status.is_success() || status.as_u16() == 409 {
        info!(account_id = %id, "direct-shop catch-all account ensured");
        Ok(())
    } else {
        let body = resp.text().unwrap_or_default();
        warn!(account_id = %id, status = %status, body = %body, "direct-shop account create failed");
        Ok(())
    }
}

/// Pre-seed a prospect Account for the `sale` Workflow. Distinct
/// from `ensure_account_with_id` because the payload is fixed
/// (account_type='wholesale-prospect', name carries the "Prospect"
/// label so it's visually distinguishable on AccountsList until
/// the deeper account-stage UI lands).
fn ensure_prospect_account(
    client: &Client,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    id: &str,
    i: u32,
) -> Result<()> {
    let exists_url = format!("{api_base}/api/people/accounts/{id}/exists");
    let resp = client.get(&exists_url).headers(headers.clone()).send();
    if let Ok(r) = resp
        && r.status().is_success()
    {
        let body: serde_json::Value = r.json().unwrap_or_default();
        if body.get("exists").and_then(|v| v.as_bool()) == Some(true) {
            return Ok(());
        }
    }

    // Reuse the rotating name + city + director arrays so each
    // prospect reads as a plausible bar / restaurant / distributor.
    let accounts = fixtures::accounts();
    let base_name = &accounts.names[(i as usize + 17) % accounts.names.len()];
    let name = format!("{base_name} (Prospect)");
    let city_row = &accounts.cities[(i as usize + 3) % accounts.cities.len()];
    let (city, state) = (&city_row.name, &city_row.state);
    // Prospect tier rotates gold/silver — gold flags high-value
    // targets the sales team prioritises. No platinum on prospects
    // (platinum is a customer-tier achievement, not a prospect one).
    let tier = if i.is_multiple_of(3) {
        "gold"
    } else {
        "silver"
    };
    let director = &accounts.directors[(i as usize + 11) % accounts.directors.len()];

    let payload = json!({
        "id": id,
        "name": name,
        "director": director,
        "city": city,
        "state": state,
        "tier": tier,
        // Customer_since left null-ish (placeholder past date) since
        // prospects haven't bought yet. The `sale` Workflow's tier-6
        // milestone marks the actual onboarding moment.
        "customer_since": "2026-01-01",
        "territory_rep_id": "emp-aa-005",
        "account_type": "wholesale-prospect",
        "contacts": [],
    });

    let url = format!("{api_base}/api/people/accounts");
    let resp = client
        .post(&url)
        .headers(headers.clone())
        .json(&payload)
        .send()
        .with_context(|| format!("POST {url} for {id}"))?;
    let status = resp.status();
    if status.is_success() || status.as_u16() == 409 {
        info!(prospect_id = %id, name = %name, "prospect account ensured");
        Ok(())
    } else {
        let body = resp.text().unwrap_or_default();
        warn!(prospect_id = %id, status = %status, body = %body, "prospect account create failed");
        Ok(())
    }
}

fn ensure_account_with_id(
    client: &Client,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    id: &str,
    i: u32,
) -> Result<()> {
    let exists_url = format!("{api_base}/api/people/accounts/{id}/exists", id = id);
    let resp = client.get(&exists_url).headers(headers.clone()).send();
    if let Ok(r) = resp
        && r.status().is_success()
    {
        let body: serde_json::Value = r.json().unwrap_or_default();
        if body.get("exists").and_then(|v| v.as_bool()) == Some(true) {
            return Ok(());
        }
    }

    let accounts = fixtures::accounts();
    let name = &accounts.names[i as usize % accounts.names.len()];
    let city_row = &accounts.cities[i as usize % accounts.cities.len()];
    let (city, state) = (&city_row.name, &city_row.state);
    let tier = TIERS[i as usize % TIERS.len()];
    let account_type = ACCOUNT_TYPES[i as usize % ACCOUNT_TYPES.len()];
    let director = &accounts.directors[i as usize % accounts.directors.len()];
    // Customer-since dates spread across the past 5 years for
    // realistic aging on the AccountsList.
    let since = chrono::NaiveDate::from_ymd_opt(2021, 1, 1).unwrap()
        + chrono::Duration::days(((i as i64) * 27) % (365 * 5));

    let payload = json!({
        "id": id,
        "name": name,
        "director": director,
        "city": city,
        "state": state,
        "tier": tier,
        "customer_since": since,
        // Rotate territory rep through the brewery's account-
        // manager pool so the 50 bigseed accounts spread across
        // the AM team instead of all pointing at one operator —
        // a plausible book of business + the input role-aware
        // step assignment needs.
        "territory_rep_id": ACCOUNT_MANAGER_POOL[i as usize % ACCOUNT_MANAGER_POOL.len()],
        "account_type": account_type,
        "contacts": [],
    });

    let url = format!("{api_base}/api/people/accounts");
    let resp = client
        .post(&url)
        .headers(headers.clone())
        .json(&payload)
        .send()
        .with_context(|| format!("POST {url} for {id}"))?;
    let status = resp.status();
    if status.is_success() {
        info!(account_id = %id, name = %name, "account created");
        Ok(())
    } else if status.as_u16() == 409 {
        Ok(())
    } else {
        let body = resp.text().unwrap_or_default();
        warn!(account_id = %id, status = %status, body = %body, "account create failed");
        Ok(())
    }
}

fn ensure_vendor(
    client: &Client,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    i: u32,
    templates: &HashMap<String, serde_json::Value>,
) -> Result<()> {
    let id = format!("vnd-bigseed-{i:03}");
    let url_one = format!("{api_base}/api/inventory/vendors/{id}");
    let resp = client.get(&url_one).headers(headers.clone()).send();
    if let Ok(r) = resp
        && r.status().is_success()
    {
        return Ok(());
    }

    let vendors = fixtures::vendors();
    let accounts = fixtures::accounts();
    let vendor_row = &vendors.vendors[i as usize % vendors.vendors.len()];
    let name = &vendor_row.name;
    let category = &vendor_row.category;
    let city_row = &accounts.cities[i as usize % accounts.cities.len()];
    let (city, state) = (&city_row.name, &city_row.state);
    let terms = &vendors.payment_terms[i as usize % vendors.payment_terms.len()];
    let contact = &accounts.directors[(i as usize + 7) % accounts.directors.len()];

    // Bootstrap this vendor's hand-set behavior from its category template
    // (a supplier category). When present, the supply lead time also drives
    // the canonical `lead_time_days` field so reorder timing and the
    // behavior profile agree; non-supplier vendors keep the spread default.
    let behavior = templates
        .get(category.as_str())
        .and_then(|t| VendorBehavior::from_template(t, category));
    let lead_time_days = match &behavior {
        Some(b) => b.lead_time_days.round().max(1.0) as u32,
        None => 7 + (i % 14),
    };

    let mut payload = json!({
        "id": id,
        "name": name,
        "contact_name": contact,
        "contact_email": format!("contact@{}.example", id),
        "city": city,
        "state": state,
        "lead_time_days": lead_time_days,
        "payment_terms": terms,
        "category": category,
    });
    if let Some(b) = &behavior {
        payload["behavior"] = serde_json::to_value(b).unwrap_or_default();
    }

    let url = format!("{api_base}/api/inventory/vendors");
    let resp = client
        .post(&url)
        .headers(headers.clone())
        .json(&payload)
        .send()
        .with_context(|| format!("POST {url} for {id}"))?;
    let status = resp.status();
    if status.is_success() {
        info!(vendor_id = %id, name = %name, "vendor created");
        Ok(())
    } else if status.as_u16() == 409 {
        Ok(())
    } else {
        let body = resp.text().unwrap_or_default();
        warn!(vendor_id = %id, status = %status, body = %body, "vendor create failed");
        Ok(())
    }
}

/// Read the per-category vendor behavior templates from the brewery class
/// seed (`classes.json`) — `category → behavior_template` (the raw JSON
/// object). Only supplier categories carry one. Empty on a missing or
/// unparseable seed (vendors then seed without a behavior profile, the same
/// as before this feature).
fn load_vendor_behavior_templates(seeds_dir: &Path) -> HashMap<String, serde_json::Value> {
    let mut out = HashMap::new();
    let path = seeds_dir.join("classes.json");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return out;
    };
    let Ok(rows) = serde_json::from_str::<Vec<serde_json::Value>>(&body) else {
        warn!(path = %path.display(), "classes.json unparseable; vendors seed without behavior");
        return out;
    };
    for row in rows {
        let is_vendor_category = row.get("subject_kind").and_then(|v| v.as_str()) == Some("vendor")
            && row.get("member_attribute").and_then(|v| v.as_str()) == Some("category");
        if !is_vendor_category {
            continue;
        }
        if let Some(code) = row.get("code").and_then(|v| v.as_str())
            && let Some(tmpl) = row.get("metadata").and_then(|m| m.get("behavior_template"))
        {
            out.insert(code.to_string(), tmpl.clone());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Messages — operator-baseline conversations.
// ---------------------------------------------------------------------------

// Message thread fixtures live in
// `examples/brewery/seeds/messages.toml` — see `fixtures::messages()`.

/// Load the brewery tenant's operator hires
/// (examples/brewery/seeds/operator_hires.toml) and POST each
/// to /api/people — the CTO/COO/CEO/owner identities the brewery
/// references throughout its seed data (account managers,
/// operator-baseline conversations, exec sign-off rows).
///
/// Tenant org-chart identities live in the tenant's seed bundle
/// and are loaded via the public API like every other external
/// caller. Idempotent via 409-tolerant POSTs.
fn seed_brewery_operator_hires(
    client: &Client,
    people_base: &str,
    headers: &reqwest::header::HeaderMap,
    seeds_dir: &Path,
) -> Result<()> {
    let path = seeds_dir.join("operator_hires.toml");
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            warn!(path = %path.display(), error = %e, "brewery operator_hires.toml not found; skipping tenant operator seed");
            return Ok(());
        }
    };
    #[derive(serde::Deserialize)]
    struct Bundle {
        hire: Vec<serde_json::Value>,
    }
    let bundle: Bundle =
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;

    let url = format!("{}/api/people", people_base.trim_end_matches('/'));
    let mut posted = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    for emp in &bundle.hire {
        let resp = match client.post(&url).headers(headers.clone()).json(emp).send() {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "POST tenant operator transport error");
                failed += 1;
                continue;
            }
        };
        let status = resp.status();
        if status.is_success() {
            posted += 1;
        } else if status.as_u16() == 409 {
            skipped += 1;
        } else {
            let id = emp.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let body = resp.text().unwrap_or_default();
            warn!(%id, %status, body = %body, "POST tenant operator failed");
            failed += 1;
        }
    }
    if failed > 0 {
        anyhow::bail!(
            "{failed} tenant operator POSTs failed (posted={posted}, skipped={skipped}). \
             The brewery seed depends on these org-chart rows landing before downstream \
             references."
        );
    }
    info!(
        path = %path.display(),
        posted,
        skipped,
        "brewery tenant operator hires seeded via /api/people"
    );
    Ok(())
}

/// POST every row in `examples/brewery/seeds/employees.json` to
/// /api/people on the people-api. The roster is generated by
/// `examples/brewery/scripts/gen_employees.py` (deterministic
/// seed=42) and ships 405 rows at the script's current SCALE=3 —
/// cut from SCALE=6 in #96, where 789 employees put annual payroll
/// ($48M) above gross margin ($29M) and drove the sim cash-negative.
///
/// 405 is this file's roster only. A prepared brewery holds 411
/// employees: these 405, plus the four tenant execs from
/// `examples/brewery/seeds/operator_hires.toml`
/// (`seed_brewery_operator_hires` above), plus `emp-audit` and the
/// injected `emp-bootstrap-admin` from the platform baseline
/// (`infra/operator-baseline/operator_hires.toml`). Any count taken
/// against `/api/people` is the 411.
///
/// Idempotent: a 409 Conflict on duplicate id is treated as
/// success, so re-running the seeder is a no-op.
fn seed_employees(
    client: &Client,
    people_base: &str,
    headers: &reqwest::header::HeaderMap,
) -> Result<()> {
    let path = std::path::Path::new("/opt/boss/examples/brewery/seeds/employees.json");
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            warn!(path = %path.display(), error = %e, "employees.json not found; skipping employee seed");
            return Ok(());
        }
    };
    let mut roster: Vec<serde_json::Value> =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;

    // Two-pass to satisfy the manager_id self-FK: first POST
    // every employee with manager_id stripped, then PATCH each
    // row that has a manager so the reporting graph lands.
    // Avoids requiring a topological sort of the roster on the
    // client side.
    let mut manager_assignments: Vec<(String, String)> = Vec::new();
    for emp in &mut roster {
        if let Some(obj) = emp.as_object_mut() {
            let id = obj
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(mgr_val) = obj.remove("manager_id")
                && let Some(mgr) = mgr_val.as_str()
                && !id.is_empty()
                && !mgr.is_empty()
            {
                manager_assignments.push((id, mgr.to_string()));
            }
        }
    }

    let url = format!("{}/api/people", people_base.trim_end_matches('/'));
    let mut posted = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    for emp in &roster {
        let resp = match client.post(&url).headers(headers.clone()).json(emp).send() {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "POST employee transport error");
                failed += 1;
                continue;
            }
        };
        let status = resp.status();
        if status.is_success() {
            posted += 1;
        } else if status.as_u16() == 409 {
            skipped += 1;
        } else {
            let id = emp.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let body = resp.text().unwrap_or_default();
            warn!(%id, %status, body = %body, "POST employee failed");
            failed += 1;
        }
    }
    if failed > 0 {
        anyhow::bail!(
            "{failed} employee POSTs failed (posted={posted}, skipped={skipped}). \
             Refusing to continue — the seed bundle requires every employee to land."
        );
    }

    // Pass 2: PUT each employee back with manager_id populated.
    // Uses /api/people/{id} PUT which the people-api handler
    // accepts as a full-row update.
    let mut linked = 0usize;
    let mut link_failed = 0usize;
    for (emp_id, mgr_id) in &manager_assignments {
        let get_url = format!(
            "{}/api/people/{}",
            people_base.trim_end_matches('/'),
            emp_id
        );
        let current: serde_json::Value = match client.get(&get_url).headers(headers.clone()).send()
        {
            Ok(r) if r.status().is_success() => match r.json() {
                Ok(v) => v,
                Err(e) => {
                    warn!(%emp_id, error = %e, "GET employee body decode failed");
                    link_failed += 1;
                    continue;
                }
            },
            Ok(r) => {
                warn!(%emp_id, status = %r.status(), "GET employee for manager link failed");
                link_failed += 1;
                continue;
            }
            Err(e) => {
                warn!(%emp_id, error = %e, "GET employee transport error");
                link_failed += 1;
                continue;
            }
        };
        // Carry forward the current row, just setting manager_id.
        let mut body = current;
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "manager_id".into(),
                serde_json::Value::String(mgr_id.clone()),
            );
        }
        let put_url = format!(
            "{}/api/people/{}",
            people_base.trim_end_matches('/'),
            emp_id
        );
        let resp = match client
            .put(&put_url)
            .headers(headers.clone())
            .json(&body)
            .send()
        {
            Ok(r) => r,
            Err(e) => {
                warn!(%emp_id, error = %e, "PUT manager link transport error");
                link_failed += 1;
                continue;
            }
        };
        if resp.status().is_success() {
            linked += 1;
        } else {
            let resp_body = resp.text().unwrap_or_default();
            warn!(%emp_id, %mgr_id, body = %resp_body, "PUT manager link failed");
            link_failed += 1;
        }
    }
    if link_failed > 0 {
        anyhow::bail!(
            "{link_failed} manager-link PUTs failed (linked={linked} of {} pairs). \
             Refusing to continue — the org chart must be complete.",
            manager_assignments.len()
        );
    }

    info!(
        total = roster.len(),
        posted,
        skipped,
        manager_links = linked,
        "seeded brewery employees"
    );
    Ok(())
}

fn ensure_messages(
    client: &Client,
    base: &str,
    headers: &reqwest::header::HeaderMap,
) -> Result<()> {
    // Idempotence by sender+subject — refuse to double-post the
    // same scripted thread on reruns.
    let inbox_url = format!("{base}/api/messages/inbox/{}", OPERATORS[0]);
    let existing_subjects: std::collections::HashSet<String> =
        match client.get(&inbox_url).headers(headers.clone()).send() {
            Ok(r) if r.status().is_success() => {
                let body: Vec<serde_json::Value> = r.json().unwrap_or_default();
                body.iter()
                    .filter_map(|m| m.get("subject").and_then(|v| v.as_str()).map(String::from))
                    .collect()
            }
            _ => Default::default(),
        };

    let url = format!("{base}/api/messages/send");
    for thread in &fixtures::messages().threads {
        let sender = thread.from.as_str();
        let recipient = thread.to.as_str();
        let subject = thread.subject.as_str();
        let body = thread.body.as_str();
        if existing_subjects.contains(subject) {
            continue;
        }
        // Per-message x-boss-user must match sender_id (the
        // /send endpoint enforces this for non-trusted callers).
        let mut h = headers.clone();
        let user_json = json!({
            "id": sender,
            "role": "ceo",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "executive",
        });
        h.insert(
            "x-boss-user",
            reqwest::header::HeaderValue::from_str(&user_json.to_string())?,
        );

        let payload = json!({
            "sender_id": sender,
            "recipient_id": recipient,
            "subject": subject,
            "body": body,
            "kind": "direct",
        });
        let resp = client
            .post(&url)
            .headers(h)
            .json(&payload)
            .send()
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        if status.is_success() {
            info!(subject = %subject, "message sent");
        } else {
            let txt = resp.text().unwrap_or_default();
            warn!(subject = %subject, status = %status, body = %txt, "message send failed");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bulletins — three brewery-themed company posts on the manual / inbox.
// ---------------------------------------------------------------------------

// Bulletin fixtures live in examples/brewery/seeds/bulletins.toml
// — see `fixtures::bulletins()`.

fn ensure_bulletins(
    client: &Client,
    base: &str,
    headers: &reqwest::header::HeaderMap,
) -> Result<()> {
    let list_url = format!("{base}/api/content/bulletins");
    let existing_titles: std::collections::HashSet<String> =
        match client.get(&list_url).headers(headers.clone()).send() {
            Ok(r) if r.status().is_success() => {
                let body: serde_json::Value = r.json().unwrap_or_default();
                body.as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|m| {
                                m.get("title").and_then(|v| v.as_str()).map(String::from)
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
            _ => Default::default(),
        };

    for bulletin in &fixtures::bulletins().bulletins {
        let title = bulletin.title.as_str();
        let body = bulletin.body.as_str();
        if existing_titles.contains(title) {
            continue;
        }
        let payload = json!({
            "title": title,
            "body": body,
            "priority": "normal",
            "audience": { "all": true },
        });
        let resp = client
            .post(&list_url)
            .headers(headers.clone())
            .json(&payload)
            .send()
            .with_context(|| format!("POST {list_url}"))?;
        let status = resp.status();
        if status.is_success() {
            info!(title = %title, "bulletin created");
        } else {
            let txt = resp.text().unwrap_or_default();
            warn!(title = %title, status = %status, body = %txt, "bulletin create failed");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Calendar reservations — a scripted week of operator commitments.
// ---------------------------------------------------------------------------

fn ensure_reservations(
    client: &Client,
    base: &str,
    headers: &reqwest::header::HeaderMap,
) -> Result<()> {
    use chrono::{Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};

    // Anchor on the Monday-after-sim-start so the seeded
    // reservations always sit in the visible /calendar/me window
    // when the playground opens the bundle. Sources from
    // BOSS_EPOCH_START to stay sim-aligned — anchoring on wallclock
    // would shift the seeded reservations on every re-export and
    // break the bundle's stability.
    let epoch_start = std::env::var("BOSS_EPOCH_START")
        .ok()
        .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(2025, 4, 1).unwrap());
    let today = epoch_start;
    let days_to_monday = (7 - today.weekday().num_days_from_monday() as i64) % 7;
    let monday = today
        + Duration::days(if days_to_monday == 0 {
            7
        } else {
            days_to_monday
        });

    let utc_at = |day: NaiveDate, hour: u32, min: u32| {
        let t = NaiveTime::from_hms_opt(hour, min, 0).unwrap();
        Utc.from_utc_datetime(&day.and_time(t))
    };

    // (offset_days, hours_start, hours_end, employee, reason_kind, reason_ref, notes)
    let plan: &[(i64, u32, u32, &str, &str, &str, &str)] = &[
        (
            0,
            9,
            10,
            "emp-ceo",
            "meeting",
            "weekly-1on1-cto",
            "CTO weekly 1:1",
        ),
        (
            0,
            14,
            15,
            "emp-ceo",
            "meeting",
            "weekly-1on1-coo",
            "COO weekly 1:1",
        ),
        (
            1,
            13,
            14,
            "emp-cto",
            "meeting",
            "platform-architecture",
            "Platform architecture sync",
        ),
        (
            2,
            9,
            12,
            "emp-coo",
            "travel",
            "wholesale-glacier-visit",
            "Glacier Pass on-site visit",
        ),
        (
            3,
            10,
            11,
            "emp-cto",
            "training",
            "audit-log-walkthrough",
            "Walk auditor through chain checks",
        ),
        (
            4,
            13,
            17,
            "emp-coo",
            "pto",
            "personal-day-fri",
            "Friday PTO",
        ),
    ];

    // Idempotence: the bulk-list endpoint per-resource lets us see
    // existing reservations in the window. If a `reason_ref` already
    // exists we skip — same as the create_account flow.
    let url = format!("{base}/api/calendar/reservations");
    let listing_window_end = monday + Duration::days(7);
    for emp in OPERATORS {
        let listing = format!(
            "{base}/api/calendar/reservations?resource_kind=employee&resource_id={emp}&start={start}&end={end}",
            start = utc_at(monday, 0, 0).to_rfc3339(),
            end = utc_at(listing_window_end, 0, 0).to_rfc3339(),
        );
        let _ = client.get(&listing).headers(headers.clone()).send(); // priming the cache; we just want to noop on success
    }

    for (offset, h_start, h_end, emp, reason_kind, reason_ref, notes) in plan {
        let day = monday + Duration::days(*offset);
        let payload = json!({
            "subject": { "subject_kind": "employee", "id": emp },
            "window": {
                "start": utc_at(day, *h_start, 0),
                "end": utc_at(day, *h_end, 0),
            },
            "reason_kind": reason_kind,
            "reason_ref_id": reason_ref,
            "strength": "soft",
            "notes": notes,
            "created_by": "system-seed",
        });
        let resp = client
            .post(&url)
            .headers(headers.clone())
            .json(&payload)
            .send()
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        if status.is_success() {
            info!(employee = %emp, reason_ref = %reason_ref, "reservation created");
        } else if status.as_u16() == 409 {
            // Already exists — idempotent.
        } else {
            let txt = resp.text().unwrap_or_default();
            warn!(employee = %emp, reason_ref = %reason_ref, status = %status, body = %txt, "reservation create failed");
        }
    }
    Ok(())
}

/// Seed the brewery's equipment catalog from
/// `examples/brewery/data/catalog.json`. Each row becomes a
/// AssetModel — POST /api/catalog/models.
fn ensure_equipment_catalog(
    client: &Client,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    seeds_dir: &Path,
) -> Result<()> {
    let path = brewery_data_dir(seeds_dir).join("catalog.json");
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let models: Vec<serde_json::Value> =
        serde_json::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
    let count = models.len();
    let url = format!("{api_base}/api/catalog/models");
    for model in models {
        let resp = client
            .post(&url)
            .headers(headers.clone())
            .json(&model)
            .send()
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        // 200/201 = created; 409 = already exists from a prior seed run.
        if !status.is_success() && status.as_u16() != 409 {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("POST {url} returned {status}: {body}");
        }
    }
    info!(count, "equipment catalog seeded");
    Ok(())
}

/// Seed the brewery's marketing-asset library from
/// `examples/brewery/data/marketing-assets.json`. Each row becomes
/// a MarketingAsset — POST /api/catalog/marketing-assets is an
/// upsert (ON CONFLICT (id) DO UPDATE), so reruns are idempotent
/// without needing 409-swallow plumbing on the seeder side.
fn ensure_marketing_assets(
    client: &Client,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    seeds_dir: &Path,
) -> Result<()> {
    let path = brewery_data_dir(seeds_dir).join("marketing-assets.json");
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let assets: Vec<serde_json::Value> =
        serde_json::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
    let count = assets.len();
    let url = format!("{api_base}/api/catalog/marketing-assets");
    for asset in assets {
        let resp = client
            .post(&url)
            .headers(headers.clone())
            .json(&asset)
            .send()
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("POST {url} returned {status}: {body}");
        }
    }
    info!(count, "marketing assets seeded");
    Ok(())
}

/// Mirror of the raw + FG opening-balance JE pass for the
/// brewery's serialized assets. The 67 units in `assets.json`
/// were dropped onto the brewhouse with cost basis pulled from
/// the equipment catalog's `commerce.list_price_new_cents`.
/// Without this, the balance sheet hides ~$10M of PP&E — the
/// assets sit in the `assets` table with no GL counterpart.
/// Posts one DR 1500 / CR 3000 JE per system; idempotent via
/// `source_id = opening-asset-{asset_id}`. New-acquisition JEs
/// (DR 1500 / CR 2100) come from the runtime `equipment-purchase`
/// Workflow, not this seed pass.
fn ensure_asset_opening_balances(
    client: &Client,
    ledger_base: &str,
    headers: &reqwest::header::HeaderMap,
    seeds_dir: &Path,
) -> Result<()> {
    let data_dir = brewery_data_dir(seeds_dir);
    let assets_path = data_dir.join("assets.json");
    let catalog_path = data_dir.join("catalog.json");
    let assets_body = std::fs::read_to_string(&assets_path)
        .with_context(|| format!("read {}", assets_path.display()))?;
    let catalog_body = std::fs::read_to_string(&catalog_path)
        .with_context(|| format!("read {}", catalog_path.display()))?;
    let assets: Vec<serde_json::Value> = serde_json::from_str(&assets_body)?;
    let catalog: Vec<serde_json::Value> = serde_json::from_str(&catalog_body)?;

    // Build sku → list_price_new_cents lookup. Catalog has 13
    // models; the brewery's assets draw from all of them.
    let mut cost_by_sku: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for model in &catalog {
        if let (Some(sku), Some(price)) = (
            model.get("sku").and_then(|v| v.as_str()),
            model
                .get("commerce")
                .and_then(|c| c.get("list_price_new_cents"))
                .and_then(|v| v.as_i64()),
        ) {
            cost_by_sku.insert(sku.to_string(), price);
        }
    }

    let je_url = format!("{ledger_base}/api/ledger/inventory-transferred");
    let mut posted = 0u64;
    let mut total_cents: i64 = 0;
    for unit in &assets {
        let asset_id = unit
            .get("asset_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("asset entry missing asset_id"))?;
        let sku = unit
            .get("sku")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("asset entry {asset_id} missing sku"))?;
        let Some(cost) = cost_by_sku.get(sku).copied() else {
            warn!(
                asset_id,
                sku, "no catalog list_price_new_cents — skipping asset opening JE"
            );
            continue;
        };
        if cost <= 0 {
            continue;
        }
        let body = serde_json::json!({
            "total_cost_cents": cost,
            "debit_account": "1500",
            "credit_account": "3000",
            "memo": format!("Opening balance — {} ({}) (PP&E ← retained earnings)", asset_id, sku),
            "source_table": "brewery_seed_opening_balance",
            "source_id": format!("opening-asset-{asset_id}"),
            "created_by": "boss-brewery-data-seed",
        });
        let resp = client
            .post(&je_url)
            .headers(headers.clone())
            .json(&body)
            .send()
            .with_context(|| format!("POST {je_url} (asset {asset_id})"))?;
        let status = resp.status();
        if !status.is_success() {
            let txt = resp.text().unwrap_or_default();
            anyhow::bail!("POST {je_url} (asset {asset_id}) returned {status}: {txt}");
        }
        posted += 1;
        total_cents = total_cents.saturating_add(cost);
    }
    info!(
        asset_opening_jes_posted = posted,
        total_cost_cents = total_cents,
        "asset PP&E opening-balance JEs complete (DR 1500 / CR 3000)"
    );
    Ok(())
}

/// Monthly depreciation pass for the seeded brewery assets.
/// Mirrors the opening-balance pattern: at seed time we post
/// twelve DR 6900 / CR 1510 JEs covering the 12-month sim window,
/// one per month, sized at (total_cost / useful_life_years / 12).
/// Useful life is a flat 10 years for brewery vessels — the
/// catalog carries no per-model depreciation schedule, so the
/// pass applies one rate across every asset. This gives the GL
/// real monthly expense activity in 6900 and a 1510 contra-asset
/// that grows visibly; reseeding each epoch re-posts the year, so
/// depreciation recurs rather than being a one-off.
///
/// Month end-dates: 2025-04-30 … 2026-03-31 (matches the
/// 2025-04-01 → 2026-03-31 sim window).
fn ensure_asset_depreciation_schedule(
    client: &Client,
    ledger_base: &str,
    headers: &reqwest::header::HeaderMap,
    seeds_dir: &Path,
) -> Result<()> {
    let data_dir = brewery_data_dir(seeds_dir);
    let assets_path = data_dir.join("assets.json");
    let catalog_path = data_dir.join("catalog.json");
    let assets: Vec<serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(&assets_path)
            .with_context(|| format!("read {}", assets_path.display()))?,
    )?;
    let catalog: Vec<serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(&catalog_path)
            .with_context(|| format!("read {}", catalog_path.display()))?,
    )?;

    let mut cost_by_sku: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for model in &catalog {
        if let (Some(sku), Some(price)) = (
            model.get("sku").and_then(|v| v.as_str()),
            model
                .get("commerce")
                .and_then(|c| c.get("list_price_new_cents"))
                .and_then(|v| v.as_i64()),
        ) {
            cost_by_sku.insert(sku.to_string(), price);
        }
    }

    let mut total_cost_cents: i64 = 0;
    for unit in &assets {
        if let Some(sku) = unit.get("sku").and_then(|v| v.as_str())
            && let Some(cost) = cost_by_sku.get(sku).copied()
        {
            total_cost_cents = total_cost_cents.saturating_add(cost);
        }
    }
    if total_cost_cents <= 0 {
        info!("zero asset cost — skipping depreciation schedule");
        return Ok(());
    }

    const USEFUL_LIFE_YEARS: i64 = 10;
    let annual_cents = total_cost_cents / USEFUL_LIFE_YEARS;
    let monthly_cents = annual_cents / 12;
    if monthly_cents <= 0 {
        return Ok(());
    }

    // Twelve monthly JEs across the 12-month sim window (epoch
    // 2025-04-01 → 2026-03-31), so depreciation accrues monthly the
    // way a real business books it (and recurs each epoch on reseed).
    let month_ends = [
        ("M01", "2025-04-30"),
        ("M02", "2025-05-31"),
        ("M03", "2025-06-30"),
        ("M04", "2025-07-31"),
        ("M05", "2025-08-31"),
        ("M06", "2025-09-30"),
        ("M07", "2025-10-31"),
        ("M08", "2025-11-30"),
        ("M09", "2025-12-31"),
        ("M10", "2026-01-31"),
        ("M11", "2026-02-28"),
        ("M12", "2026-03-31"),
    ];
    let je_url = format!("{ledger_base}/api/ledger/inventory-transferred");
    for (label, end_date) in &month_ends {
        let body = serde_json::json!({
            "total_cost_cents": monthly_cents,
            "debit_account": "6900",
            "credit_account": "1510",
            "happened_on": end_date,
            "memo": format!(
                "{} depreciation — assets (straight-line, {} year useful life)",
                label, USEFUL_LIFE_YEARS
            ),
            "source_table": "brewery_seed_depreciation_schedule",
            "source_id": format!("depreciation-{}", label.to_lowercase()),
            "created_by": "boss-brewery-data-seed",
        });
        let resp = client
            .post(&je_url)
            .headers(headers.clone())
            .json(&body)
            .send()
            .with_context(|| format!("POST {je_url} ({})", label))?;
        let status = resp.status();
        if !status.is_success() {
            let txt = resp.text().unwrap_or_default();
            anyhow::bail!("POST {je_url} ({}) returned {status}: {txt}", label);
        }
        info!(
            month = label,
            posted_on = end_date,
            amount_cents = monthly_cents,
            "asset depreciation JE posted (DR 6900 / CR 1510)"
        );
    }
    info!(
        annual_cents,
        monthly_cents,
        total_asset_cost_cents = total_cost_cents,
        useful_life_years = USEFUL_LIFE_YEARS,
        "asset depreciation schedule complete (12 monthly JEs)"
    );
    Ok(())
}

/// Seed the brewery's serialized assets from
/// `examples/brewery/data/assets.json`. Each row becomes an asset
/// (per-unit Subject) — POST /api/assets.
fn ensure_brewhouse_assets(
    client: &Client,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
    seeds_dir: &Path,
) -> Result<()> {
    let path = brewery_data_dir(seeds_dir).join("assets.json");
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let units: Vec<serde_json::Value> =
        serde_json::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
    let count = units.len();
    // Assets are event-sourced — there is no "POST a system" endpoint.
    // Each unit gets a Received event (binds sku + creates the
    // System row) followed by an Installed event (lands the unit in
    // the live in-service phase).
    let url = format!("{api_base}/api/assets/events");
    for unit in units {
        let asset_id = unit
            .get("asset_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("asset entry missing asset_id"))?;
        let sku = unit
            .get("sku")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("asset entry {asset_id} missing sku"))?;
        let oem_serial = unit
            .get("oem_serial")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let first_seen = unit
            .get("first_seen")
            .and_then(|v| v.as_str())
            .unwrap_or("2024-01-01");

        // SystemEvent uses internal-tag serde with `kind` discriminator;
        // variant fields sit at the top level alongside the tag.
        let received = serde_json::json!({
            "id": format!("evt-recv-{asset_id}"),
            "asset_id": asset_id,
            "ts": first_seen,
            "actor_id": "automation:brewery-data-seed",
            "kind": "Received",
            "sku": sku,
            "source": "oem-new",
            "oem_serial": oem_serial,
        });
        let resp = client
            .post(&url)
            .headers(headers.clone())
            .json(&received)
            .send()
            .with_context(|| format!("POST {url} (received {asset_id})"))?;
        let status = resp.status();
        if !status.is_success() && status.as_u16() != 409 {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("POST {url} (received {asset_id}) returned {status}: {body}");
        }

        let installed = serde_json::json!({
            "id": format!("evt-install-{asset_id}"),
            "asset_id": asset_id,
            "ts": first_seen,
            "actor_id": "automation:brewery-data-seed",
            "kind": "Installed",
            "holder_kind": "location",
            "holder_id": "loc-brewery-brewhouse",
        });
        let resp = client
            .post(&url)
            .headers(headers.clone())
            .json(&installed)
            .send()
            .with_context(|| format!("POST {url} (installed {asset_id})"))?;
        let status = resp.status();
        if !status.is_success() && status.as_u16() != 409 {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("POST {url} (installed {asset_id}) returned {status}: {body}");
        }
    }
    info!(
        count,
        "brewhouse assets seeded (received + installed events)"
    );
    Ok(())
}

/// Pre-seed the brewery's finished-product inventory at the
/// brewhouse cooler so the first sim-month's wholesale orders
/// have stock to consume while morning-brew Jobs catch up.
/// Without this, the first wholesale-keg-order in --hard-fail
/// regen 400s on POST /api/products/{sku}/inventory/consume
/// because the morning-brew → packaging step hasn't completed
/// yet (the Workflow takes ~5 sim-days to walk through its
/// 7-tier graph). Pre-seeded buffer = ~30 sim-days of
/// canonical wholesale demand.
///
/// Pairs with the products.toml seed comment about "Initial
/// on-hand at the brewhouse cooler is wired separately via
/// finished_product_inventory POSTs". Those POSTs are this
/// function.
fn ensure_finished_product_inventory(
    client: &Client,
    api_base: &str,
    headers: &reqwest::header::HeaderMap,
) -> Result<()> {
    // Step 1: ensure the brewery's finished-product catalog exists.
    // products.toml lists these SKUs but no separate seeder loads
    // them; this function does both jobs (catalog upsert + initial
    // inventory) so the brewery sim has a complete finished-product
    // setup on day 1. Keep these in sync with examples/brewery/seeds/
    // products.toml.
    let products = serde_json::json!([
        {"sku":"FP-PALE-1-2-BBL","name":"Pale Ale 1/2-BBL Keg","product_kind":"beer","package_unit":"1/2-bbl-keg","description":"Flagship pale ale.","metadata":{"abv_pct":5.4,"ibu":38,"style":"American Pale Ale","msrp_cents":16500},"active":true},
        {"sku":"FP-PALE-1-6-BBL","name":"Pale Ale 1/6-BBL Keg (Sixtel)","product_kind":"beer","package_unit":"1/6-bbl-keg","description":"Flagship pale ale, sixtel.","metadata":{"abv_pct":5.4,"ibu":38,"style":"American Pale Ale","msrp_cents":6500},"active":true},
        {"sku":"FP-IPA-1-2-BBL","name":"West Coast IPA 1/2-BBL Keg","product_kind":"beer","package_unit":"1/2-bbl-keg","description":"Hop-forward West Coast IPA.","metadata":{"abv_pct":6.8,"ibu":65,"style":"West Coast IPA","msrp_cents":18500},"active":true},
        {"sku":"FP-IPA-1-6-BBL","name":"West Coast IPA 1/6-BBL Keg (Sixtel)","product_kind":"beer","package_unit":"1/6-bbl-keg","description":"Hop-forward West Coast IPA, sixtel.","metadata":{"abv_pct":6.8,"ibu":65,"style":"West Coast IPA","msrp_cents":7500},"active":true},
        {"sku":"FP-STOUT-1-2-BBL","name":"Dry Irish Stout 1/2-BBL Keg","product_kind":"beer","package_unit":"1/2-bbl-keg","description":"Roast-malt forward, English ale yeast.","metadata":{"abv_pct":4.6,"ibu":30,"style":"Dry Irish Stout","msrp_cents":15500},"active":true},
        {"sku":"FP-STOUT-1-6-BBL","name":"Dry Irish Stout 1/6-BBL Keg (Sixtel)","product_kind":"beer","package_unit":"1/6-bbl-keg","description":"Roast-malt forward, English ale yeast, sixtel.","metadata":{"abv_pct":4.6,"ibu":30,"style":"Dry Irish Stout","msrp_cents":6000},"active":true},
        {"sku":"FP-LAGER-1-2-BBL","name":"Czech Pilsner 1/2-BBL Keg","product_kind":"beer","package_unit":"1/2-bbl-keg","description":"Pilsner malt, noble hops, lager yeast.","metadata":{"abv_pct":4.8,"ibu":38,"style":"Czech Pilsner","msrp_cents":16500},"active":true},
        {"sku":"FP-LAGER-1-6-BBL","name":"Czech Pilsner 1/6-BBL Keg (Sixtel)","product_kind":"beer","package_unit":"1/6-bbl-keg","description":"Pilsner malt, noble hops, lager yeast, sixtel.","metadata":{"abv_pct":4.8,"ibu":38,"style":"Czech Pilsner","msrp_cents":6500},"active":true},
        {"sku":"FP-HAZY-1-2-BBL","name":"Hazy IPA 1/2-BBL Keg","product_kind":"beer","package_unit":"1/2-bbl-keg","description":"Heavy late hops, biotransformation dry hop.","metadata":{"abv_pct":6.5,"ibu":50,"style":"Hazy IPA","msrp_cents":19500},"active":true},
        {"sku":"FP-HAZY-1-6-BBL","name":"Hazy IPA 1/6-BBL Keg (Sixtel)","product_kind":"beer","package_unit":"1/6-bbl-keg","description":"Heavy late hops, biotransformation dry hop, sixtel.","metadata":{"abv_pct":6.5,"ibu":50,"style":"Hazy IPA","msrp_cents":8000},"active":true},
        {"sku":"FP-SEASONAL-12OZ-CS","name":"Seasonal Release — 12oz Bottle Case","product_kind":"beer","package_unit":"12oz-case","description":"Limited-run seasonal in 24×12oz cases.","metadata":{"style":"varies","msrp_cents":5400},"active":true}
    ]);
    let url = format!("{api_base}/api/products/batch");
    let resp = client
        .post(&url)
        .headers(headers.clone())
        .json(&products)
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        anyhow::bail!("POST {url} returned {status}: {body}");
    }
    info!(count = 11, "finished-product catalog seeded");

    // Step 2: pre-seed the brewhouse cooler so the first sim-month's
    // wholesale orders have stock to consume while morning-brew Jobs
    // catch up. Without this, the first wholesale-keg-order in
    // --hard-fail regen 400s on POST /api/products/{sku}/inventory/
    // consume because the morning-brew → packaging step hasn't
    // completed yet (the Workflow takes ~5 sim-days to walk through
    // its 7-tier graph).
    //
    // Buffer sized for ~30 sim-days at the canonical
    // wholesale-keg-order rate (35 orders/day × line items × 30 days).
    // Per-line-item demand: PALE-1/2=4, IPA-1/6=8, STOUT/LAGER/HAZY-1/6=1
    // each. Sixtel co-products (PALE-1/6, IPA-1/2, STOUT/LAGER/HAZY-1/2)
    // accumulate from production with no consumer — buffered low so
    // the cooler has *something* on day 1 if a future Workflow reaches
    // for them.
    const BUFFER_PALE_HALF_BBL: i64 = 5000;
    const BUFFER_PALE_SIXTEL: i64 = 2000;
    const BUFFER_IPA_HALF_BBL: i64 = 1000;
    const BUFFER_IPA_SIXTEL: i64 = 10000;
    const BUFFER_STOUT_HALF_BBL: i64 = 500;
    const BUFFER_STOUT_SIXTEL: i64 = 1500;
    const BUFFER_LAGER_HALF_BBL: i64 = 500;
    const BUFFER_LAGER_SIXTEL: i64 = 1500;
    const BUFFER_HAZY_HALF_BBL: i64 = 500;
    const BUFFER_HAZY_SIXTEL: i64 = 1500;
    // The seasonal release is bottled in infrequent bursts (the
    // seasonal-release Workflow, ~50 cases/run) against steady allocation
    // demand (~50 cases/sale) — unlike the kegs that refill continuously
    // from daily morning-brew. With no opening buffer, a sale landing
    // between production runs 404s the invoice on `insufficient FG
    // stock`, so buffer it like the kegs.
    const BUFFER_SEASONAL_CASE: i64 = 1000;

    // Opening-FG standard cost — valued at the brew's real raw-materials +
    // packaging cost (BOM × ingredient prices) ÷ yield, split by keg
    // volume. At runtime `products.produce` derives the FG cost basis from
    // the actual consumed inputs at their real `avg_cost`, so this opening
    // stock just needs to be valued at that same standard cost. Valuing it
    // any lower dilutes the weighted-average basis and drives COGS-at-sale
    // toward zero.
    //
    // The 250-BBL BOM is ~$6,790 raw (e.g. 196×$25 2-row + 4×$300 Cascade)
    // + ~$1,250 packaging ≈ $8,040 over 210 half-BBLs + 315 sixtels =
    // 157.6 BBL → ~$51/BBL → ~$25.5/half-BBL, ~$8.5/sixtel.
    const COST_HALF_BBL: i64 = 2550; // ~$25.5 per half-BBL (real BOM ÷ yield)
    const COST_SIXTEL: i64 = 850; // ~$8.5 per sixtel
    // $18.60/case — matches the seasonal-release `package` step's
    // BOM-derived FG cost ($930 raw ÷ 50 cases). Keeps the opening
    // buffer on the same weighted-average basis as new production so
    // COGS-at-sale stays BOM-driven.
    const COST_SEASONAL_CASE: i64 = 1860;

    let location = "loc-brewery-brewhouse";
    // (sku, qty, unit_cost_cents)
    let seeds: &[(&str, i64, i64)] = &[
        ("FP-PALE-1-2-BBL", BUFFER_PALE_HALF_BBL, COST_HALF_BBL),
        ("FP-PALE-1-6-BBL", BUFFER_PALE_SIXTEL, COST_SIXTEL),
        ("FP-IPA-1-2-BBL", BUFFER_IPA_HALF_BBL, COST_HALF_BBL),
        ("FP-IPA-1-6-BBL", BUFFER_IPA_SIXTEL, COST_SIXTEL),
        ("FP-STOUT-1-2-BBL", BUFFER_STOUT_HALF_BBL, COST_HALF_BBL),
        ("FP-STOUT-1-6-BBL", BUFFER_STOUT_SIXTEL, COST_SIXTEL),
        ("FP-LAGER-1-2-BBL", BUFFER_LAGER_HALF_BBL, COST_HALF_BBL),
        ("FP-LAGER-1-6-BBL", BUFFER_LAGER_SIXTEL, COST_SIXTEL),
        ("FP-HAZY-1-2-BBL", BUFFER_HAZY_HALF_BBL, COST_HALF_BBL),
        ("FP-HAZY-1-6-BBL", BUFFER_HAZY_SIXTEL, COST_SIXTEL),
        (
            "FP-SEASONAL-12OZ-CS",
            BUFFER_SEASONAL_CASE,
            COST_SEASONAL_CASE,
        ),
    ];
    for (sku, qty, unit_cost_cents) in seeds {
        // Step 2a: upsert the FG row with on_hand + the row's exact
        // conserved value (qty × the authored unit cost — PR 6a,
        // value-primary). products.consume drains this value
        // proportionally at sale to recognize COGS (Model B:
        // DR 5100 / CR 1320); the display per-unit cost is derived.
        let value_cents = qty.saturating_mul(*unit_cost_cents);
        let url = format!("{api_base}/api/products/{sku}/inventory");
        let body = serde_json::json!({
            "product_sku": sku,
            "location_id": location,
            "on_hand": qty,
            "allocated": 0,
            "value_cents": value_cents,
        });
        let resp = client
            .put(&url)
            .headers(headers.clone())
            .json(&body)
            .send()
            .with_context(|| format!("PUT {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("PUT {url} returned {status}: {body}");
        }
        info!(
            sku = %sku,
            location = %location,
            on_hand = qty,
            unit_cost_cents = unit_cost_cents,
            "finished-product starter inventory upserted",
        );

        // The opening FG asset's GL leg is owned by the products API:
        // the PUT above posts the atomic DR 1320 / CR 3000 JE (sized
        // at exactly the row's conserved value) AND emits the
        // ledger.inventory.transferred event rebuild re-projects it
        // from. The explicit second post this seed used to make
        // conflicted on the same opening-fg-{sku} key and — once
        // emits were gated on inserted (#86) — muted the only rebuild
        // source. One writer owns the fact and its event.
    }
    Ok(())
}

/// Seed the brewery's day-1 raw-material opening balances — BOTH the
/// physical stock and its matching GL value — from `parts.toml`.
///
/// Mirrors `ensure_finished_product_inventory` (the same, for finished
/// goods): the brewery already owns these sacks of malt and boxes of hops
/// on day 1.
///   1. Physical: POST every part to `/api/inventory/items/batch` so
///      `on_hand` + reorder points exist. WITHOUT THIS every brew's
///      `production-consume` defers forever on short-ingredients and the
///      brewery never brews — the live-from-empty demo's production stall.
///   2. GL: the batch endpoint itself posts the atomic DR 1300 /
///      CR 3000 opening JE per part and emits its rebuild event —
///      idempotent on `opening-raw-{sku}`, one writer.
///
/// Consolidated here from the offline engine's `seed_parts` so ONE seed
/// path owns raw materials for both the live demo and the CI regen.
fn ensure_raw_inventory_opening_balances(
    client: &Client,
    inventory_base: &str,
    headers: &reqwest::header::HeaderMap,
    seeds_dir: &std::path::Path,
) -> Result<()> {
    let parts = crate::load_parts(seeds_dir)
        .with_context(|| format!("load_parts from {}", seeds_dir.display()))?;
    if parts.is_empty() {
        info!("no parts.toml found — skipping raw-material opening balances");
        return Ok(());
    }
    // 1. Physical raw stock (on_hand + reorder points). Same payload the
    // inventory batch endpoint takes from the engine's old seed_parts.
    let batch_url = format!("{inventory_base}/api/inventory/items/batch");
    let batch_resp = client
        .post(&batch_url)
        .headers(headers.clone())
        .json(&parts)
        .send()
        .with_context(|| format!("POST {batch_url}"))?;
    let batch_status = batch_resp.status();
    if !batch_status.is_success() {
        let body = batch_resp.text().unwrap_or_default();
        anyhow::bail!("POST {batch_url} returned {batch_status}: {body}");
    }
    info!(
        count = parts.len(),
        %batch_url,
        "seeded physical raw inventory (on_hand + reorder points)"
    );

    // The raw opening-balance GL legs are owned by the inventory API:
    // the batch upsert above posts one atomic DR 1300 / CR 3000 JE per
    // part (sized at exactly the row's conserved value_cents) AND
    // emits the ledger.inventory.transferred event rebuild re-projects
    // each from. The explicit per-part second post this seed used to
    // make conflicted on the same opening-raw-{sku} keys and — once
    // emits were gated on inserted (#86) — muted the only rebuild
    // source. One writer owns the fact and its event.
    Ok(())
}

/// Cash working-capital opening balance. The brewery doesn't
/// open its doors with $0 in the bank — without this JE, the
/// first payroll run + tax payment drives 1000 Cash net-negative
/// weeks before any AR conversion lands. Same shape as the FG +
/// raw + ledger opening JEs: DR 1000 / CR 3000 Retained Earnings,
/// idempotent source_id `opening-cash-brewery` so re-runs and
/// parallel emitters collide cleanly.
///
/// Algedonic Ales is an industrial-scale brewer with ~800
/// employees (see brewery_employees seed) whose biweekly payroll
/// run nets ~$1.44M cash. $10M covers ~7 pay periods plus COGS +
/// opex burn, giving the multi-month sim enough runway before AR
/// collections start landing. The number is data, not
/// load-bearing — tenants set their own seed capital by editing
/// this constant.
const CASH_OPENING_BALANCE_CENTS: i64 = 1_000_000_000; // $10,000,000

fn ensure_cash_opening_balance(
    client: &Client,
    ledger_base: &str,
    headers: &reqwest::header::HeaderMap,
) -> Result<()> {
    let je_url = format!("{ledger_base}/api/ledger/inventory-transferred");
    let body = serde_json::json!({
        "total_cost_cents": CASH_OPENING_BALANCE_CENTS,
        "debit_account": "1000",
        "credit_account": "3000",
        "memo": "Opening balance — brewery working capital (Cash ← retained earnings)",
        "source_table": "brewery_seed_opening_balance",
        "source_id": "opening-cash-brewery",
        "created_by": "boss-brewery-data-seed",
    });
    let resp = client
        .post(&je_url)
        .headers(headers.clone())
        .json(&body)
        .send()
        .with_context(|| format!("POST {je_url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        anyhow::bail!("POST {je_url} returned {status}: {body}");
    }
    info!(
        total_cost_cents = CASH_OPENING_BALANCE_CENTS,
        "cash opening-balance JE posted (DR 1000 / CR 3000 Retained Earnings)"
    );
    Ok(())
}

/// Seed the excise-tax rate schedules from
/// `seeds/excise_rates.toml` into the ledger's
/// `excise_rate_schedules` registry (brewery-fidelity Q4: rates are
/// registry data, not rule args). PUT is an upsert on
/// `(jurisdiction, effective_from)`, so re-running prepare converges
/// instead of duplicating; tier-shape validation happens ledger-side
/// so the constraint has one home. Without these rows the accrual
/// endpoint falls back — loudly — to the dispatcher rule's flat
/// $3.50/bbl, which understates real TTB liability ~3.7× at the
/// tenant's stated volume.
fn ensure_excise_rate_schedules(
    client: &Client,
    ledger_base: &str,
    headers: &reqwest::header::HeaderMap,
    seeds_dir: &Path,
) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct ExciseRatesSeed {
        #[serde(default)]
        schedule: Vec<ExciseScheduleSeed>,
    }
    #[derive(serde::Deserialize)]
    struct ExciseScheduleSeed {
        jurisdiction: String,
        effective_from: String,
        tiers: Vec<serde_json::Value>,
    }

    let path = seeds_dir.join("excise_rates.toml");
    let seed: ExciseRatesSeed = toml::from_str(
        &std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))?;

    let url = format!("{ledger_base}/api/ledger/excise-rate-schedules");
    for schedule in &seed.schedule {
        let body = json!({
            "jurisdiction": schedule.jurisdiction,
            "effective_from": schedule.effective_from,
            "tiers": schedule.tiers,
        });
        let resp = client
            .put(&url)
            .headers(headers.clone())
            .json(&body)
            .send()
            .with_context(|| format!("PUT {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            anyhow::bail!(
                "PUT {url} for {} @ {} returned {status}: {text}",
                schedule.jurisdiction,
                schedule.effective_from
            );
        }
        info!(
            jurisdiction = %schedule.jurisdiction,
            effective_from = %schedule.effective_from,
            tiers = schedule.tiers.len(),
            "excise rate schedule seeded"
        );
    }
    Ok(())
}

/// Best-effort reachability probe. Returns false on any error
/// (connection refused, timeout, non-2xx) so callers can skip a
/// seed step without raising.
fn base_reachable(client: &Client, base: &str, health_path: &str) -> bool {
    let url = format!("{base}{health_path}");
    matches!(
        client
            .get(&url)
            .timeout(std::time::Duration::from_secs(2))
            .send(),
        Ok(r) if r.status().is_success()
    )
}
