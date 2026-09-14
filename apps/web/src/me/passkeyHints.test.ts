import { describe, expect, test } from 'bun:test';
import { CEREMONY_DECLINED, PHONE_QR_HINT } from './passkeyHints';

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
