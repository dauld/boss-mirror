// Who sees what of an employee's calendar feed (backlog 7ae9ccec).
// The feed URL is the whole authentication of the sessionless
// /ics/{token}/calendar.ics — 90 days back and 180 ahead of the
// employee's schedule — and the employee page rendered it for every
// viewer of every employee. The server now refuses the read to anyone
// but the employee and the rotate to anyone but the employee or a
// platform-admin; these cases hold the page to the same rule, so it
// offers nobody a control the server will refuse.

import { describe, expect, it } from 'bun:test';
import { calendarFeedAccess } from './calendarFeedAccess';

const viewer = (id: string, role: string) => ({ id, role });

describe('calendarFeedAccess', () => {
  it('shows the employee their own feed', () => {
    expect(calendarFeedAccess(viewer('emp-tech-001', 'service-tech'), 'emp-tech-001')).toBe(
      'owner',
    );
  });

  it('gives an operator a rotate-only control, never the feed', () => {
    expect(calendarFeedAccess(viewer('emp-david', 'platform-admin'), 'emp-tech-001')).toBe(
      'rotate-only',
    );
  });

  it('an operator viewing their own page is the owner', () => {
    expect(calendarFeedAccess(viewer('emp-david', 'platform-admin'), 'emp-david')).toBe('owner');
  });

  it('shows another employee nothing', () => {
    expect(calendarFeedAccess(viewer('emp-tech-002', 'service-tech'), 'emp-tech-001')).toBe(
      'none',
    );
  });

  it('shows a guest nothing, however global its read', () => {
    expect(
      calendarFeedAccess(viewer('guest@algedonic.dev', 'audit-readonly'), 'emp-tech-001'),
    ).toBe('none');
  });

  it('shows no one anything before the session has said who they are', () => {
    expect(calendarFeedAccess(null, 'emp-tech-001')).toBe('none');
  });
});
