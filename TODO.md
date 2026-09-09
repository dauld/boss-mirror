# TODO

BOSS's public roadmap — open, forward-looking work. **Done work lives
in [CHANGELOG.md](CHANGELOG.md)** — don't restate it here.

This is a **preliminary release**: the core shape is in place — the
four primitives (Subjects, Jobs, Steps, Events), the event-sourced
audit log, and the registry-driven extensibility model (Workflows,
StepTypes, Classes, StepPlugins). The buckets below are what's
deliberately *not* done yet. Treat [CHANGELOG.md](CHANGELOG.md) and
[docs/architecture-decisions.md](docs/architecture-decisions.md) as the
source of truth for what already exists, and verify against the working
tree before acting on a detailed item.

## Where to read first (orientation for a new contributor)

- [`docs/architecture-decisions.md`](docs/architecture-decisions.md) — the consolidated decision record (Workflow v2 shape, dispatcher-as-event-router, step types as property bundles, all of it)
- [`docs/design/extending-boss.md`](docs/design/extending-boss.md) — the extensibility ladder (Workflows, StepTypes, StepPlugins)
- [`docs/design/class-registry.md`](docs/design/class-registry.md) — registry-backed taxonomy pattern
- [`docs/design/correctness-protocol.md`](docs/design/correctness-protocol.md) — five-property invariant (provenance, conservation, closure, idempotence, determinism)
- [`docs/design/human-powered-state-machine.md`](docs/design/human-powered-state-machine.md) — reading frame for what BOSS *is*

The three buckets below sort by how soon a contributor should
reach for them, not by importance: **open questions** → items
blocked on a decision before they land; **post-release strategic**
→ bigger shapes deferred until a real tenant steers the priority;
**post-release polish** → automation + UX nice-to-haves.

---

## Near-term queue (2026-07-03)

The active workstream order after the costing-fidelity arc (#51–#63),
the overhead-absorption review cleanup (#73), and the deep-gate
first-contact fixes (#74). Sorted by dependency, not size.

- [x] **Full-year regen validation of #73 + #74** — done 2026-07-13:
      the capstone 365-day from-empty run (run 3, ephemeral VM)
      passed every gate. 0 dead-letters + 0 transient redeliveries;
      determinism live == rebuilt; deep replay 267,209 facts /
      267,146 entries, 0 divergences; conservation N + P exact to
      the cent (1300 ≡ raw $3,718,828.00; 1320 ≡ FG $3,019,403.03);
      full-year P&L rev ~$42.4M / COGS ~$10.5M / GM 75.2%. Runs 1–2
      surfaced two fixes along the way (#104 write-off dual-shape
      adapter, #109 case-packaging buffer); evidence posted on both.
- [x] **Costing PR 6 — WIP reconciliation** — landed as the
      value-primary costing arc (6a/6b): inventory rows carry the
      line-total `value_cents` as primary, and the GL reconciles
      against Σ value_cents exactly on both sides (invariants N +
      P; run-3 evidence above, to the cent). The remaining 1310
      balance is the year-end WIP-variance close item below — a
      close-rule gap, not a costing residual.
- [x] **Overhead rates as dispatcher rule args × the step's bbl** —
      done: `infra/dispatcher/rules/` carries `rate_cents_per_bbl`
      args on the three `inventory.overhead.absorb` rules (direct
      labor → 6100, process utilities → 6300, production
      depreciation → 6900), multiplied by the step's actual bbl at
      runtime — the excise pattern. The
      `overhead_absorption_rules_agree` test pins the absorb-args ↔
      produce-drain-set contract.
- [x] **`HandlerError::Permanent`** — done. The house contract does
      the classification: services answer **422** for deterministic
      request-data errors (seed typos, malformed bodies), and the
      shared post/get helpers map exactly that status to `Permanent`;
      409 (convergent conflicts, e.g. insufficient stock — load-bearing
      for the 6b backorder path) and 404 (not-yet-projected) stay
      retryable, as do all 5xx. The runner Terms an event only when
      EVERY failed handler is deterministic (Permanent/MissingArg/
      BadArgType); any transient in the mix NAKs so the idempotent
      re-run converges it. Permanent Terms log the same `DEAD-LETTER:`
      pattern with `class=permanent`, so gates and operators keep one
      vocabulary.
- [x] **Dedup audit-event emits on redelivery** — done for the
      fact-backed occurrence events (`record_fact_in_tx` returns
      `FactRecorded { id, inserted }`; ledger manual-entry +
      inventory-movement, inventory receive (`ReceiveApplied`) and
      overhead-absorbed all gate their emits; consume/produce were
      already gated). DELIBERATELY left at-least-once: state-snapshot
      events (`ITEM_UPSERTED`, `PRODUCT_INVENTORY_UPSERTED`,
      `INVOICE_CREATED`, …) — they are the last-write-wins rebuild
      sources, so gating them would turn projection recovery into
      at-most-once; a duplicate snapshot is harmless by design. The
      CRUD surfaces (vendors/contacts/accounts-team) emit per client
      call and sit outside the NAK-redelivery path — revisit only if
      client retries show up in the log.
- [x] **Costing PR 7 — BOM expansion** — done where defensible:
      per-style ingredient variety (pilsner base + lager yeast for the
      lager; roasted barley for the stout; flaked wheat + Citra for
      the hazy; Cascade/Citra split for the IPA — six new sourceable
      SKUs at 2025 wholesale) and real case packaging
      (PKG-CAN-12OZ-CS24, ~$3.80/case bundle — the 12oz format's cans
      were free before, understating its COGS ~17%). Water is
      DELIBERATELY not a part: it is already costed inside the 6300
      process-utilities absorption driver, so a water BOM line would
      double-count (documented in parts.toml). Everything flows
      through drain-actual-wip automatically — no stamped amounts
      anywhere (per #77).

## Read-surface + coherence audit (2026-08-06)

A measured audit of the read surfaces after the app-split, global-search
and Views arcs (#178–#187). Every item below carries the measurement it
came from, taken against the live playground rather than read off the
code. Ordered by what to reach for first.

### Correctness

- [ ] **C1 — `event_facts` and `search_index` have no refresh path.**
      Both are rebuild-only. Measured mid-session: the log head was at
      `6,948,127` and `event_facts` at `6,894,632` — **53,495 events
      behind**, and climbing continuously while the sim runs. Seven
      systemd timers exist (conservation invariants, ledger
      replay-check, audit integrity, recognition, ML batch, messages
      purge, files GC) and **none refreshes a projection**.
      `catch_up_event_facts` is written, tested and exported; nothing
      calls it. Views and global search are the two surfaces an
      operator most expects to be live, and both are correct only in
      the minutes after a manual `boss-rebuild-all`.
      *Search needs its own answer:* its index caps events per Subject
      via `ROW_NUMBER`, so it has no incremental form today — a
      periodic full rebuild (~77s at 1.4M events) is the honest
      interim.
- [x] **C2 — the ledger read surface is unscoped.** Done 2026-08-06
      (#188): `Resource::ledger()` in the registry, enforced by a
      router-wide layer rather than another per-handler check.
      Original finding: `boss-ledger`
      carries exactly one authorization check across its whole HTTP
      surface (`user.role == "auditor"`) over 211,512 `financial_facts`
      rows plus the GL and bank settlements. It reads `CurrentUser` in
      seven files but never scopes rows to the caller.
      *Note:* an earlier pass over-reported this as four unpoliced
      crates by grepping for `scope_predicate`; that misses the crates
      using `CurrentUser` directly. `boss-subject-kinds` and
      `boss-customers` read no caller at all but expose only registry
      data and 73 customer rows. The ledger is the real gap.
- [x] **C3 — duplicated facts kept in step by comment.** Done
      2026-08-06 (#188): convention recorded in CLAUDE.md §9a; the
      ports ↔ deploy-array pair pinned by a test that immediately
      caught live drift (`observability`, `simulator` missing from the
      fallback); `VENDOR_COUNT` collapsed from two hardcoded 13s to one
      `pub const`. Original finding: Three
      instances caused real failures in a single session: `boss-ports`
      ↔ the `deploy-services.sh` fallback arrays (search and views were
      not deployable at all), `MODEL_ROUTES` ↔ `MODEL_KINDS` (pages
      rendering under the wrong tab), `manifest.txt` ↔ `SCHEMA_FILES`
      (every DB-backed test running without two tables). A scan finds
      12+ more sites carrying "keep this in sync" / "must match"
      comments. Two of the three are now pinned by tests; the pattern
      is not. Wants a convention — a fact that lives twice gets an
      equality test — and possibly a lint on the comment phrasing that
      marks them.
- [x] **C4 — idempotence has no static guard.** Done 2026-08-06
      (#189): `infra/lint/idempotence-ratchet.sh`, wired into CI.
      Flags accumulating writes (`col = col + $n`), which
      double-apply under at-least-once redelivery — the 2026-06-16
      class where `on_hand` mutations decoupled GL 1300/1320 from
      physical stock and raised nothing. A ratchet, not a ban: the
      two known stock sites are listed with why they are safe, and
      a third fails until reviewed. Original finding: Guard coverage across
      the five properties: conservation 3 tests / 4 lints, provenance
      4 / 1, closure 1 / 2, determinism 2 / 0, idempotence 2 / 0.
      Determinism is well covered at runtime by the replay-check
      timer; idempotence is the genuinely thin one, and it is the
      property that produced the JetStream double-side-effect class.

### Features

- [x] **F1 — Steps has no read surface.** Done 2026-08-06 (#189):
      `steps` is a View source, scoped on `Resource::step()` with the
      owner allow-list landing on `assignee_id`, and pushdown on
      `status` / `kind` / `assignee_id`. No projection needed — those
      three are already indexed, including a partial index built for
      exactly this question. Original finding: Views offers Subjects, Jobs
      and Events — three of the four primitives. Steps is 160,254 rows
      and is where work state lives; "my ready steps" cannot be
      expressed as a View.
- [x] **F2 — Views' other two sources are still capped.** Done
      2026-08-06 (#189): `jobs` and `subjects` have pushdown
      descriptors, so all four sources narrow in the database. Every
      pushable job column is already indexed; `subjects` is keyed
      `(kind, id)` and both are TEXT, so a filter on either rides the
      primary key. Original finding: Only
      `events` received a projection in #186. `subjects` and `jobs`
      still read base tables under the 5,000-row scan with no
      pushdown, carrying the same silent-truncation behaviour `events`
      had before. Jobs is 20,555 rows, so the cap does not bite yet.
- [x] **F3 — the app tabs are code while the org is data.** Done
      2026-08-06 (#189): `DEPARTMENT_APP` maps all 19 department
      Classes to an app, pinned by a test that reads the seed files
      rather than restating them. Tabs are deliberately NOT derived —
      that would produce 19 and undo the app split. Surfaced a
      follow-up: departments come from two sources (12 core, 8
      tenant), so `apps/web` maps departments a tenant invented; a
      second in-tree tenant needs a real extension point.
      Original finding: Eleven
      employee departments in the Class registry; eight apps
      hand-authored in `libs/web-kit/src/nav.ts`. Four departments
      collapse into Operations and `audit` has no app at all. Nothing
      keeps the two in step and no test fails when they diverge —
      Principle 9 sitting in the one layer the app split did not put
      on data.
- [x] **F4 — five Subject kinds no app claims:** Done 2026-08-06
      (#189). Only TWO were real gaps — `message` (13,483 rows,
      surfaced by Inbox) and `custom` (the design-doc-review Job
      subject). The other three are ABSTRACT ROOTS of the
      subject-kind taxonomy (`person`, `object`, `intangible`);
      nothing is ever an instance of them, so no app should claim
      them. The audit's guess that `person` duplicated `employee`
      was wrong — `employee` specializes `person`. Pinned by a test
      that exempts roots-with-children structurally. Original
      finding: `custom`,
      `intangible`, `message`, `object`, `person`. `message` is the
      notable one — Inbox exists as a surface, but no app claims the
      kind, so global search never floats messages. `person` alongside
      `employee` with no surface is either a duplicate or an
      unfinished kind.
- [x] **F5 — pushdown residuals.** Done 2026-08-06 (#189) for the
      one that was not merely scan cost: payload paths.
      `payload.sku = "…"` pushed nothing, so the scan took the
      newest N rows and filtered in-process — it reported **0
      against a true 351**, the same silent-zero class #186 fixed
      for column terms. Dotted paths now push as
      `payload #>> $1::text[]`, with the path bound rather than
      interpolated. `NOT` and text-ordering remain residual and
      genuinely are only scan cost. Original finding: `NOT` needs an exactness proof
      before it can be pushed (negation inverts the direction of
      approximation, turning a superset into a subset). Ordering
      comparisons on text columns and any term reaching into `payload`
      stay residual-only. None of these can affect correctness — the
      residual is always the whole predicate — they are scan cost.

## Subject-model audit residuals (2026-07-14 appendix, re-homed 2026-07-29)

The R1–R4 workstream + both edge PRs (#137, #159) shipped; the
design doc flattened into the baseline record and its appendix
defect worklist — independent, own-PR-sized findings of record —
lives here now. Items verified still open at re-homing:

- [x] `assets.phase` closed DB CHECK — dropped (#163); vocabulary
      lives in the Class rows, validated at the API boundary.
- [x] `classes.subject_kind` FK to `subject_kinds` — added (#161),
      with a 422 on batch-upsert against an unregistered kind.
- [x] `purchase_orders` vendor column duality — the vestigial FK'd
      `vendor_id` dropped (#162); the `vendor` TEXT ref is the one
      column, edge-enforced against subjects (#159).
- [x] Closed enums that should be Classes: `PoStatus`,
      `VendorInvoiceStatus`, `DocumentAudience` — all three lifted
      to String newtypes + Class rows + API-boundary gates
      (#164, #165).
- [x] `products.product_kind` + `package_unit` — validated on upsert
      against the Class registry keyed (subject_kind='product', code)
      (#166). Neither carries a DB CHECK by design; a tenant adds a
      kind/unit by seeding a `product` Class. Made the products.toml
      doc claim (previously package_unit only) true for both fields.
- [x] Invoice `revenue_category` — validated on every line against the
      Class registry keyed (subject_kind='invoice', code). Revenue
      categories are a taxonomy the `invoice` subject owns
      (01-registries.sql), seeded per-tenant (classes.json); the
      `invoice` Class namespace carries both status + revenue-category
      codes without collision. Not a Class of a fake "revenue" subject
      and not config-vocab (no module reads tenant.toml) — the invoice
      IS the subject, mirroring the existing invoice-status gate.
- [x] Vendor `payment_terms` — DB CHECK dropped; lifted to a
      Class-of-`vendor` vocabulary (net-30/45/60, prepaid) validated on
      the vendor write path keyed (subject_kind='vendor', code), NULL
      permissive (#168).
- [ ] Deferred taxonomy lifts — re-triaged 2026-07-30:
      • `account_type` is ALREADY a Class + validated (accounts.rs) —
        done, not open.
      • account `tier` is a doc contradiction: `22-accounts.sql` calls
        it a Class-registry field, but class-registry.md §"When NOT to
        use" calls `tier` per-row Subject config, not a Class. Resolve
        the disposition before acting.
      • revenue categories + tax jurisdictions/rates in `rules.toml` are
        dispatcher RULE ARGS (`default_revenue_category`,
        `tax_jurisdiction`, `taxable_categories`, `tax_rate_bps`), not a
        taxonomy table — a different shape than a Class lift.
- [x] Vendor `category` — validated on the vendor write path against
      (subject_kind='vendor', code), sharing the `check_vendor_class`
      helper with `payment_terms` (#169). Completed the category Class
      vocabulary: the 8 plain OpEx categories the loaded classes.json
      never carried (only .toml had them — drift) + `uncategorized` (the
      sim's spawn default) now seed, so every posted category resolves.
- [ ] `brewery-hire` subject shape (an existing employee as the
      hire target) — suppressed with `rate=0`; needs remodeling,
      likely subject = the requisition or the org.
- [x] Dead-by-construction seed ids — verified stale 2026-07-29:
      `loc-hq` has its locations row (01-registries seed) and all 30
      `acc-prospect-*` accounts are in the sim pool with subjects.
- [x] `assets.account_id` holding location ids — resolved by the Q5
      typed holder pair (#134) + custody edges (#159).

## Open questions — back-compat cleanup

The pre-release back-compat audit found vestigial code preserving
compatibility with implementations that no longer exist after the
public cut. Each removal needs a small decision before landing.

- [x] **BC2: `StepStatus` `parse_step_status` fallback** — collapsed
      2026-07-10. Confirmed: the alias arms were already gone, the
      live DB carries only the five canonical values, zero
      done/waived rows exist, the schema CHECK pins the column to the
      vocabulary, and no SPA helper maps legacy statuses. What
      remained was a silent `_ => Pending` catch-all — a storage-
      corruption masker (garbage rows reanimated as pending steps).
      Now strict: `Option` + a Storage error naming the offending
      value, threaded through the row mappers.

- [x] **BC20: legacy churn-risk duplication** — resolved 2026-07-10,
      with a corrected finding: `boss-accounts/src/account_risk_scores.rs`
      is NOT legacy — it is the post-cutover READ endpoint (it serves
      `/api/people/accounts/risk-scores` straight from `ml_predictions`
      written by the `account-churn-risk-composite-v1` plugin, and two
      SPA pages consume it), so module + route STAY. The actual dead
      duplicate was `boss-ml::generators` + the `boss-ml-generate` bin:
      a parallel churn/mtbf/win-prob scorer referenced by no timer, no
      deploy list, and no consumer (the deployed path is
      boss-ml-inference-batch.timer → infer-batch → plugin registry).
      Deleted (~500 LoC). The mtbf/win-prob model registry rows stay —
      declared phase-two catalog candidates, plugin-less by design.

- [x] **BC-tail: remaining safe-delete items** — landed as #103
      (2026-07-12): `run_days` shim dropped (rename landed),
      `SubjectCadence::fires_on(day)` inlined into `fires_on_tick`,
      `sku` ↔ `part_sku` serde aliases dropped (one canonical
      name), `paginated.normalise` bare-array fallback removed
      (the envelope is the only accepted shape), `entity_path`
      emitters fixed + the inbox fallback dropped. One survivor,
      deliberately: **`commerce.paid_on` `_day` alias** is
      load-bearing (counterparty trigger uses `inject_day`); keep
      until the sim retires the `_day` injection path. Not
      back-compat — leave alone.

---

- [x] **Deep replay-check write-stall window** — fixed: both replay
      checks now shadow the mutable ledger tables with TEMP clones
      (`pg_temp` name resolution carries the whole existing replay
      path unchanged) under REPEATABLE READ, so live tables are never
      locked or written and concurrent writers see one consistent
      snapshot instead of a frozen world. The entry-level check's
      open-period DELETE row-locks went with it. Lock-freedom is
      pinned by a race test (concurrent writer with a 2s statement
      timeout across both the mechanism and the real deep check).
      #95's timer quiescence stays as belt-and-braces for regens.

## Post-release — strategic

Big-shape work pulled out of the launch path on purpose. None of
this gates the release; reopen once a real tenant's needs steer
which one matters first.

- [x] **Edge-strip hardening for the gateway trust boundary** —
      done 2026-07-13 for the two code-level pieces: the gateway
      now unconditionally strips inbound `x-boss-*` at its edge
      before injecting session-derived identity (strip-then-inject
      ordering pinned by middleware probe tests), and
      `infra/deploy-services.sh` emits `http_bind = 127.0.0.1` for
      every backend. [SECURITY.md](SECURITY.md) §Deployment trust
      model updated: a front proxy's header stripping is
      defense-in-depth now, not load-bearing. Deliberately NOT
      done here: fail-closed on the `change-me` bootstrap-admin
      default — the public demo baseline signs in with it, so the
      fail-closed shape (refuse to start? force first-login
      rotation? demo-mode exemption?) needs a product call; it
      stays bundled in **Integrated IAM** below.

- [x] **`boss-rebuild-all --audit-log-seed` pipe deadlock on slow
      disk** — resolved 2026-07-12, with a corrected finding: the
      loader in the public tree already streams gunzip→psql (the
      `wait4(gzip)`-while-pipe-full order the HDD VM hit predates
      the cut), so the async rewrite the original note proposed
      would have replaced a sound pattern. What the slow-disk
      report actually needed: the pump is now extracted and
      test-pinned (backpressure completes; the reap-only-after-
      the-read-end-drops order is structural), a psql death
      mid-load reports its exit status instead of the misleading
      `broken pipe`, progress logs every 256 MiB so a multi-hour
      HDD load is distinguishable from a hang, and the non-gz
      branch's pointless whole-file `read_to_end` (an OOM
      candidate on multi-GB dumps) is gone. The two-step manual
      load stays a valid operator workaround on old binaries.

- [ ] **Income statement + cash flow report aggregation
      audits.** The balance-sheet endpoint fix closed one shape of
      the bug class — every JE balances by trigger, but a reporting
      endpoint can still mis-roll the categories. The BS endpoint
      now has invariant S asserting A=L+E in the daily conservation
      sweep + a property test in `boss-ledger/tests/http_api.rs`.
      The sibling reports (`/api/ledger/income-statement` and
      `/api/ledger/cash-flow`) need the same shape: structural
      audit that each endpoint's output satisfies its own
      invariants (revenue − COGS − OpEx = net income; cash-flow
      report's net-change-in-cash matches GL `account=1000`
      delta over the period). Mirror the BS pattern: refactor
      to a single kind-bucketed query where it isn't already,
      add `T` (income statement) and `U` (cash flow) invariants
      to the sweep, add property tests with multi-period seed
      data.

- [ ] **Sim-time threading across all financial facts.** The
      `consume_part` fix threaded `sim_day` through one HTTP path
      so auto-restock Jobs land on the sim's timeline instead of
      wallclock today. The same shape applies to every other
      side-effect emitter: products.produce / products.consume,
      inventory.receive, bill-approval, payroll-run-submit. Every
      one of those stamps wallclock `Utc::now()` for the resulting
      `financial_facts.happened_on`, so the live sim's facts would
      collapse onto install-day instead of the sim's timeline and
      break every date-keyed report. Concrete evidence of the class
      (2026-07-13): a full-stack service restart left 10 audit rows
      wall-stamped via the clock-client fallback
      (`boss-clock-client/src/lib.rs:289`) while clock-api was
      mid-restart — outside the epoch, wiped at the next reset.
      Same failure shape, different entry point: time must thread
      from the triggering event, not be re-fetched (or defaulted)
      at emit time.

- [ ] **Remaining "synthesized-amount" structural issues in the
      ledger.** Same bug-class as the `invoice_issued` COGS
      shortcut (since fixed): a posting rule or bridge invents a JE
      amount instead of deriving it from real per-row data. Five
      remaining of the same shape — none block the launch now that
      COGS is honest:
        - **bill_approved posts a lump `amount_cents`** instead
          of summing `purchase_order_lines.unit_cost_cents ×
          qty` off the linked PO. Memo + line breakdown lost at
          the GL. Fix: `BillApproveEmitter` should compute the
          total from the bridge's `expected_items` array (or
          the live PO); `bill_approved` rule should accept a
          per-SKU breakdown.
        - **revenue_recognized V2 emits zero COGS.** Ratable
          revenue recognizes 100% gross margin until V3 ships.
          Fix when ratable contracts become a real demo flow.
        - **payroll synthesize uses bps tables on annual
          salary.** Flat tenant-wide withholding / employer-tax
          rates stand in for per-employee jurisdiction-specific
          calcs. OK for the sim; revisit when a real tenant
          brings real W-4s.
        - **tax_accrued / tax_remitted trust payload amount
          verbatim** rather than reconciling against the actual
          `gl_journal_lines` balance for the period. A drift
          between snapshot and ledger leaves a residual on the
          liability account. Fix: validator should pull the
          `SUM(credit - debit)` for the (account_code, period)
          window and reject mismatches.
        - **commerce bridge applies tax as flat bps of line
          sum** rather than per-jurisdiction lookup with line
          exempt handling. Flagged in-code as sim-only.

- [ ] **Period close Workflow / cadence.** The validation script's
      step 7b creates + closes past fiscal years via `POST
      /api/ledger/periods/{id}/close`. Live playgrounds still need
      a Workflow that fires at year-end so ongoing sim activity
      continues to close cleanly. Existing
      `[periodic.quarterly-sales-tax]` infra is a model; new
      Workflow `fiscal-year-close` with a `tax-remittance`-like
      terminal step that POSTs to
      `/api/ledger/periods/{id}/close`.

- [x] **Ledger: WIP-variance year-end close** — done: the close
      endpoint computes the residual WIP balance (cumulative
      through `ends_on`) and stamps `wip_variance_cents` +
      `wip_account` into the `finance.period.closed` fact; the rule
      posts DR RE / CR wip (mirror for an over-absorbed credit
      residual) as separate lines from the net-income roll, so WIP
      closes to 0 without re-inflating the drained expense account.
      The account decision landed payload-driven: the rule stays
      chart-agnostic, the endpoint owns the starter-chart `1310`
      default via a `wip_account` body param — the
      `retained_earnings_account` contract exactly. The pinned test
      is un-ignored (+ negative/zero/missing-account siblings + an
      e2e through trial balance). Concrete case that motivated it:
      the 2026-07-13 capstone run's Dec-31 close left $42,859.20
      sitting on 1310; the next year regen's close should write it
      off and leave 1310 at in-flight-brews-only (invariant Q
      tightens accordingly).

- [x] **Conservation-P: finished-goods cost-basis reconciliation
      (consume-side)** — resolved by the value-primary costing arc,
      and it settled the open decision the stronger way: the
      closure is EXACT, not tolerance-based. Invariant P
      (`infra/lint/conservation-invariants.sh`) asserts GL 1320 ≡
      Σ `value_cents` over live FG rows — value-primary rows made
      the physical side an integer sum, so the moving-average
      rounding class this item described no longer exists. The
      2026-07-13 capstone year run held it to the cent
      ($3,019,403.03) across a 267K-fact year.

- [ ] **Sales-tax accrual on retail / taproom invoices.** The
      brewery's wholesale invoices are tax-exempt (resale), but
      retail + taproom line items should accrue sales tax. The
      `commerce.invoice.issue` bridge supports `tax_rate_bps` +
      `tax_jurisdiction` already, but the side-effect spec
      doesn't set them. Once retail / taproom Workflows exist
      (see below), wire `tax_rate_bps` on their billing steps.
      The `[periodic.quarterly-sales-tax]` Workflow is disabled
      because it remitted without an accrual flow; re-enable when
      the flow is whole.

- [ ] **Taproom-pour and distribution-contract Workflows.** 4120
      (Taproom) and 4140 (Distribution Contracts) are revenue
      accounts that no Workflow currently posts to.
      `seasonal-release` posts to 4130 (event-package);
      `wholesale-keg-order` → 4100; `direct-shop-order` → 4110
      (retail). Missing flows:
        - **taproom-shift-close**: aggregate per-shift pours
          and emit one invoice with `revenue_category =
          "taproom"`.
        - **distribution-contract**: recurring monthly invoice
          for distributor agreements; `revenue_category =
          "distribution"`.
      Without these the brewery's income statement is missing
      ~10% of plausible revenue.

- [ ] **External CRM integration is post-release.** The
      preliminary release ships with zero external CRM adapters.
      When a real tenant asks for one, design it as a port-shaped
      adapter crate under `crates/tenants/` — explicitly outside
      core.

- [ ] **Cloud-provider blueprints — opt-in recipes, no core
      dependence.** The release ships one supported install
      path (the Docker compose stack under
      `infra/oss-quickstart/`), plus a source-tree dev-mode
      script (`quickstart.sh`, whose engine is
      `infra/bootstrap-local.sh`) and the
      undocumented-with-known-flaws `infra/bootstrap-vm.sh`
      (no SPA deploy, hardcoded binary list — packets exist
      for both). Cloud-provider provisioning
      recipes (Azure Bicep, GCP, Cloudflare Tunnel + Origin
      Cert, AWS, Hetzner) come back post-release as
      `infra/blueprints/<provider>/` directories, each with a
      README walking the operator through "stand up a VM in
      your cloud + run quickstart.sh on it." No part of BOSS
      core depends on any of these — they're worked-example
      recipes, not load-bearing infrastructure. Operators
      discover them via `infra/blueprints/README.md` index;
      none referenced from CLAUDE.md / README / CLI defaults.

- [ ] **Production infrastructure template — single-VM today,
      multi-VM when warranted.** Current prod posture is one VM
      running the full stack with `pg_dump` (or
      `infra/backup.sh`) backups plus `audit_log` itself as the
      disaster-recovery primitive (any snapshot replays cleanly
      via `boss-rebuild-all`). The template gets fleshed out
      post-release: a polished "stand up a prod VM in <30 min"
      runbook, documented backup cadence + restore drill, and
      **the CI/CD pipeline that ships binaries to it**
      (`.github/workflows/deploy-prod.yml` cross-compiles every
      `boss-*-api` for `x86_64-unknown-linux-gnu`, scps to the
      prod VM, restarts via `infra/deploy-services.sh` —
      today's `release.yml` covers only the boss CLI). Multi-
      VM topologies (warm-standby, active/active, edge LB) are
      out of scope until a real tenant's SLAs force the
      conversation.

- [ ] **Integrated IAM — Authelia (or any OIDC IDP) via
      forward-auth.** The release ships file-backed credentials —
      fine for evaluation, not production (see [SECURITY.md](SECURITY.md)
      §Deployment trust model). The gateway already mints sessions
      from a `Remote-User`-shaped input, which is exactly the
      contract Authelia / Keycloak / Dex / Pomerium speak when
      fronting a proxied app. Post-release deliverable:
      `infra/authelia/` as the reference bundle (systemd unit +
      sample `configuration.yml` + role/group-to-role mapping) plus
      a runbook that walks "bring your own VM" → "DNS + TLS" →
      "Authelia up + first user" without any SaaS dependency.
      Adds: WebAuthn / TOTP / SSO / proper email-OTP password
      reset / account lockout / audit-log of identity events.
      Cloudflare Access stays available as opt-in edge hardening
      (DDoS / WAF / origin-IP hiding) on top of the origin-tier
      IAM; the gateway code path doesn't care which IDP terminates
      the auth. Includes the edge-strip hardening called out at the
      top of this section.

- [ ] **Workflow modeling UX improvements.** The release ships a
      functional `/system/workflows` editor (read-only catalog at
      `/system/workflows`, full author surface at `/system/workflows`) plus
      `/system/step-plugins` for custom step UX bundles. The
      modeling experience is correct but rough — it leans on
      operators understanding the StepGraph / TierSpec / step
      registry up front. Polish:
        - Visual DAG editor with drag-to-reorder steps and
          drag-to-connect blocked-by edges (today's editor is
          form-driven tier list).
        - Step-template gallery — drop a `scheduling` /
          `repair` / `quote` / `sign-off` step from a palette
          rather than typing the kind slug.
        - Live preview pane: open a Workflow in "what would a
          new Job of this kind look like" mode.
        - Validation hints surfaced inline (metadata schema
          required-at-done fields, blocker cycles, missing
          authority roles for sign-off steps).
        - Per-tenant Workflow diff view so a head of department
          can see what changed between published versions
          without git-blame archaeology.
      Compounds with Integrated IAM above — once dept heads
      sign in with their real identity, the modeling UX they
      reach is what the platform is for.
      - **used-device-shop `workflow-approver` parity.** Core defaults
        grant `step-signoff:workflow-approver` to `platform-admin` and the
        brewery grants it to its leaders; the used-device-shop tenant has
        no grant, so if it ever drives `workflow-design` Jobs its leaders
        can't approve. Add the grant when that tenant authors Workflows.

- [ ] **Information theory on audit_log — triage, anomaly
      detection, error handling.** The audit_log is a stream
      of typed events with rich structure that BOSS today
      treats as a flat list. Information theory gives a math
      vocabulary for operating on the stream that fits the
      project's cybernetics framing (Beer's algedonic signals
      are exactly the high-information attention-grabbing
      events). Concrete directions:
        - **Per-topic entropy / surprise rate.** High-
          frequency / low-surprise (`jobs.step.updated`) gets
          compressed in operator views; low-frequency / high-
          surprise (`commerce.invoice.past_due`,
          `jobs.job.cancelled`, `audit_log integrity check
          failed`) bubbles up.
        - **Distributional anomaly detection (KL divergence).**
          For sliding 5-min / 1-hr / 1-day windows, compare
          topic-frequency distribution against a historical
          baseline. Sudden KL spike = something off.
        - **Cross-topic mutual information for chain-integrity
          checks.** Some topic pairs are tightly coupled
          (`step.completed.billing` → `commerce.invoice.created`).
          MI drops if the chain breaks — would have caught the
          BatchEngine-empty + missing-tax-route bugs
          automatically.
        - **Information-bottleneck triage** for operators.
          Smallest event subset whose conditional entropy of
          current state is below a threshold = the dashboard's
          recommended-attention list.
        - **Error-budget framing** per error-topic, SRE-style.
      Probably a new analytics layer on top of audit_log
      queries; pairs with `boss-audit-integrity-check.timer`.

---

## Post-release — polish

Lower-priority automation + UX nice-to-haves. Pull forward only
if a contributor wants them.

- [ ] **CI workflow: run the sim-validation gate on tagged
      releases.** Goal: `git tag vX.Y.Z && git push --tags`
      runs `validate-brewery-sim.sh` (a sim-year + clean-rebuild
      assertion) automatically, so a release can't ship a sim that
      doesn't reconstruct. Blocker is that the script is heavily
      systemd-coupled today (stops/starts boss-* units, drops the
      live DB via sudo) — it doesn't run as-is in a GitHub Actions
      runner. Two real paths to land it:
        (a) Refactor the regen to drive services via Docker
            compose (the `infra/oss-quickstart/` stack) instead
            of systemctl, then wire the workflow to run inside
            that stack on the runner. Probably ~4-6 hours of
            work. Full 365-day regen would take ~90 min on
            GH-hosted runners.
        (b) Pin a self-hosted runner on a beefy VM that
            already has systemd + the boss tooling installed.
            The workflow becomes trivial (`git pull && infra/
            postgres/validate-brewery-sim.sh`). Pays a real
            ongoing maintenance cost for the VM.
      Pull forward when there's a tenant cutting frequent
      releases who'd benefit from the automation.

- [ ] **Landing-page state-machine visualizations — second
      pass.** The current SVGs on the landing page introduce
      Subject / Job / Step / Event reasonably but stay at the
      "boxes and arrows" level. Goal: animate the live event
      tail driving a Job through its Step DAG so a first-time
      visitor sees the model *operating* instead of just
      diagrammed. Tie the tick rate to the same 1s landing-
      page refresh and use real audit-log frames so the
      visualization is the literal projection, not a mock.

- [ ] **Finished-product `/shop` integration — Phase 2 tail.**
      Phase 1 fully shipped (5→11 SKUs across 5 beer styles,
      `direct-shop-order` Workflow, `shipment_line_items`
      projection, production + sale side-effects via
      `products.produce` / `products.consume` handlers, `/shop`
      SPA + sidebar module gate). One open sub-item:
        - **Per-customer email-OTP gate.** Today the shop POST
          opens the Job under the catch-all `acc-direct-shop`
          Account regardless of who's checking out, and
          `/shop` + `/shop/{sku}` require a session. Public-
          by-default shape: routes readable without a session
          (matches `/api/workflows*`); checkout opens an
          email-OTP or magic-link step before the Job is
          created. Blocks on the email-OTP design call — which
          also gates "should /shop create a thin per-customer
          Account on checkout."

- [ ] **A/R aging SPA view filters by status.** With the period
      write-off pipeline shipped, invoices that sit past-due > 60
      sim-days flip to `'written-off'` and the GL drops them off
      A/R. The SPA's A/R aging view at `/finance` (or wherever the
      AR aging table lives) currently groups all non-paid invoices
      together; once the bundle ships with written-off rows, the
      view should exclude them from "outstanding" and either hide
      them entirely or surface them in a separate "written off
      (uncollectable)" row so the operator sees the real
      receivables vs the historical uncollectables at a glance.
      Small SPA change (~1 hour). Pair with the existing past-due
      display so the same view shows: outstanding (current) /
      outstanding (past-due) / written-off (uncollectable).

## Coherence audit backlog (2026-07-01)

A read-only coherence audit (4 parallel agents: financial/ledger, frontend,
projections/rebuild, cross-boundary contracts) found the core sound
(double-entry enforced at 3 layers, WIP→FG→COGS reconciles, projections
mostly deterministic) but surfaced these edge / display / latent issues.
Grouped by priority; the display cluster is being tackled first.

### Display — user-visible wrong/wonky numbers (frontend)

- [ ] **Account-class rollup count isn't rendered in the cockpit.** The
      Rust sends a per-class `distinct` account count, but
      `apps/simulator/src/CockpitPage.svelte` has `DISTINCT_NOUN.account =
      ''`, so `distinctLabel('account', n)` returns `''` and "N accounts"
      never shows. Set `account` (and `vendor`) to a real noun. Completes the
      account-class rollup PR (its Rust half). *(→ folded into that PR.)*
- [ ] **"Paid YTD" is actually lifetime-paid.**
      `apps/web/src/accounts/AccountPage.svelte:177` sums all paid invoices
      with no year filter; overstates for any account with history. Filter
      `paid_on >= startOfYear`, or relabel.
- [ ] **~20 sites format money as `$${(cents/100).toLocaleString()}` and
      drop cents** ($42.50→"$42.5", $42.00→"$42"). PoPage, VendorPage,
      VendorInvoicePage, VendorsList, PartPage, AccountPage, RepairSurface,
      DevicePage, CatalogBrowser. Sweep to web-kit `formatMoney`/`formatUsd`.
- [ ] **Indirect cash-flow shows the subtotal but zero operating line
      items.** Rust moved the breakdown to a new `operating_activities`
      field (`boss-ledger/src/http/statements.rs:679,688`); the TS
      `CashFlowStatement` + `CashFlowTab` still render the now-always-empty
      `working_capital_adjustments`/`non_cash_adjustments`. Add + render
      `operating_activities`.
- [ ] **`written-off` invoices render a blank status chip.** TS
      `InvoiceStatus` union (`apps/web/src/finance/types.ts:5`) is missing
      `'written-off'` (a real terminal state) → `INVOICE_STATUS_LABEL[status]`
      is `undefined`; `InvoicesTab` also miscounts them as "unpaid". Add to
      union + label + chip variant. *(Overlaps the A/R-aging polish item above.)*

### Correctness — numbers that don't add up under the hood

- [ ] **`financial_facts` payloads diverge live-vs-rebuilt.** The publisher
      injects `_actor`/`_simulated` into the event payload
      (`boss-core/src/publisher.rs:123-140`) but the in-tx fact omits them
      (`boss-inventory/http/items.rs:239-252`, `boss-products/http.rs`), so
      `rebuild_facts` reconstructs a different payload. Masked because the
      shipped gate (`boss_ledger_replay_check.rs`) only diffs journal *lines*.
      Build the in-tx fact payload from the post-injection value (or strip the
      injected keys in `rebuild_facts`).
- [ ] **Message ages are nonsense on some messages.** `boss-messages/http.rs`
      stamps `sent_at` with sim-time on one path (:344) and `Utc::now()` on
      another (:394); the frontend ages against sim-time. Stamp sim-time on
      both send paths.
- [ ] **Income-tax net-income sums a dead `kind='cogs'` bucket** (always 0 —
      the chart has no `'cogs'` kind, COGS is `'expense'`).
      `boss-ledger/src/http/tax.rs:277-295`. Total is right by accident;
      switch to `code LIKE '5%'` and align with the income statement.
- [ ] **`gl_account_daily` panics on a missing account code.**
      `boss-ledger/src/postgres.rs:318,339` `account_ids[line.account_code]`
      indexes a HashMap (no-panic-in-library violation). Use a checked lookup
      → `LedgerError`. Also add `gl_account_daily` to a replay-check (live
      i128 vs rebuild SQL `trunc` parity is correct-but-unverified).
- [ ] **Shipping status roll-up is replay-order-dependent.**
      `boss-shipping/src/rebuild.rs:164-195` sets status from tracking scans in
      `id` order with no advance-only gate → live-vs-rebuilt `status`/`shipped_on`
      divergence on out-of-order delivery. Gate on `stage_index` monotonicity.
- [ ] **Inconsistent replay `ORDER BY` across rebuilders** (`id` vs
      `(timestamp, event_id)`). Under sim-time many events share a timestamp →
      tie-break on a random UUID. Order-invariant for pure upserts today;
      standardize on `ORDER BY id`.

### Latent / low

- [ ] **Cash-attribution split loses cents on 3+-line cash JEs.**
      `upsert_daily_rollup` (`boss-ledger/postgres.rs:344`) truncates the
      proportional split and never redistributes the remainder → surfaces as
      `reconciliation_gap_cents`. Give the largest-share offset the residual.
- [ ] **Cash-flow "indirect" branch is actually the direct method** (header
      comment inverted) and the **investing section is always empty** —
      equipment cash is attributed to 2100 A/P → classified operating.
      `boss-ledger/src/http/statements.rs:439+`.
- [ ] **`post_inventory_movement` hardcodes the FactRef kind**
      (`boss-ledger/src/http/facts.rs:465`) ignoring its `fact_kind` param.
      Latent (both kinds share a rule today).
- [ ] **New-invoice header total ≠ line-item sum on sub-cent inputs**
      (`apps/web/src/finance/NewInvoicePage.svelte:164` rounds the summed
      float; lines round independently) → trips the "line sum ≠ header"
      warning on a just-created invoice.
- [ ] **Zero-cost stock mutations double-apply on NAK-redelivery.** When
      `avg_cost == 0` no proof-fact is written, so the idempotency guard has
      nothing to check (`boss-products/postgres.rs:210`, `boss-inventory/
      postgres.rs:150`). Self-heals on rebuild. Write a zero-amount proof-fact.
- [ ] **Schedule-triggered dispatcher rules break the cascade graph.** TS
      `DispatcherRule.on_event` is non-optional; a scheduled rule omits it →
      `EVT(undefined)` node (`apps/web/src/dispatcher/cascadeToGraph.ts`).
      Latent — 0 scheduled rules today.
- [ ] **Messages `archived` rebuild overwrites `kind` + drops `archived_at`**
      (`boss-messages/src/rebuild.rs:179`). Projection-fidelity gap.
- [ ] **Commerce status-flip rebuild skips line items** if the `created` event
      is outside the audit slice (`boss-commerce/src/rebuild.rs:109`). Edge.
- [ ] **`AccountPage` `kFmt` always renders "$…K"** → $400 shows "$0K". Reuse
      the tiered `ExecPage` formatter.
- [ ] **Cockpit cadence label shows the per-pass batch cap as "per tick"**
      (`CockpitPage.svelte:209`) — overstates granularity at hourly ticks.
- [ ] **(unverified) `TaxFiling` TS unions narrower than the Rust `String`**
      (`liability_account`/`kind`) — mis-narrows if a new code is added
      server-side.
