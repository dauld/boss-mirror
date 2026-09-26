// Who sees what of an employee's calendar feed (backlog 7ae9ccec,
// 2026-09-25). The same rule the jobs API enforces on
// /api/scheduling/techs/{emp}/calendar-token, so the page offers no
// control the server will refuse:
//
// - 'owner'        the employee themself: the feed URL, copy, rotate.
// - 'rotate-only'  a platform-admin on someone else's page: a revoke
//                  button, whose response carries no token.
// - 'none'         everyone else — another employee, a guest (global
//                  READ is not this token's grant), or a session that
//                  has not resolved yet. The section does not render.
//
// The server is the authority; this only keeps the page honest.

export type CalendarFeedAccess = 'owner' | 'rotate-only' | 'none';

/// Mirrors `boss_core::roles::PLATFORM_ADMIN_ROLE`.
const PLATFORM_ADMIN_ROLE = 'platform-admin';

export function calendarFeedAccess(
  viewer: Readonly<{ id: string; role: string }> | null,
  empId: string,
): CalendarFeedAccess {
  if (viewer === null) return 'none';
  if (viewer.id === empId) return 'owner';
  if (viewer.role === PLATFORM_ADMIN_ROLE) return 'rotate-only';
  return 'none';
}
