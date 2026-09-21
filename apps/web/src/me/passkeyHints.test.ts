import { describe, expect, test } from 'bun:test';
import {
  CEREMONY_DECLINED,
  PHONE_QR_HINT,
  enrolmentFailure,
} from './passkeyHints';

// Feedback a55d9a01: the phone QR was scanned with authenticator apps,
// which cannot read it. Both sentences the page owes must name the app
// that CAN (the camera) and the one that cannot (an authenticator app),
// and the hint must say the QR comes from the browser, since this page
// never draws it.
describe('the phone passkey QR says which app reads it', () => {
  test('the hint before the click names the camera and warns off authenticator apps', () => {
    expect(PHONE_QR_HINT).toContain('Camera app');
    expect(PHONE_QR_HINT).toContain('not an authenticator app');
    expect(PHONE_QR_HINT).toContain('browser');
  });
  test('a declined or timed-out ceremony carries the same way out', () => {
    expect(CEREMONY_DECLINED).toContain('declined or timed out');
    expect(CEREMONY_DECLINED).toContain('Camera app');
    expect(CEREMONY_DECLINED).toContain('not an authenticator app');
  });
});

// Feedback f1fd9168 (David, 2026-09-20, /ux/me): "Tried to add
// app-based phone passkey and couldn't." This is the SECOND report of
// the same symptom — a55d9a01 was 2026-09-14 — and the reason it came
// back is that `enrollPasskey` caught the browser's failure with a bare
// `catch {}` and replaced EVERY outcome with CEREMONY_DECLINED. So the
// 09-14 fix improved the prose of one message that was being shown for
// all six failures, including ones it actively misdescribes: a phone
// that already holds a synced credential fails InvalidStateError, and
// telling that person to "use the Camera app" sends them to do again
// the thing that cannot work.
//
// The browser already knows which failure it was. The DOMException name
// is the only copy of that fact, and discarding it before storing it is
// throwing away the only copy (CLAUDE.md §Diagnosis).
describe('an enrolment failure says WHICH failure', () => {
  const dom = (name: string) => {
    const e = new Error(`${name} raised`);
    e.name = name;
    return e;
  };

  test('a device that already holds a credential is not told to rescan', () => {
    const msg = enrolmentFailure(dom('InvalidStateError'));
    expect(msg).toContain('already has a passkey');
    // The misdirection this whole car exists to remove.
    expect(msg).not.toContain('Camera app');
    expect(msg).not.toContain('declined or timed out');
  });

  test('a declined or timed-out ceremony still carries the camera hint', () => {
    // Contains, not equals: it carries the browser's reason too, like
    // every other message here, so a report of this one is diagnosable.
    expect(enrolmentFailure(dom('NotAllowedError'))).toContain(
      CEREMONY_DECLINED,
    );
  });

  test('an origin mismatch is named as a site problem, not a user one', () => {
    const msg = enrolmentFailure(dom('SecurityError'));
    expect(msg).toContain('address');
    expect(msg).not.toContain('Camera app');
  });

  test('an authenticator that cannot meet the requirements says so', () => {
    for (const name of ['NotSupportedError', 'ConstraintError']) {
      const msg = enrolmentFailure(dom(name));
      expect(msg).toContain('cannot');
      expect(msg).not.toContain('Camera app');
    }
  });

  test('every message names the browser reason, so a report can be diagnosed', () => {
    // Without this, a second report six days later is as unreadable as
    // the first — which is exactly what happened between a55d9a01 and
    // f1fd9168.
    for (const name of [
      'InvalidStateError',
      'NotAllowedError',
      'SecurityError',
      'NotSupportedError',
      'ConstraintError',
      'AbortError',
    ]) {
      expect(enrolmentFailure(dom(name))).toContain(name);
    }
  });

  test('an unrecognised failure is reported verbatim rather than flattened', () => {
    const msg = enrolmentFailure(dom('SomeFutureError'));
    expect(msg).toContain('SomeFutureError');
    // The old behaviour — every unknown becoming "declined or timed
    // out" — is the defect, so an unknown must NOT claim to know.
    expect(msg).not.toContain('declined or timed out');
  });

  test('a non-Error throw still produces something a person can report', () => {
    expect(enrolmentFailure('boom')).toContain('boom');
    expect(enrolmentFailure(undefined)).toBeTruthy();
  });
});
