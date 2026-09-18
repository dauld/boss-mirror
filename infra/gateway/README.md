# boss-gateway unit + drop-ins

The gateway's systemd unit and its drop-ins, once installed on a
bare-metal host by the deploy script that left with that path on
2026-09-18 (the container's gateway takes the same settings as
environment in `infra/cluster/manifests/boss.yaml` and the compose
file). They live here because on 2026-08-07 they did
not: the only file in this directory was `demo-mode.conf`, describing
a mode removed that same day, while the two drop-ins that were live
and load-bearing — `guest-access.conf` and `local-auth.conf` — existed
only on the box.

That is worse than unversioned. A rebuild from this repo would have
deployed dead demo-mode config and lost both authentication and guest
access, and the directory would have looked authoritative while doing
it.

## Secrets do not go here

Every value in these files is a path, a flag or a public URL. A
credential — `BOSS_MAIL_API_TOKEN`, when the auth-mail relay lands —
belongs in an `EnvironmentFile=` at `0600` owned by root, the same
shape `BOSS_AUTH_FILE` already uses. A drop-in is world-readable.
