-- 202609031700-credentials-are-registry-rows.sql — knowledge about
-- credentials becomes registry data (packet 7ee101aa, second leg;
-- David 2026-09-03: "token management is pure operational
-- book-keeping... I created that admin token so the system could
-- manage tokens and I could stop thinking about it").
--
-- WHAT WAS WRONG. Possession of a credential and KNOWLEDGE about it
-- were fused: the value sat in a k8s Secret (correct) and everything
-- else — kind, scopes, storage, consumers, rotation posture — lived
-- in manifest comments, rule args, and David's head. On 2026-09-02 an
-- admin token went half-used for days because its scope lived
-- nowhere readable, and a 403 was how an agent learned /api/v1/user
-- was out of scope. A scope question should be a LOOKUP, not an
-- experiment.
--
-- THE SPLIT THIS TABLE ENFORCES. Possession stays in Secrets and
-- token files, exactly where it is today. Knowledge lives here: one
-- row per credential, carrying every fact an operator or agent needs
-- EXCEPT the value. `storage_location` says where the value lives —
-- it never says what the value is. The HTTP surface over this table
-- (GET /api/credentials) inherits that rule: locations, never
-- contents.
--
-- IDENTITY, NOT VERSIONS. Unlike `workflows` or `delivery_policy`,
-- a credential's identity SURVIVES rotation — boss-dev-forge-token
-- is the same credential before and after the broker re-mints it
-- (the forge-side instances carry packet-derived names,
-- `boss-dev-forge-token-<first 8 of packet id>`). So the key is a
-- plain id, not (name, version).
--
-- MUTABILITY DECISION (the append-only-ish posture, written down):
--   - `rotated_at` and `notes` are UPDATEABLE — they are the
--     operational book-keeping this registry exists to hold, and the
--     rotation path and the audit both write them.
--   - kind / issuer / principal / scopes / storage_location /
--     consumers / rotation_policy describe WHAT the credential is.
--     Correcting an unverified fact (an empty `scopes` the audit
--     filled) is an update; CHANGING the grant is a new credential —
--     retire the old row by note and insert a new id, the same
--     instinct every other registry has. The audit-vs-forge
--     comparison is the drift alarm if a row and reality part ways.
--   - Rows are never deleted while the credential exists anywhere.
--     A revoked credential keeps its row (notes say revoked) until
--     the forge-side audit confirms nothing matches it.
--
-- WHAT IS DELIBERATELY NOT SEEDED. The estate/ops runner actor
-- headers (`x-boss-user`) are identity CLAIMS, not credentials — no
-- secret is possessed, nothing can be rotated — so they get no rows;
-- the credential-shaped part of that surface is the machine token,
-- which is row 3. `infra/platform/forge-tokens.toml` remains the
-- forge-host inventory declaration (nine live tokens, most with
-- consumer=UNKNOWN — the debt it exists to name); the weekly
-- forge-token-audit now compares the forge against BOTH that file
-- and these rows, so the two declarations cannot silently disagree:
-- the audit run is the equality test (CLAUDE.md 9a).

CREATE TABLE IF NOT EXISTS credentials (
    -- The durable identity, e.g. 'boss-dev-forge-token'. Forge-side
    -- token instances derive their names from it.
    id               TEXT PRIMARY KEY,
    -- 'forgejo-access-token', 'k8s-serviceaccount', 'machine-token',
    -- 'kubeconfig', ... Open TEXT, not an enum: a new credential kind
    -- is a row, never a migration (CLAUDE.md 9).
    kind             TEXT NOT NULL,
    -- Who mints it: a forge, a cluster control plane, David.
    issuer           TEXT NOT NULL,
    -- Whose authority it carries: a user, a ServiceAccount.
    principal        TEXT NOT NULL,
    -- JSON array of scope strings AS THE ISSUER SPELLS THEM. Empty
    -- means scope-unverified — an honest gap the audit fills, never a
    -- guess. For k8s credentials the entries name the RBAC roles.
    scopes           JSONB NOT NULL DEFAULT '[]'::jsonb
                     CHECK (jsonb_typeof(scopes) = 'array'),
    -- WHERE the value lives (Secret ns/name/key, file path). Never
    -- the value.
    storage_location TEXT NOT NULL,
    -- JSON array of {kind, location}: every place that reads the
    -- value. The 2026-08-27 lesson: a credential nobody can
    -- attribute to a consumer is one nobody dares revoke.
    consumers        JSONB NOT NULL DEFAULT '[]'::jsonb
                     CHECK (jsonb_typeof(consumers) = 'array'),
    rotation_policy  TEXT NOT NULL
                     CHECK (rotation_policy IN ('on-demand', 'scheduled')),
    -- When the value last changed. NULL = no rotation recorded since
    -- this registry existed (rotations before it live only on their
    -- packets).
    rotated_at       TIMESTAMPTZ,
    notes            TEXT NOT NULL DEFAULT '',
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Seed: the credentials tonight surfaced, with every UNVERIFIED fact
-- marked as such rather than invented.
INSERT INTO credentials
    (id, kind, issuer, principal, scopes, storage_location, consumers,
     rotation_policy, rotated_at, notes)
VALUES
(
    'boss-credential-broker-root',
    'forgejo-access-token',
    'forgejo (10.20.0.15)',
    'user david (admin)',
    '[]'::jsonb,
    'k8s Secret boss/boss-credential-broker-root key forgejo-token',
    '[{"kind": "env",
       "location": "dispatcher env BOSS_BROKER_FORGEJO_TOKEN in the boss pod (boss.yaml)"}]'::jsonb,
    'on-demand',
    NULL,
    'scope unverified — audit fills this. Declared only as "admin scope" in '
    'boss-credential-broker.yaml; minted once by David in a passkey-authorized '
    'ceremony on 2026-09-02. Verified by effect, not by scope string: it mints '
    'tokens via the Forgejo admin API, and on 2026-09-03 GET /api/v1/user '
    'answered 403, so read:user is absent. Its forge-side token NAME is also '
    'unrecorded — until it is filled in, the forge audit reports this row as '
    'matching no live token, which is correct and loud.'
),
(
    'boss-dev-forge-token',
    'forgejo-access-token',
    'forgejo (10.20.0.15)',
    'user david',
    '["write:repository"]'::jsonb,
    'k8s Secret boss-dev/boss-dev-forge-token key token',
    '[{"kind": "secret-mount",
       "location": "/etc/boss-train/forge.token in the boss-dev pod (boss-dev.yaml; the boss train verbs)"},
      {"kind": "git-credential-helper",
       "location": "global git credential helper in the boss-dev pod — reads the token file after boss credential pull forge"}]'::jsonb,
    'on-demand',
    NULL,
    'Rotated by the credential broker: dispatcher rule '
    'broker-rotates-the-boss-dev-forge-token fires handler '
    'credential.rotate.forgejo off the scope step of a rotate-a-credential '
    'packet. Forge-side instances carry packet-derived names '
    '(boss-dev-forge-token-<first 8 of packet id>); this id is the durable '
    'identity the audit prefix-matches. rotated_at starts NULL: rotations '
    'that predate this registry are recorded only on their packets.'
),
(
    'boss-machine-token',
    'machine-token',
    'BOSS (static shared secret, administered by David)',
    'platform service writers (shared)',
    '[]'::jsonb,
    'k8s Secret boss/boss-secrets key machine-token (every mount optional: true) '
    '+ env BOSS_MACHINE_TOKEN where a unit sets it',
    '[{"kind": "env",
       "location": "maintenance CronJobs in the boss namespace via secretKeyRef boss-secrets/machine-token: boss-audit-integrity, boss-backup, boss-files-gc, boss-ledger-recognize, boss-ledger-replay-check, boss-messages-events-purge, boss-search-reindex, boss-views-catchup"},
      {"kind": "env",
       "location": "infra/boss-step.sh, infra/boss-maintenance-wrap.sh and infra/ops/ops-runner.sh forward BOSS_MACHINE_TOKEN as x-boss-machine-token when set"},
      {"kind": "env",
       "location": "every service writer via boss_core::machine_token::attach (reads BOSS_MACHINE_TOKEN at process start)"}]'::jsonb,
    'on-demand',
    NULL,
    'DORMANT, and the boot logs declare it: boss-jobs-api warns "no '
    'BOSS_MACHINE_TOKEN configured" until the env var is set, and both '
    'enforcement and attachment are inert until then. Activation runbook: '
    'docs/runbooks/machine-token-activation.md. Whether Secret '
    'boss/boss-secrets currently holds the key is unverified — every '
    'consumer mounts it optional — the estate is the verifier for that.'
),
(
    'dev-session-token',
    'k8s-serviceaccount',
    'kubernetes (the cluster control plane mints the ServiceAccount token into the Secret)',
    'ServiceAccount boss-dev/dev-session',
    '["Role boss-dev/dev-session: pods read + exec, pvc + deployments read, gate Jobs create/get/list/watch, configmaps read, get Secret boss-dev-forge-token (by name, no list)",
      "Role boss/dev-session-ops-read: workloads read; rollout-undo patch on deployment boss only"]'::jsonb,
    'k8s Secret boss-dev/dev-session-token (kubernetes.io/service-account-token, non-expiring) '
    '+ kubeconfig on the boss-dev workspace PVC',
    '[{"kind": "kubeconfig",
       "location": "~/.kube/config on the boss-dev workspace PVC — kubectl, boss gate (creates and watches gate runner Jobs), boss credential pull forge"}]'::jsonb,
    'on-demand',
    NULL,
    'The gate credential (0b1b32f9, greenlit 2026-08-31). Non-expiring by '
    'design (boss-dev-access.yaml): the durable session must survive without '
    'a human on a timer, and the grant is small enough to accept that trade. '
    'Rotation: kubectl delete secret dev-session-token -n boss-dev, then '
    're-apply the manifest. Not a forge credential — the forge audit skips '
    'this row; scope entries name the two RBAC Roles so "can this credential '
    'read Secrets" is a lookup (answer: one, by name), not an experiment.'
)
ON CONFLICT (id) DO NOTHING;
