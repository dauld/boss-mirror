// The presence ceremony, browser half (docs/design/presence.md;
// BOSS-native call on packet 7218c3f1).
//
// A step that demands `presence` assurance refuses a plain sign-off
// with 422 {required: "presence"}. The caller then runs this ceremony:
// begin (the gateway mints a challenge bound to the step's CURRENT
// shape hash), navigator.credentials.get (the passkey signs exactly
// that binding), finish (the gateway verifies and issues a two-minute
// single-step ticket), and retries the sign-off with the ticket in
// `x-presence-ticket` — which the gateway swaps for the trusted
// header. No fallback path exists on purpose (Q3): if the actor has
// no passkey or declines the prompt, the step waits.

const b64urlToBytes = (s: string): Uint8Array => {
  const pad = s.length % 4 === 2 ? '==' : s.length % 4 === 3 ? '=' : '';
  const bin = atob(s.replace(/-/g, '+').replace(/_/g, '/') + pad);
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
};

const bytesToB64url = (buf: ArrayBuffer): string =>
  btoa(String.fromCharCode(...new Uint8Array(buf)))
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '');

import { CEREMONY_DECLINED } from '../me/passkeyHints';

export type PresenceRefusal = Readonly<{
  required?: string;
  detail?: string;
}>;

/** True when a sign-off response is the presence refusal this module answers. */
export async function needsPresence(resp: Response): Promise<boolean> {
  if (resp.status !== 422) return false;
  const body = (await resp
    .clone()
    .json()
    .catch(() => null)) as PresenceRefusal | null;
  return body?.required === 'presence';
}

/**
 * Run the full ceremony for one step. Resolves to the ticket value for
 * the `x-presence-ticket` header. Rejects with a human-readable Error
 * when the actor has no enrolled passkey, declines the prompt, or the
 * gateway refuses the assertion.
 */
export async function performPresenceCeremony(
  jobId: string,
  stepId: string,
): Promise<string> {
  const begin = await fetch('/api/auth/passkey/assert/begin', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ job_id: jobId, step_id: stepId }),
  });
  if (begin.status === 409) {
    throw new Error(
      'No passkey enrolled — add one under My Day → Passkeys first.',
    );
  }
  if (!begin.ok) {
    throw new Error(`presence ceremony unavailable (${begin.status})`);
  }
  const opts = (await begin.json()) as {
    challenge_id: string;
    publicKey: {
      challenge: string;
      rpId?: string;
      allowCredentials: { type: 'public-key'; id: string }[];
      userVerification: UserVerificationRequirement;
      timeout: number;
    };
  };
  let credential: PublicKeyCredential | null = null;
  try {
    credential = (await navigator.credentials.get({
      publicKey: {
        challenge: b64urlToBytes(opts.publicKey.challenge).buffer as ArrayBuffer,
        rpId: opts.publicKey.rpId,
        allowCredentials: opts.publicKey.allowCredentials.map((c) => ({
          type: c.type,
          id: b64urlToBytes(c.id).buffer as ArrayBuffer,
        })),
        userVerification: opts.publicKey.userVerification,
        timeout: opts.publicKey.timeout,
      },
    })) as PublicKeyCredential | null;
  } catch {
    throw new Error('Passkey prompt was declined or timed out.');
  }
  if (!credential) throw new Error('Passkey prompt returned no credential.');
  const assertion = credential.response as AuthenticatorAssertionResponse;
  const finish = await fetch('/api/auth/passkey/assert/finish', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      challenge_id: opts.challenge_id,
      credential: {
        id: credential.id,
        rawId: bytesToB64url(credential.rawId),
        type: credential.type,
        response: {
          authenticatorData: bytesToB64url(assertion.authenticatorData),
          clientDataJSON: bytesToB64url(assertion.clientDataJSON),
          signature: bytesToB64url(assertion.signature),
          userHandle: assertion.userHandle
            ? bytesToB64url(assertion.userHandle)
            : null,
        },
      },
    }),
  });
  if (!finish.ok) {
    throw new Error(`assertion rejected (${finish.status})`);
  }
  const { ticket } = (await finish.json()) as { ticket: string };
  return ticket;
}

/** Enrolment: register a new passkey for the signed-in employee. */
export async function enrollPasskey(label: string): Promise<void> {
  const begin = await fetch('/api/auth/passkey/register/begin', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({}),
  });
  if (!begin.ok) throw new Error(`enrolment unavailable (${begin.status})`);
  const { challenge_id, options } = (await begin.json()) as {
    challenge_id: string;
    options: { publicKey: Record<string, unknown> };
  };
  const pk = options.publicKey as {
    challenge: string;
    rp: PublicKeyCredentialRpEntity;
    user: { id: string; name: string; displayName: string };
    pubKeyCredParams: PublicKeyCredentialParameters[];
    timeout?: number;
    excludeCredentials?: { type: 'public-key'; id: string }[];
    authenticatorSelection?: AuthenticatorSelectionCriteria;
    attestation?: AttestationConveyancePreference;
  };
  let credential: PublicKeyCredential | null = null;
  try {
    credential = (await navigator.credentials.create({
      publicKey: {
        challenge: b64urlToBytes(pk.challenge).buffer as ArrayBuffer,
        rp: pk.rp,
        user: {
          id: b64urlToBytes(pk.user.id).buffer as ArrayBuffer,
          name: pk.user.name,
          displayName: pk.user.displayName,
        },
        pubKeyCredParams: pk.pubKeyCredParams,
        timeout: pk.timeout,
        excludeCredentials: (pk.excludeCredentials ?? []).map((c) => ({
          type: c.type,
          id: b64urlToBytes(c.id).buffer as ArrayBuffer,
        })),
        authenticatorSelection: pk.authenticatorSelection,
        attestation: pk.attestation,
      },
    })) as PublicKeyCredential | null;
  } catch {
    throw new Error(CEREMONY_DECLINED);
  }
  if (!credential) throw new Error('Passkey creation returned no credential.');
  const attestation = credential.response as AuthenticatorAttestationResponse;
  const finish = await fetch('/api/auth/passkey/register/finish', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      challenge_id,
      label,
      credential: {
        id: credential.id,
        rawId: bytesToB64url(credential.rawId),
        type: credential.type,
        response: {
          attestationObject: bytesToB64url(attestation.attestationObject),
          clientDataJSON: bytesToB64url(attestation.clientDataJSON),
        },
      },
    }),
  });
  if (!finish.ok) {
    const text = await finish.text().catch(() => '');
    throw new Error(`enrolment rejected (${finish.status}): ${text}`);
  }
}
