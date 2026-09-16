-- 20260916040500-the-cloudflare-tunnel-credential-is-declared.sql —
-- the Cloudflare Tunnel credential and the root it is minted with,
-- declared as registry rows (202609031700: possession stays in
-- Secrets, knowledge lives here). Packet 04e5f833.
--
-- WHY. The tunnel's connector token on boss-gcp was copied into an
-- immutable ops-request by the unit-cat verb on 2026-09-16 (9c760dd7);
-- the rotation packet for it (2ff7a586) offered two HAND paths, and
-- David's standing rule is "avoid doing anything by hand unless
-- absolutely required". The credential broker already runs the
-- rotate-a-credential protocol for the forge token by machine; the
-- dispatcher rule `broker-rotates-the-cloudflare-tunnel` (handler
-- `credential.rotate.cloudflare-tunnel`) does the same for this one.
-- The ONLY human act left is the root ceremony: one account API token,
-- minted once into the broker's root Secret. Both rows below carry
-- LOCATIONS and consumers, never a value.
INSERT INTO credentials
    (id, kind, issuer, principal, scopes, storage_location, consumers,
     rotation_policy, rotated_at, notes)
VALUES
(
    'cloudflare-account-token',
    'cloudflare-api-token',
    'cloudflare (minted by David in the Cloudflare dashboard, account API token)',
    'the Cloudflare account that owns zone algedonic.dev',
    '["Cloudflare Tunnel:Edit", "Zone:DNS:Edit", "Zone:Read"]'::jsonb,
    'k8s Secret boss/boss-credential-broker-root key cloudflare-token',
    '[{"kind": "env",
       "location": "dispatcher env BOSS_BROKER_CLOUDFLARE_TOKEN in the boss pod (boss.yaml) — the credential.rotate.cloudflare-tunnel handler signs every Cloudflare API call with it"}]'::jsonb,
    'on-demand',
    NULL,
    'THE ROOT the broker mints tunnel credentials with — never a rotation '
    'product; the revoke step of a tunnel rotation must not touch it. Minted '
    'ONCE by David on 2026-09-16 (the root ceremony, the only hand act). Scopes '
    'as the ceremony declared them; Zone:Read is what lets the handler read '
    'the account id off the zone (GET /zones?name=algedonic.dev carries '
    'account.id) instead of needing Account Settings:Read. Rotation of this '
    'row is itself a ceremony: mint a new token, patch the Secret key, delete '
    'the old token in the dashboard.'
),
(
    'cloudflare-tunnel-credentials',
    'cloudflare-tunnel-credentials',
    'cloudflare account API (POST /accounts/{account}/cfd_tunnel, by the credential broker)',
    'one Cloudflare Tunnel (config_src local), named boss-cluster-<first 8 of its rotation packet id>',
    '["connect as this tunnel"]'::jsonb,
    'k8s Secret boss/cloudflare-tunnel-credentials key credentials.json '
    '(cloudflared connection.Credentials: {AccountTag, TunnelSecret, TunnelID})',
    '[{"kind": "secret-mount",
       "location": "the in-cluster cloudflared Deployment boss/cloudflared (5a2bb0ce), credentials-file mode — reads the file once at start, so the rotation rollout-restarts it"}]'::jsonb,
    'on-demand',
    NULL,
    'Rotated by the credential broker: dispatcher rule '
    'broker-rotates-the-cloudflare-tunnel fires handler '
    'credential.rotate.cloudflare-tunnel off the scope step of a '
    'rotate-a-credential packet (opened on this id, or whose scope step names '
    'it in `credential`). A rotation is a NEW tunnel: issue creates it, install '
    'writes its credentials.json and points the declared hostnames (rule args '
    'until 5e58922c) at <id>.cfargotunnel.com, verify waits for a connector and '
    'a 2xx through the edge, revoke deletes the old tunnel by name only once it '
    'shows zero live connections. The predecessor — the boss-gcp connector''s '
    'tunnel, token inline in cloudflared.service since 2026-06-09, leaked into '
    'ops-request 0117de08 — was never a registry row; its retirement is '
    '0b7804f3. rotated_at starts NULL: the first machine rotation stamps it.'
)
ON CONFLICT (id) DO NOTHING;
