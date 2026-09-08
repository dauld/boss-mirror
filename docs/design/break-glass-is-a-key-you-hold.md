# Break-glass is a key you hold

**Status**: in-review — Q1–Q6 ride the design packet "Break-glass is
a key you hold" (`e9703d8f`, filed 2026-09-03) with proposals
attached; the review step is assigned.

Break-glass authority moves from a secret on a disk to a hardware
security key in the operator's pocket. The emergency path into BOSS
stops being *something the system stores* and becomes *something only
a person can physically do* — a touch on an attested, device-bound
passkey — verified by whichever layer is still standing when the
emergency is real. (David, 2026-09-03: "I really want the break glass
protocol to be based on putting a passkey onto a security key.")

## Where break-glass authority lives today

Identity on this network is already passkey-only: passwords are not
an accepted factor, and the normal login is a passkey ceremony at the
IdP ([idm-kanidm.md](idm-kanidm.md)). The one deliberate exception is
break-glass — local auth via `credentials.toml`, reachable with curl
at `POST /api/auth/login`, deliberately a non-button so nobody drifts
back to it. It exists so an IdP outage cannot lock operators out, and
that reasoning is sound and stays.

What is wrong with it is its *substance*, not its existence:

- It is the last secret at rest in the auth story. idm-kanidm.md
  itself names the blast radius of losing that credential class.
- It lives on the `boss-auth` PVC — an RWO volume that is one of the
  two named blockers keeping the deployment on `strategy: Recreate`
  (the manifest's own comment), which priced every converge as an
  outage window on 2026-09-02.
- Possession of a string is the whole ceremony. Anything that can
  read a file can be the operator.

## What 2026-09-02 taught about verifiers

The boot-brick outages produced a working inventory of the emergency
levers and — the part that matters here — *which verifiers survive
which outages*:

| lever | verifier | survives |
|---|---|---|
| rollout undo | kube-apiserver | BOSS stack down, IdP down |
| emergency merge to forge main | Forgejo | BOSS stack down, IdP down |
| break-glass web session | boss-gateway | IdP down (not stack down) |

The design principle that falls out: **each lever gets
hardware-key-gated at its own verifier**, not at a central authority
that may be part of the outage. A break-glass that routes through the
thing that is down is a fiction, which is the same reasoning
idm-kanidm.md used to keep local auth local.

## The design

### The gateway becomes the break-glass verifier

boss-gateway grows a minimal WebAuthn relying party (the `webauthn-rs`
crate — the same author and idiom as Kanidm, so both halves of the
auth story stay in one dialect) with exactly one registered
credential: a **device-bound passkey on a hardware security key**,
enrolled by David. `POST /api/auth/login` with a password body is
replaced by a challenge/assertion ceremony.

The stored material is the credential's public key, sign counter, and
AAGUID. Nothing secret rests anywhere:

- A leaked break-glass store is a leaked *public key* — harmless by
  construction, retiring the credential-class blast radius
  idm-kanidm.md worries about.
- Public material needs no RWO volume. The credential record moves to
  a ConfigMap (in-tree manifest; the repo's public mirror can carry a
  public key without ceremony), and the `boss-auth` PVC retires —
  which removes RollingUpdate blocker #1. The agreed sequence
  (David, 2026-09-03) is: converge auto-rollback first (landed as
  `feat/converge-rolls-back-a-brick`), then this design, then
  RollingUpdate.

Device-bound is enforced, not assumed: registration requires
attestation, so a synced software passkey (iCloud/Google password
manager) cannot enroll. "A passkey on a security key" means the
private key is born on the hardware and cannot leave it; the
attestation statement is what proves that at enrollment time.

### Below the gateway: the same key, other applets

When the gateway itself is down — the 2026-09-02 case — WebAuthn has
no relying party to talk to. The levers below it are gated by the
same physical key through verifiers that were still standing that
night:

- **Rollout undo.** The break-glass kubeconfig's client certificate
  keeps its private key on the security key's PIV applet:
  non-exportable, PIN + touch per use, verified by kube-apiserver.
  A stolen kubeconfig file is inert without the hardware.
- **Emergency merge.** Forgejo already supports passkey login for
  the merge click. The *approval artifact* — the thing the
  post-mortem's break-glass protocol requires David to produce — is
  an SSH signature from an `sk-ed25519` key (FIDO2-backed, resident
  on the same hardware) over the gate-receipt sha: a
  hardware-touch-proven, permanently verifiable record that the
  operator authorized this exact tree state. The audit log gets a
  cryptographic fact instead of a chat transcript.

One key, three applets (FIDO2, PIV, sk-SSH), three verifiers, zero
shared secrets.

### What deliberately does not change

- The IdP remains the only normal door. Break-glass stays
  understated — reachable, documented, not a button.
- The break-glass session's *authority* is unchanged by this doc
  (see Q4 for whether it should be).
- The refusal posture: a break-glass attempt that fails verification
  fails loudly, like every other refusal in the system.

## Costs, said out loud

- The curl-able break-glass becomes a browser ceremony; WebAuthn
  does not fit a bare terminal. Q3 decides whether a terminal
  fallback (sk-SSH signature exchanged for a session ticket) is
  worth its surface.
- Break-glass gated on one losable physical object is a lockout
  waiting to happen. Two keys minimum — primary carried, backup in a
  safe — both enrolled (Q2). Enrollment and hardware are David's
  domain per the token-admin rule; the agent builds the ceremony and
  verifies by effect.
- `webauthn-rs` is a real new dependency in the gateway. It is the
  cost of not hand-rolling signature verification, which is not a
  place to be original.

## Open questions

### Q1: Attestation policy — allowlist or any-hardware?
Enforcing attestation keeps software passkeys out. Should enrollment
further pin an AAGUID allowlist (only the exact key models David
owns), or accept any attested roaming authenticator? Allowlist is
tighter; any-hardware survives buying a different brand of backup
key without a code change.

### Q2: Backup-key ceremony
Enroll both keys at setup (two credential records, either
sufficient), or enroll-on-loss (backup key is enrolled only when the
primary is declared lost)? Both-upfront means the safe key works the
moment it is needed; enroll-on-loss means a stolen safe key is inert
but recovery depends on a working enrollment path during an
incident.

### Q3: Terminal fallback
Keep a browserless break-glass (an `ssh-keygen -Y` signature over a
gateway-issued nonce, exchanged for a session ticket), or accept
browser-only and lean on the PIV/SSH levers for terminal cases? The
fallback doubles the ceremony surface; its absence bets that a
browser is always reachable when the gateway is.

### Q4: Break-glass session scope
`credentials.toml` today issues platform-admin-equivalent claims.
Should the passkey session carry the same authority, or a narrower
break-glass role (deploy rollback, merge approval, auth
administration — and nothing else)? Narrower is safer; same-scope is
simpler and matches what break-glass has always meant here.

### Q5: Where the credential record lives
ConfigMap in `infra/cluster/manifests/` (in-tree, public mirror
carries a public key — cryptographically fine) versus a Secret
(out-of-tree, consistent with how every other sensitive object is
referenced by name). The material is public; the question is whether
*policy* wants all auth-adjacent objects handled one way regardless
of secrecy.

### Q6: PVC retirement sequencing
Retire `boss-auth` in the same car that lands the WebAuthn RP, or
one train later after the new ceremony is proven in prod? Same-car
is one clean cut; a soak respects that this is the only emergency
door while it is being replaced.
