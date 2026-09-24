import { describe, expect, test } from 'bun:test';
import { packetOf, windowBound } from './auditRetro';

describe('packetOf — the packet an audit row belongs to (backlog 62a0bbee)', () => {
  test('a job state or marker event names its packet as `id`', () => {
    expect(packetOf('jobs.job.created', { id: 'job-1', kind: 'backlog-item' })).toBe('job-1');
    expect(packetOf('jobs.job.updated', { id: 'job-1' })).toBe('job-1');
    expect(packetOf('jobs.job.closed', { id: 'job-1', status: 'closed' })).toBe('job-1');
  });

  test('a step event names its packet as `job_id`, never its own `id`', () => {
    // The Step struct serializes its own identity as `id` (and
    // `step_id`); the packet is `job_id` (boss-jobs events.rs).
    expect(packetOf('jobs.step.updated', { id: 'step-9', step_id: 'step-9', job_id: 'job-1' })).toBe('job-1');
    expect(packetOf('jobs.step.completed', { job_id: 'job-1', step_id: 'step-9' })).toBe('job-1');
    expect(packetOf('jobs.step.updated', { id: 'step-9' })).toBeNull();
  });

  test('any row whose payload carries a `job_id` names that packet', () => {
    expect(packetOf('dispatcher.rule.fired', { rule: 'r1', job_id: 'job-7' })).toBe('job-7');
  });

  test('an `id` outside jobs.job.* is some other thing, not a packet', () => {
    expect(packetOf('jobs.kind.published', { id: 'wf-1', kind: 'backlog-item' })).toBeNull();
    expect(packetOf('credential.rotate.verified', { id: 'forge' })).toBeNull();
  });

  test('no payload, a blank id, or a non-string id names no packet', () => {
    expect(packetOf('jobs.job.created', null)).toBeNull();
    expect(packetOf('jobs.job.created', 'text')).toBeNull();
    expect(packetOf('jobs.job.created', [{ id: 'job-1' }])).toBeNull();
    expect(packetOf('jobs.job.created', { id: '' })).toBeNull();
    expect(packetOf('jobs.step.updated', { job_id: 42 })).toBeNull();
  });
});

describe('windowBound — a datetime-local value as the RFC 3339 instant the tail takes', () => {
  test('reads the value in the browser zone, as the page paints its times, and sends UTC', () => {
    expect(windowBound('2026-09-23T14:05')).toBe(new Date('2026-09-23T14:05').toISOString());
    expect(windowBound('2026-09-23T14:05')).toMatch(/Z$/);
  });

  test('a blank or unreadable value is no bound at all', () => {
    expect(windowBound('')).toBeNull();
    expect(windowBound('  ')).toBeNull();
    expect(windowBound('not a time')).toBeNull();
  });
});
