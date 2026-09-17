-- 20260917041500-the-stripe-read-credential-is-declared.sql — the
-- restricted read-only Stripe key the first sensor polls with, declared
-- as a registry row (202609031700: possession stays in Secrets,
-- knowledge lives here). Design 14c9b2ad, backlog 2d33e111.
--
-- WHY. Decided with David 2026-09-17: "Polling is fine, use a
-- restricted read-only key." The `sensor.poll` handler reads Stripe's
-- charges on a cadence and opens a receive-a-sponsorship packet per
-- new succeeded charge; the ONLY credential it needs is a key that can
-- read, minted by David (a root ceremony, the one hand act) into the
-- broker's root Secret. The row carries the LOCATION and the consumer,
-- never a value — the handler reads the value from env only to send
-- it as a bearer, and no agent or log ever prints it.
INSERT INTO credentials
    (id, kind, issuer, principal, scopes, storage_location, consumers,
     rotation_policy, rotated_at, notes)
VALUES
(
    'stripe-restricted-read',
    'stripe-restricted-key',
    'stripe (minted by David in the Stripe dashboard, Developers > API keys > restricted key)',
    'the Stripe account that receives sponsorships for the company',
    '["charges: read", "checkout_sessions: read", "customers: read"]'::jsonb,
    'k8s Secret boss/boss-credential-broker-root key stripe-restricted-read',
    '[{"kind": "env",
       "location": "dispatcher env BOSS_BROKER_STRIPE_KEY in the boss pod (boss.yaml) — the sensor.poll handler sends it as the bearer on every GET /v1/charges for a sensor declaring credential = stripe-restricted-read"}]'::jsonb,
    'on-demand',
    NULL,
    'READ-ONLY by construction: a restricted key with the three read '
    'scopes above and nothing else, so the worst a leak can do is read '
    'charge history. Minted ONCE by David on 2026-09-17 (the root ceremony); '
    'installed with kubectl -n boss patch secret boss-credential-broker-root '
    '--type merge -p ''{"stringData":{"stripe-restricted-read":"<key>"}}''. '
    'Rotation is itself a ceremony: mint a new restricted key, patch the '
    'Secret key, roll the boss pod, delete the old key in the dashboard. '
    'Until the Secret key exists the dispatcher boots without it (optional '
    'secretKeyRef) and the poll files the sensor_unreadable:<sensor id> '
    'alarm naming BOSS_BROKER_STRIPE_KEY instead of failing silently.'
)
ON CONFLICT (id) DO NOTHING;
