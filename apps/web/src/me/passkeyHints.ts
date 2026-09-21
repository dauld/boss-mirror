// The two sentences the passkey panel owes a person BEFORE and AFTER
// the browser's own enrolment dialog — the one surface in this flow we
// do not draw. Feedback a55d9a01 (David, 2026-09-14, /ux/me): "I tried
// to add a phone passkey — none of my phone Authenticator apps
// recognized the QR." The QR is the browser's cross-device passkey
// link (a FIDO:/ URI), which only the phone's own camera + passkey
// manager (iCloud Keychain, Google Password Manager) can answer; a TOTP
// authenticator app scans for otpauth:// codes and ignores it. Nothing
// in the dialog says so, so this page must — before the click, and
// again in the error a timed-out scan comes back as.

/** Shown under the Add button, before the browser dialog opens. */
export const PHONE_QR_HINT =
  'Your browser will ask where to keep the passkey: a security key, this device, or a phone. ' +
  'For a phone, scan the QR with the phone’s Camera app — not an authenticator app. ' +
  'The code is a passkey link that only the phone’s own passkey manager (iCloud Keychain, Google Password Manager) can answer.';

/** The error a declined or timed-out ceremony surfaces as. A phone scan
 *  that never connected is the common way to time out, so the way out
 *  rides in the error rather than only above the button. */
export const CEREMONY_DECLINED =
  'Passkey creation was declined or timed out. ' +
  'If you were scanning the QR with a phone: use the phone’s Camera app, not an authenticator app — authenticator apps do not read passkey links.';

/** What the browser's failure actually was, said in the words a person
 *  can act on — and always carrying the browser's own reason so a
 *  report six days later is diagnosable.
 *
 *  WHY THIS EXISTS. `enrollPasskey` caught the ceremony with a bare
 *  `catch {}` and threw CEREMONY_DECLINED for EVERY outcome. Feedback
 *  a55d9a01 (2026-09-14) was answered by improving that one message —
 *  and f1fd9168 (2026-09-20) reported the same symptom again, because
 *  the message was never the thing that varied. A `InvalidStateError`
 *  shown as "declined or timed out … use the Camera app" sends someone
 *  to repeat the one action that cannot succeed.
 *
 *  The DOMException `name` is the only copy of which failure it was.
 *  Discarding it before storing it is throwing away the only copy
 *  (CLAUDE.md §Diagnosis: quiet is a loan against the next diagnosis).
 *
 *  An UNRECOGNISED name is reported verbatim and claims nothing. The
 *  defect being fixed is a message that asserted a cause it did not
 *  know; replacing it with a different confident guess would be the
 *  same defect in new words. */
export function enrolmentFailure(err: unknown): string {
  const name =
    err instanceof Error && err.name ? err.name : String(err ?? 'unknown error');
  const because = ` (the browser reported ${name}.)`;

  switch (name) {
    // The authenticator already holds one of the credentials this
    // account excluded — and with a SYNCED provider that includes a
    // passkey enrolled on a different device, which is why this reads
    // as "my phone won't add one" when the phone already has it.
    case 'InvalidStateError':
      return (
        'That device already has a passkey for this account, so it declined to add a second. ' +
        'A passkey kept in iCloud Keychain or Google Password Manager syncs to your other devices, ' +
        'so one enrolled elsewhere is already usable on your phone — there is nothing to add.' +
        because
      );

    // The only case the old blanket message was ever right about — and
    // it carries its reason like every other, so a report of THIS one
    // is as diagnosable as the rest.
    case 'NotAllowedError':
      return CEREMONY_DECLINED + because;

    case 'SecurityError':
      return (
        'This site’s address does not match the domain its passkeys are registered to, so the browser refused. ' +
        'That is a problem with how the site is being reached, not with your device.' +
        because
      );

    case 'NotSupportedError':
    case 'ConstraintError':
      return (
        'The device you chose cannot meet what this site asks of a passkey. ' +
        'Try a different one — this device, a phone, or a security key.' +
        because
      );

    case 'AbortError':
      return 'Passkey creation was cancelled before it finished.' + because;

    default:
      return (
        'Passkey creation failed, and the browser gave a reason this page does not recognise. ' +
        'Please report it with the reason below.' +
        because
      );
  }
}
