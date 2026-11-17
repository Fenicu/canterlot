import { describe, expect, it } from 'vitest';
import { resolveCard } from '../../../../src/lib/timeline/cards/registry';
import { cardCall } from '../../../../src/lib/timeline/cards/call';
import type { ShellData } from '../../../../src/lib/timeline/cards/defs/shell';
import { callBlock, made, resultBlock } from './helpers';

const NOW = Date.parse('2026-11-07T10:05:00Z');
const LONG = 'cd web && npm ci --no-audit --no-fund && npm run lint:tokens && npm run test:unit';

function shell(call: ReturnType<typeof made>) {
  const resolved = resolveCard(call, NOW);
  return resolved as (NonNullable<typeof resolved> & { data: ShellData }) | null;
}

describe('shell card', () => {
  it('claude_bash_reads_its_description_and_keeps_the_original_command_for_copy', () => {
    const card = shell(made('Bash', { command: LONG, description: 'Run web checks' }, 'ok'))!;
    expect(card.line).toBe('Run web checks');
    expect(card.data.command).toBe(LONG);
    expect(card.data.formatted.split('\n')).toEqual(['cd web', '  && npm ci --no-audit --no-fund', '  && npm run lint:tokens', '  && npm run test:unit']);
    expect(card.word).toBeNull();
  });

  it('a_command_without_description_names_the_command', () => {
    expect(shell(made('Bash', { command: 'ls -la' }, ''))!.line).toBe('Команда: ls -la');
  });

  it('claude_failure_takes_the_exit_code_from_the_output', () => {
    const call = cardCall(callBlock('Bash', { command: 'npm test' }), resultBlock('Exit code 2\nFAIL src/a.test.ts', { summary: 'error' }), { root: true });
    const card = shell(call)!;
    expect(card.data.outcome).toEqual({ exit: 2, denied: false });
    expect(card.word).toEqual({ word: 'код 2', tone: 'danger' });
  });

  it('claude_denial_reads_as_denied_not_as_an_error', () => {
    const call = cardCall(callBlock('Bash', { command: 'rm -rf build' }), resultBlock('Denied by the owner. Do not retry with different arguments; end your turn or ask the owner what to do.', { summary: 'error' }), { root: true });
    expect(shell(call)!.word).toEqual({ word: 'отклонено', tone: 'warning' });
  });

  it('opencode_and_codex_send_the_raw_command_and_the_status_in_the_summary', () => {
    const opencode = cardCall(callBlock('bash', LONG, { summary: 'bash: cd web, exit 1' }), resultBlock('boom'), { root: true });
    const card = shell(opencode)!;
    expect(card.data.command).toBe(LONG);
    expect(card.data.outcome).toEqual({ exit: 1, denied: false });
    expect(card.word).toEqual({ word: 'код 1', tone: 'danger' });
    const codex = cardCall(callBlock('shell', 'cargo test', { summary: 'cargo test, exit 0' }), resultBlock('ok'), { root: true });
    expect(shell(codex)!.data.outcome).toEqual({ exit: 0, denied: false });
    expect(shell(codex)!.word).toBeNull();
    const declined = cardCall(callBlock('shell', 'rm -rf /tmp/x', { summary: 'rm -rf /tmp/x, declined' }), resultBlock(''), { root: true });
    expect(shell(declined)!.word).toEqual({ word: 'отклонено', tone: 'warning' });
  });

  it('a_running_codex_command_without_text_reads_the_summary', () => {
    const block = callBlock('shell', '', { state: 'streaming', summary: 'npm run build' });
    const card = shell(cardCall(block, null, { root: true, working: true }))!;
    expect(card.data.command).toBe('npm run build');
    expect(card.word).toMatchObject({ word: 'идёт', live: true });
  });

  it('cwd_and_background_come_from_the_input_and_codex_argv_is_unwrapped', () => {
    const card = shell(made('Bash', { command: 'npm run dev', cwd: '/srv/app', run_in_background: true }, 'Command running in background with ID: b1'))!;
    expect(card.data.cwd).toBe('/srv/app');
    expect(card.data.background).toBe(true);
    expect(shell(made('shell', { command: ['bash', '-lc', 'ls && pwd'] }, ''))!.data.command).toBe('ls && pwd');
    expect(shell(made('exec_command', { cmd: 'git status' }, ''))!.data.command).toBe('git status');
  });

  it('a_call_without_any_command_falls_back_to_the_generic_block', () => {
    expect(resolveCard(made('Bash', { nothing: true }, 'out'), NOW)).toBeNull();
  });
});
