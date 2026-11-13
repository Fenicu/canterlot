import { describe, expect, it } from 'vitest';
import { prettyToolOutput } from '#lib/timeline/present.js';

describe('prettyToolOutput', () => {
  it('indents the compact answer of a canterlot tool and keeps the hint', () => {
    const text = prettyToolOutput('{"tasks":[{"number":1,"title":"Собрать"}],"next":null}\nCall plan_list.');
    expect(text).toBe('{\n  "tasks": [\n    {\n      "number": 1,\n      "title": "Собрать"\n    }\n  ],\n  "next": null\n}\nCall plan_list.');
  });

  it('indents a compact JSON tail after a plain-text search answer', () => {
    const text = prettyToolOutput('Найдено 1 совпадение.\n\n{"hits":[{"session_id":"s","snippet":"a"}],"next_cursor":null}');
    expect(text).toContain('\n\n{\n  "hits": [');
    expect(text).toContain('"next_cursor": null');
    expect(text).not.toContain('"hits":[{"');
  });

  it('leaves text that is not JSON alone', () => {
    expect(prettyToolOutput('Claude API budget: five-hour window 15% used.')).toBe('Claude API budget: five-hour window 15% used.');
  });

  it('leaves truncated JSON alone', () => {
    const cut = '{"tasks":[{"number":1,"title":"Собр';
    expect(prettyToolOutput(cut)).toBe(cut);
  });
});
