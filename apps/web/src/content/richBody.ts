// Pure tokenizer for RichBody — extracted so Svelte components can
// render each token without an HTML string. Mirrors the logic in
// apps/web/src/content/RichBody.tsx.

import type { EntityKind } from '@boss/web-kit/ui/entity-href';

type Pattern = {
  re: RegExp;
  kind: EntityKind;
};

const PATTERNS: ReadonlyArray<Pattern> = [
  { re: /\b(emp-\d{3,})\b/gu, kind: 'employee' },
  { re: /\b(account-[0-9a-z-]+)\b/gu, kind: 'account' },
  { re: /\b(job-[0-9a-z-]+)\b/gu, kind: 'job' },
  { re: /\b(INV-[A-Z0-9-]+)\b/gu, kind: 'invoice' },
  { re: /\b(SYS-[A-Z0-9-]+)\b/gu, kind: 'asset' },
  { re: /\b(part-[0-9a-z-]+)\b/gu, kind: 'part' },
  { re: /\b(PO-[A-Z0-9-]+)\b/gu, kind: 'po' },
  { re: /\b(SHIP-[A-Z0-9-]+)\b/gu, kind: 'shipment' },
];

export type RichToken =
  | { kind: 'text'; text: string }
  | { kind: 'link'; entityKind: EntityKind; id: string };

export function tokenize(body: string): RichToken[] {
  type Hit = { start: number; end: number; kind: EntityKind; id: string };
  const hits: Hit[] = [];
  for (const { re, kind } of PATTERNS) {
    re.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = re.exec(body)) !== null) {
      const id = m[1];
      if (!id) continue;
      const start = m.index + (m[0]!.length - id.length);
      hits.push({ start, end: start + id.length, kind, id });
    }
  }
  if (hits.length === 0) return [{ kind: 'text', text: body }];
  hits.sort((a, b) => a.start - b.start);
  const out: RichToken[] = [];
  let cursor = 0;
  for (const h of hits) {
    if (h.start < cursor) continue;
    if (h.start > cursor) {
      out.push({ kind: 'text', text: body.slice(cursor, h.start) });
    }
    out.push({ kind: 'link', entityKind: h.kind, id: h.id });
    cursor = h.end;
  }
  if (cursor < body.length) {
    out.push({ kind: 'text', text: body.slice(cursor) });
  }
  return out;
}
