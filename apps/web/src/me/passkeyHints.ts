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
