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
// no passkey or declines the prompt, the step waits. The begin names
// the step as this page rendered it, and the gateway refuses one that
// has changed since (fd7090cc). That guards an honest page from a swap
// between its render and the key press; it is not proof of what was on
// screen, because the page itself supplies what it names.

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

import { assertionFailure, enrolmentFailure } from '../me/passkeyHints';
import { putStep, type StepWriteResult } from './stepWrite';

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
 * The step as the approver was SHOWN it: the title and metadata the
 * surface rendered, with any write of the surface's own folded in
 * ({@link shownAfter}). The gateway hashes it with the one shape-hash
 * definition and refuses (412) a begin whose shown step is not the step
 * as it stands, so an honest page never has a swap made after its render
 * signed (backlog fd7090cc, security re-review of 2026-09-25). The page
 * supplies this value, so it binds nothing a dishonest page displayed.
 */
export type ShownStep = Readonly<{
  title: string;
  metadata: Readonly<Record<string, unknown>>;
}>;

/**
 * `rendered` with `patch` applied the way the step metadata door applies
 * it: a key sent is set, a key sent as null is deleted — and undefined
 * travels as null through `saveStep`, so it deletes too. A new object;
 * `rendered` is untouched.
 */
export function shownAfter(
  rendered: Readonly<Record<string, unknown>>,
  patch: Readonly<Record<string, unknown>>,
): Record<string, unknown> {
  const out: Record<string, unknown> = { ...rendered };
  for (const [k, v] of Object.entries(patch)) {
    if (v === null || v === undefined) delete out[k];
    else out[k] = v;
  }
  return out;
}

/**
 * One key the passkey signs, as a surface puts it on screen: `key` is the
 * key itself (the row's identity), `label` the key as drawn, `text` the
 * value as drawn — both through {@link signedText}.
 */
export type SignedRow = Readonly<{ key: string; label: string; text: string }>;

// The only characters drawn as themselves: printable ASCII, and the em
// dash the platform's own prose is full of (364 of them in infra/ops
// alone), which has no ASCII look-alike. Everything else — controls, bidi
// overrides and isolates, zero-width and other invisible characters,
// non-ASCII spaces, and every letter, digit or mark from another script
// that could pass for an ASCII one — is written as an escape. A letter
// table of "confusables" would be a claim to keep up to date; this is a
// rule with no table (backlog 6093cf13).
const AS_ITSELF = /^[\x20-\x7E\u2014]$/u;
const NOT_AS_ITSELF = /[^\x20-\x7E\n\u2014]/gu;

const escaped = (c: string): string =>
  `\\u{${(c.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, '0')}}`;

const QUOTED: Readonly<Record<string, string>> = {
  '\\': '\\\\',
  '"': '\\"',
  // A line break is drawn as \n AND breaks, so a multi-line plan stays
  // readable and a line the box wraps is never mistaken for one the
  // bytes break.
  '\n': '\\n\n',
  '\t': '\\t',
  '\r': '\\r',
};

/** A string in double quotes, every character not drawn as itself escaped. */
const quoted = (s: string): string =>
  `"${[...s].map((c) => QUOTED[c] ?? (AS_ITSELF.test(c) ? c : escaped(c))).join('')}"`;

/**
 * Whether a string can be drawn bare and still read as exactly itself:
 * one line, non-empty, every character drawn as itself, no space at an
 * edge, not starting with a quote (a quoted drawing always does), and
 * not something a reader would take for a number, a boolean, null or
 * JSON — '42' and 42 drew alike until 6093cf13.
 */
function readsAsItself(s: string): boolean {
  if (s === '' || s.startsWith('"') || s.startsWith(' ') || s.endsWith(' ')) return false;
  if (![...s].every((c) => AS_ITSELF.test(c))) return false;
  try {
    JSON.parse(s);
    return false;
  } catch {
    return true;
  }
}

/**
 * A signed value AS THE BYTES IT IS (backlog 6093cf13, adversarial review
 * of car 30674304). A string that reads as exactly itself is drawn bare;
 * any other string is drawn in double quotes with every character that is
 * not printable ASCII (or an em dash) written as `\u{XXXX}`, and `\n`,
 * `\t`, `\r`, `\\`, `\"` as themselves. Anything else is its indented
 * JSON, with the same escapes inside it — so a value the surface has no
 * renderer for is still shown, never skipped, and two values the passkey
 * would sign differently are never drawn alike. Key names and the title
 * are drawn through this too. It changes what is DRAWN, never what is
 * signed: the begin still names the raw step.
 */
export function signedText(v: unknown): string {
  if (typeof v === 'string') return readsAsItself(v) ? v : quoted(v);
  const json = JSON.stringify(v, null, 2);
  return json === undefined ? String(v) : json.replace(NOT_AS_ITSELF, escaped);
}

/**
 * What a value whose box scrolls says under it: rendered is not read, so
 * a long plan that scrolls inside an 18em box names how much of it there
 * is (6093cf13). `text` is the value as drawn.
 */
export function scrollNote(text: string): string {
  const lines = text.split('\n').length;
  return `scrolls in its box: ${lines} ${lines === 1 ? 'line' : 'lines'}, ${[...text].length} characters. Read it to the end; your passkey signs all of it.`;
}

/**
 * EVERY KEY THE PASSKEY SIGNS, AS ROWS TO RENDER (design f623e425 D3;
 * backlog 6c9183de extends b and c, 2026-09-25). The gateway binds the
 * challenge to `step_shape_hash(title, metadata)` of the step as shown
 * (crates/core/boss-core/src/job.rs), and that hash covers EVERY
 * metadata key — so the rows are every key of `shown.metadata`, sorted
 * the way the hash sorts them, with no allow-list and no filter.
 * ApprovalSurface rendered only `decision` and `comment` while its
 * passkey signed `plan`, `verb`, `host`, `args` and
 * `rendered_plan_sha256` too; a planted plan was approved on one tap,
 * unseen. A surface renders these rows from the SAME object it hands the
 * ceremony, so the two key lists cannot drift; {@link notShown} is the
 * check that refuses if they ever do.
 */
export function signedRows(shown: ShownStep): SignedRow[] {
  return Object.keys(shown.metadata)
    .sort()
    .map((key) => ({ key, label: signedText(key), text: signedText(shown.metadata[key]) }));
}

/**
 * A value's JSON with object keys sorted, so equality is order-free.
 * Exported for the pin that holds sign-off.js's copy equal to it.
 */
export function canonical(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map(canonical).join(',')}]`;
  if (v !== null && typeof v === 'object') {
    const o = v as Record<string, unknown>;
    return `{${Object.keys(o)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${canonical(o[k])}`)
      .join(',')}}`;
  }
  return JSON.stringify(v) ?? 'null';
}

/**
 * What a passkey would sign in `shown` that is NOT on screen as it would
 * be signed: `title` when the title on screen is another, and each
 * metadata key the screen lacks or shows with another value, sorted. A
 * key on screen and not signed is not a finding — only an unseen signed
 * one is. `onScreen` null means the surface shows no signed content, so
 * everything is unseen. Empty means the passkey may sign.
 */
export function notShown(shown: ShownStep, onScreen: ShownStep | null): string[] {
  const keys = Object.keys(shown.metadata).sort();
  if (onScreen === null) return ['title', ...keys];
  const title = shown.title === onScreen.title ? [] : ['title'];
  const moved = keys.filter(
    (k) =>
      !Object.prototype.hasOwnProperty.call(onScreen.metadata, k) ||
      canonical(onScreen.metadata[k]) !== canonical(shown.metadata[k]),
  );
  return [...title, ...moved];
}

/** The ceremony refused because the passkey would sign unseen content. */
export class NotShownRefusal extends Error {
  readonly keys: readonly string[];
  constructor(keys: readonly string[]) {
    super(
      `Nothing was signed: your passkey would sign ${keys.join(', ')}, which this page does not show as it stands. Read the step as shown and press again.`,
    );
    this.name = 'NotShownRefusal';
    this.keys = [...keys];
  }
}

/**
 * Run the full ceremony for one step. Resolves to the ticket value for
 * the `x-presence-ticket` header. Rejects with a human-readable Error
 * when the actor has no enrolled passkey, declines the prompt, the step
 * changed since it was shown, or the gateway refuses the assertion.
 *
 * `onScreen` answers what the surface shows NOW (null: no signed content
 * at all). It is required, so no caller can run a ceremony without
 * saying what is on screen, and it is read at the instant before the
 * begin: a key of `shown` it does not show as signed is refused with a
 * {@link NotShownRefusal} and no request is made (design f623e425 D3).
 *
 * It is read again after every await the ceremony crosses — the begin,
 * the prompt, the finish — because the approver can switch steps or
 * leave the page during any of them (backlog 7c53b1bf, review of car
 * fcda5f8b: read once, a switch during the begin brought the prompt up
 * over the step now shown to sign the one that was not). A surface that
 * is gone answers null. `signal` is handed to the passkey prompt, so a
 * surface that goes away takes its prompt with it; an aborted prompt
 * reads as the refusal it is, not as a browser error.
 */
export async function performPresenceCeremony(
  jobId: string,
  stepId: string,
  shown: ShownStep,
  onScreen: () => ShownStep | null,
  signal?: AbortSignal,
): Promise<string> {
  const stillShown = (): void => {
    const unseen = notShown(shown, onScreen());
    if (unseen.length > 0) throw new NotShownRefusal(unseen);
  };
  stillShown();
  const begin = await fetch('/api/auth/passkey/assert/begin', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ job_id: jobId, step_id: stepId, shown }),
  });
  if (begin.status === 409) {
    throw new Error(
      'No passkey enrolled — add one under My Day → Passkeys first.',
    );
  }
  if (!begin.ok) {
    // The gateway's refusal text names which of its steps failed (job
    // fetch, stored passkeys, challenge mint); the status alone does
    // not (backlog 2e893e27).
    const text = await begin.text().catch(() => '');
    throw new Error(`presence ceremony unavailable (${begin.status}): ${text}`);
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
  stillShown();
  let credential: PublicKeyCredential | null = null;
  try {
    credential = (await navigator.credentials.get({
      signal,
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
  } catch (err) {
    // The DOMException name is the only copy of WHICH failure this was;
    // a bare `catch` here said 'declined or timed out' for an origin
    // mismatch and an empty allow-list too (backlog 2e893e27 — the
    // enrolment half of this defect was f1fd9168).
    // A prompt that ended because its step left the screen (the surface
    // aborted it) is that refusal, not the browser's AbortError.
    stillShown();
    throw new Error(
      assertionFailure(err, opts.publicKey.allowCredentials.length),
    );
  }
  if (!credential) throw new Error('Passkey prompt returned no credential.');
  stillShown();
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
    // e.g. 410 'challenge already spent or expired — begin again', or
    // the verifier's own reason on a 401 — the text the enrolment
    // finish below already surfaces.
    const text = await finish.text().catch(() => '');
    throw new Error(`assertion rejected (${finish.status}): ${text}`);
  }
  const { ticket } = (await finish.json()) as { ticket: string };
  // A ticket for a step no longer on screen is never handed back to be
  // stamped with; unspent, it lapses in its two minutes.
  stillShown();
  return ticket;
}

/**
 * Complete a step, answering a completion refused for PRESENCE with ONE
 * ceremony on the step as `shown` and ONE retry carrying the ticket that
 * ceremony was issued (backlog 3ce3c15f, review of car 5b30ccf9).
 *
 * The jobs API judges assurance on the request that completes a step,
 * from that request's own ticket. A surface can reach it with none it
 * will honour three ways: after a reload the stamp is already on the step
 * and no ceremony runs; the ticket its stamp was issued is past its
 * two-minute life; or the user's role carries no sign-off on the step, so
 * the stamp ceremony never runs. Each stopped the surface at the raw 422.
 *
 * `heldTicket` is a ticket a ceremony on THIS step just issued to the
 * surface, spent on the first attempt and never re-sent. Nothing here
 * mints or widens one: the gateway issues it for this step and person,
 * and the server re-checks step, person, shape and expiry on the retry.
 * Never a second ceremony — a retry the server refuses again for presence
 * is returned failed, saying so; refused for anything else, it is
 * returned as the server said it.
 */
export async function completeWithPresence(
  jobId: string,
  stepId: string,
  shown: ShownStep,
  onScreen: () => ShownStep | null,
  heldTicket?: string,
  signal?: AbortSignal,
): Promise<StepWriteResult> {
  const body = { status: 'completed' };
  const first = await putStep(jobId, stepId, body, heldTicket);
  if (first.kind === 'ok' || !first.presenceRequired) return first;
  let ticket: string;
  try {
    ticket = await performPresenceCeremony(jobId, stepId, shown, onScreen, signal);
  } catch (e) {
    const why = e instanceof Error ? e.message : String(e);
    return { kind: 'failed', error: `Completing needs your passkey, and the ceremony failed: ${why}` };
  }
  const retry = await putStep(jobId, stepId, body, ticket);
  // Only a retry refused for PRESENCE again is "refused again after a
  // fresh passkey tap"; any other refusal — a 409 for stale stamps — is
  // returned in its own words (backlog d82b5f60).
  if (retry.kind === 'ok' || !retry.presenceRequired) return retry;
  return {
    kind: 'failed',
    error: `The completion was refused again after a fresh passkey tap — ${retry.error}`,
  };
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
  } catch (err) {
    // The browser's DOMException name is the only copy of WHICH failure
    // this was; a bare `catch` here threw one blanket message for all
    // six and is why feedback f1fd9168 repeated a55d9a01 six days later.
    throw new Error(enrolmentFailure(err));
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
