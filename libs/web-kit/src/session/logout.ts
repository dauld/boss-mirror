// The sign-out request, judged. SignInControl navigates to /login only
// on `signed-out`; a `refused` or `unreachable` answer stays on the
// page and is shown, because a chrome that looks signed out over a
// session that is still live is the record and the surface
// disagreeing (backlog a5dff6f1, found by the interaction crawl's
// refused-write leg, car f09aafe1).

export type LogoutOutcome =
  | { kind: 'signed-out' }
  | { kind: 'refused'; status: number; detail: string }
  | { kind: 'unreachable'; detail: string };

/** The one shape of fetch this needs — injectable so the judgement is
 *  testable without a browser. */
export type LogoutFetch = (input: string, init?: RequestInit) => Promise<Response>;

/** POST /api/auth/logout and say what the gateway answered. */
export async function requestLogout(fetchFn: LogoutFetch = fetch): Promise<LogoutOutcome> {
  let r: Response;
  try {
    r = await fetchFn('/api/auth/logout', { method: 'POST' });
  } catch (e) {
    return { kind: 'unreachable', detail: e instanceof Error ? e.message : String(e) };
  }
  if (r.ok) return { kind: 'signed-out' };
  return { kind: 'refused', status: r.status, detail: await refusalDetail(r) };
}

/** The reason a refusal gives: the JSON body's `error` when it has
 *  one, else the body text, trimmed. Never throws — an unreadable body
 *  is an empty detail beside a status that still says what happened. */
async function refusalDetail(r: Response): Promise<string> {
  try {
    const text = (await r.text()).trim();
    try {
      const parsed = JSON.parse(text) as unknown;
      if (parsed && typeof parsed === 'object' && typeof (parsed as { error?: unknown }).error === 'string') {
        return (parsed as { error: string }).error;
      }
    } catch {
      // not JSON — the text is the detail
    }
    return text;
  } catch {
    return '';
  }
}
