import { expect, test } from 'vitest';
import { isSpawnCall, parseSpawnAnswer } from '#lib/routing/spawnAnswer.js';

test('spawn calls are recognised for every harness naming', () => {
  expect(isSpawnCall('mcp__canterlot__session_spawn')).toBe(true);
  expect(isSpawnCall('canterlot_session_spawn')).toBe(true);
  expect(isSpawnCall('canterlot.session_spawn')).toBe(true);
  expect(isSpawnCall('session_spawn')).toBe(true);
  expect(isSpawnCall('mcp__canterlot__session_spawn_preview')).toBe(false);
  expect(isSpawnCall('Bash')).toBe(false);
});

test('answer JSON is read from the tool output', () => {
  const text = 'Result:\n{\n  "status": "started",\n  "session_id": "s1",\n  "routing": {"task_type": "bugfix", "harness": "codex", "model": "gpt", "effort": "high"}\n}';
  expect(parseSpawnAnswer(text)).toMatchObject({ status: 'started', session_id: 's1', routing: { task_type: 'bugfix' } });
  expect(parseSpawnAnswer('{"status":"queued","reason":"rate","detail":"10 spawns in the last hour"}')).toMatchObject({ status: 'queued', reason: 'rate' });
  expect(parseSpawnAnswer('no json here')).toBeNull();
  expect(parseSpawnAnswer('{broken')).toBeNull();
  expect(parseSpawnAnswer('{"other": 1}')).toBeNull();
});
