import { describe, expect, test } from 'bun:test';
import { filenameOf, readExport } from './auditExport';

// Backlog 4630ebc0 (page audit 65a273d5): Save .jsonl navigated the
// window to /api/events/export, so a refusal replaced the app with the
// raw body and a failure after the 200 was reported nowhere. The export
// is now READ, and each way it can end is a named outcome.

const URL_ = '/api/events/export?since=2026-09-01T00:00:00Z';

const answering = (res: Response) => (async () => res) as unknown as typeof fetch;

describe('readExport — every way an export can end is named', () => {
  test('a whole body is saved under the server filename, with its event count', async () => {
    const body = '{"event_id":"ev-1"}\n{"event_id":"ev-2"}\n';
    const res = new Response(body, {
      status: 200,
      headers: { 'content-disposition': 'attachment; filename="audit-log-20260901-20260904.jsonl"' },
    });
    const out = await readExport(answering(res), URL_);
    expect(out.kind).toBe('saved');
    if (out.kind !== 'saved') return;
    expect(out.filename).toBe('audit-log-20260901-20260904.jsonl');
    expect(out.events).toBe(2);
    expect(await out.blob.text()).toBe(body);
    expect(out.blob.type).toBe('application/x-ndjson');
  });

  test('an empty window is still a saved file, and says 0 events', async () => {
    const out = await readExport(answering(new Response('', { status: 200 })), URL_);
    expect(out).toMatchObject({ kind: 'saved', events: 0, filename: 'audit-log.jsonl' });
  });

  test('a refusal carries its status and the server words, and saves nothing', async () => {
    const res = new Response('operator tier or executive role required', { status: 403 });
    const out = await readExport(answering(res), URL_);
    expect(out).toEqual({
      kind: 'failed',
      message: 'The export was refused: HTTP 403: operator tier or executive role required. Nothing was saved.',
    });
  });

  test('a body that breaks off after the 200 is a failure, never a short file', async () => {
    // The server streams rows after its 200, and a failed row read ends
    // the body with an error — the shape tail_http.rs's export produces.
    const enc = new TextEncoder();
    let sent = false;
    const stream = new ReadableStream<Uint8Array>({
      pull(c) {
        if (!sent) {
          sent = true;
          c.enqueue(enc.encode('{"event_id":"ev-1"}\n'));
        } else {
          c.error(new Error('connection reset'));
        }
      },
    });
    const out = await readExport(answering(new Response(stream, { status: 200 })), URL_);
    expect(out).toEqual({
      kind: 'failed',
      message:
        'The export broke off before its end (connection reset). Nothing was saved: a partial file would read as a whole one.',
    });
  });

  test('a request that never answers is named too', async () => {
    const refusing = (async () => {
      throw new TypeError('Failed to fetch');
    }) as unknown as typeof fetch;
    const out = await readExport(refusing, URL_);
    expect(out).toEqual({
      kind: 'failed',
      message: 'The export could not be read (Failed to fetch). Nothing was saved.',
    });
  });
});

describe('filenameOf — the Content-Disposition the server sends', () => {
  test('quoted, bare, and absent', () => {
    expect(filenameOf('attachment; filename="a.jsonl"')).toBe('a.jsonl');
    expect(filenameOf('attachment; filename=b.jsonl')).toBe('b.jsonl');
    expect(filenameOf(null)).toBe('audit-log.jsonl');
    expect(filenameOf('attachment')).toBe('audit-log.jsonl');
  });
});
