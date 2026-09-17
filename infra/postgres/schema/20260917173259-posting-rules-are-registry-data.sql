-- 20260917173259-posting-rules-are-registry-data.sql — a tenant
-- declares fact_kind → journal lines as DATA, and the projection can
-- pick ONE workflow's step out of an event kind.
--
-- Origin: backlog a40541cb (decided on design 18cf4272). Measured
-- 2026-09-17: `BossRuleSet` in crates/modules/boss-ledger/src/rules.rs
-- is a Rust `match` on fact kind — a tenant that needs one more
-- posting rule (Algedonic's sponsorship receipt, the first revenue
-- line of design ffc83387) must fork core, against CLAUDE.md §9:
-- new posting rules land as data in append-only versioned registries,
-- not as new branches in core code. The event→fact half was already
-- data (`gl_fact_projection_rules`, 40-ledger.sql); this is the
-- fact→lines half, and the filter the event half was missing.
--
-- gl_posting_rules — APPEND-ONLY, VERSIONED BY FACT KIND. One row per
-- (fact_kind, version); the ledger evaluates a fact by the NEWEST row
-- for its kind and falls back to the code rules when there is none.
-- `lines` is a JSON array of {account_code, side (debit|credit),
-- amount_path (an RFC 6901 pointer into the fact payload; integer
-- cents), memo?}. A rule is admitted only when its debit pointers and
-- its credit pointers are the same multiset — balanced for EVERY
-- payload, not for the one someone tried — and the balanced-draft
-- check every code rule passes still runs at evaluation.
--
-- HOW A DATA RULE'S VERSION MEETS gl_rule_versions. `gl_rule_versions`
-- names the INTERPRETER (the RuleSet that ran; BOSS RuleSet v1 is the
-- one active row and stays so), and `gl_journal_entries.rule_version_id`
-- keeps pointing at it. A data rule's `version` is the tenant's own
-- edition of ONE fact kind's lines: publishing v2 of a kind changes
-- how facts in OPEN periods post on the next rebuild (the same
-- re-projection the code rules already get under
-- `OPEN_PERIOD_FACTS_SQL`) and never touches a locked period. It mints
-- no gl_rule_versions row. Which edition produced an entry is written
-- into the entry's memo (`<fact_kind> — posting rule v<n>` when the
-- rule gives no memo), so the record says so without a schema column.
--
-- `basis` is a property the tenant declares (cash | accrual) and the
-- registry records; it is not a code path.
--
-- `source` is NULL for a platform row, `tenant:<id>` for a row
-- `boss tenant publish` landed — the same provenance the sensors
-- registry keeps in `tenant_id`.

CREATE TABLE IF NOT EXISTS gl_posting_rules (
    fact_kind   TEXT NOT NULL,
    version     INT NOT NULL CHECK (version >= 1),
    lines       JSONB NOT NULL,
    basis       TEXT NOT NULL DEFAULT 'accrual' CHECK (basis IN ('cash', 'accrual')),
    source      TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (fact_kind, version)
);

-- gl_fact_projection_rules gains `when_filter`: a JSON object of
-- {"<pointer>": <expected value>} — every pointer must resolve to
-- exactly that value for the rule to fire; NULL fires on every event
-- of the kind (every row before this migration). Named `when_filter`
-- and not `when` because WHEN is reserved in SQL; the TOML and the
-- API call it `when`.
--
-- The primary key was `event_kind` alone: one rule per event kind.
-- With a filter, one event kind can carry several rules (one per
-- workflow's step), so identity becomes (event_kind, when_filter);
-- NULL is folded to '{}' so the "always" rule is one row too. The
-- seed INSERTs in 40-ledger.sql and 202609021400 say `ON CONFLICT
-- (event_kind)`, which needs the old key — and that is fine: on a
-- fresh database they run BEFORE this file, and on a database that
-- already applied them migrate.sh never runs them again (an applied
-- migration is history; editing them was refused by the gate's
-- migrations-append-only lint on the first attempt of this car, and
-- would have failed migrate.sh's checksum at boot).
ALTER TABLE gl_fact_projection_rules ADD COLUMN IF NOT EXISTS when_filter JSONB;
ALTER TABLE gl_fact_projection_rules DROP CONSTRAINT IF EXISTS gl_fact_projection_rules_pkey;
CREATE UNIQUE INDEX IF NOT EXISTS gl_fact_projection_rules_identity
    ON gl_fact_projection_rules (event_kind, (COALESCE(when_filter, '{}'::jsonb)));

-- Declared here so the kinds do not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).
-- One fact per INSERTED row, none for a kept row, none per batch —
-- the shape of 20260917071313.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('ledger.posting_rule.declared', 'ledger', 'A tenant''s batch inserted one posting rule (POST /api/ledger/posting-rules/batch, insert-if-absent by fact_kind + version): the row as inserted — fact_kind, version, lines, basis, source — plus declared_by, the actor the request signed with. One per inserted row, none for a kept row, none per batch', NULL),
  ('ledger.fact_projection_rule.declared', 'ledger', 'A tenant''s batch inserted one event→fact projection rule (POST /api/ledger/fact-projection-rules/batch, insert-if-absent by event_kind + when): the row as inserted plus declared_by, the actor the request signed with. One per inserted row, none for a kept row, none per batch', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
