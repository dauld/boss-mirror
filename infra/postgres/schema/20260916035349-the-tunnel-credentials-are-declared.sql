-- 20260916035349-the-tunnel-credentials-are-declared.sql — the
-- Cloudflare Tunnel connector's credentials, declared as a registry row
-- (202609031700: possession stays in the Secret, knowledge lives here).
--
-- Backlog 5a2bb0ce; design 4c565f8c, decided by David 2026-09-16: every
-- public hostname reaches the cluster through a Cloudflare Tunnel whose
-- connector runs IN the cluster (infra/cluster/manifests/cloudflared.yaml,
-- config-file mode, the ingress map rendered from the tree). The
-- connector it replaces ran on boss-gcp with its token inline in a
-- hand-written unit — a value that leaked into two ops-requests
-- (0117de08, 9c760dd7) and a credential declared nowhere. This row is
-- the declaration; nothing in the tree, in a packet or in a log ever
-- carries the value.
--
-- MINTED ONCE BY DAVID, not by any machine: `cloudflared tunnel create
-- <name>` writes a JSON credentials file (TunnelID, AccountTag,
-- TunnelSecret); it becomes the Secret with
--   kubectl -n boss create secret generic cloudflare-tunnel-credentials --from-file=credentials.json=<that file>
-- and the connector mounts it read-only. Until it exists the converge
-- records `cloudflared: skipped (secret absent: ...)` on its packet and
-- is not held. The scope is what tunnel credentials can do — run
-- connectors for that one tunnel — spelled as Cloudflare spells it;
-- there is no finer-grained scope string to verify.
--
-- ROTATION is on-demand through the rotate-a-credential protocol: this
-- row is its `credential` subject; issue and revoke are Cloudflare-side
-- (`cloudflared tunnel create` / `delete`, or the dashboard), install is
-- the Secret write (a rolling restart of deploy/cloudflared picks it
-- up), verify is the converge's `cloudflared: connected` field. A broker
-- handler for it is a later car.
INSERT INTO credentials
    (id, kind, issuer, principal, scopes, storage_location, consumers,
     rotation_policy, rotated_at, notes)
VALUES
(
    'cloudflare-tunnel-credentials',
    'cloudflare-tunnel-credentials',
    'Cloudflare (cloudflared tunnel create, run by David against the algedonic.dev zone)',
    'the tunnel named in infra/cluster/manifests/cloudflared-config.yaml',
    '["tunnel: run"]'::jsonb,
    'k8s Secret boss/cloudflare-tunnel-credentials key credentials.json',
    '[{"kind": "secret-mount",
       "location": "/etc/cloudflared-creds/credentials.json in the cloudflared pods (infra/cluster/manifests/cloudflared.yaml, namespace boss; config-file mode, credentials-file in cloudflared-config.yaml)"}]'::jsonb,
    'on-demand',
    NULL,
    'NOT YET MINTED as of 2026-09-16: the connector''s pods wait in '
    'ContainerCreating and every converge packet records cloudflared: '
    'skipped (secret absent: cloudflare-tunnel-credentials) until David '
    'runs the mint above. One tunnel serves every instance (prod and the '
    'playground); the boss-gcp connector on the same tunnel retires in a '
    'later car, so both serve traffic in between.'
)
ON CONFLICT (id) DO NOTHING;
