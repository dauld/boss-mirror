// The inbox's writes, resolved to an answer the page can render.
//
// Both of the page's writes swallowed a refusal (page audit 5477d9eb,
// 2026-09-23). Mark read (backlog 129da587, GAP 5) never read `r.ok`
// and caught every throw into nothing, so the re-read showed the row
// still unread with no reason; the entity link fired the same write
// unawaited and navigated away from wherever its answer would have
// shown. Send (backlog 2a6fab80, GAP 4) read `r.ok` only to close the
// modal: a 4xx/5xx left it open with no line saying why, and a network
// throw had try/finally and no catch, so it became an unhandled
// rejection. A write the server refused is a fact about the world the
// page must say, not a no-op it may leave the viewer to infer.
//
// House style (CLAUDE.md §TypeScript): a discriminated union for an
// expected failure, never an exception — `postWrite` resolves done or
// refused-with-a-reason and never throws, so a caller that stores the
// answer has a sentence for every failure by construction.

export type WriteOutcome = { kind: 'done' } | { kind: 'refused'; reason: string };

/// One POST. A non-ok status is refused with the status and the
/// server's own message (its body, trimmed — empty adds nothing); a
/// thrown fetch is refused with the thrown message.
export async function postWrite(url: string, init: RequestInit = {}): Promise<WriteOutcome> {
  try {
    const r = await fetch(url, { ...init, method: 'POST' });
    if (r.ok) return { kind: 'done' };
    const said = (await r.text().catch(() => '')).trim();
    return { kind: 'refused', reason: said ? `HTTP ${r.status}: ${said}` : `HTTP ${r.status}` };
  } catch (e) {
    return { kind: 'refused', reason: e instanceof Error ? e.message : String(e) };
  }
}
