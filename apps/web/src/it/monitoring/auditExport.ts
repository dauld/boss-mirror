// Save .jsonl on /it/operate/audit — the export, READ rather than
// navigated to (backlog 4630ebc0, page audit 65a273d5).
//
// It used to be `window.location.href = '/api/events/export?…'`. That
// saved a whole 200 through Content-Disposition, and got every other
// ending wrong: a refusal replaced the app with the raw body at the
// export URL, and a failure AFTER the 200 was reported nowhere. The
// server streams its rows behind the status line, and a failed row read
// ends the body with an error, so the browser kept a short file that
// reads as a whole one. Reading the body to its end first is the only
// way to know it ended, and the only honest file is a complete one.

export type ExportOutcome =
  | { kind: 'saved'; filename: string; blob: Blob; events: number }
  | { kind: 'failed'; message: string };

const FALLBACK_NAME = 'audit-log.jsonl';

/// The filename the server's Content-Disposition names — the one the
/// navigation used to save under — or a plain fallback.
export function filenameOf(disposition: string | null): string {
  const m = /filename="?([^";]+)"?/i.exec(disposition ?? '');
  return m?.[1]?.trim() || FALLBACK_NAME;
}

const why = (e: unknown): string => (e instanceof Error ? e.message : String(e));

/// Read the export to its end. `fetchFn` is the browser's fetch on the
/// page and a stand-in in the unit test.
export async function readExport(fetchFn: typeof fetch, url: string): Promise<ExportOutcome> {
  let res: Response;
  try {
    res = await fetchFn(url, { credentials: 'same-origin', headers: { accept: 'application/x-ndjson' } });
  } catch (e) {
    return { kind: 'failed', message: `The export could not be read (${why(e)}). Nothing was saved.` };
  }
  if (!res.ok) {
    const said = (await res.text().catch(() => '')).trim();
    return {
      kind: 'failed',
      message: `The export was refused: HTTP ${res.status}${said ? `: ${said}` : ''}. Nothing was saved.`,
    };
  }
  let body: string;
  try {
    body = await res.text();
  } catch (e) {
    return {
      kind: 'failed',
      message: `The export broke off before its end (${why(e)}). Nothing was saved: a partial file would read as a whole one.`,
    };
  }
  return {
    kind: 'saved',
    filename: filenameOf(res.headers.get('content-disposition')),
    blob: new Blob([body], { type: 'application/x-ndjson' }),
    events: body.split('\n').filter((l) => l.length > 0).length,
  };
}
