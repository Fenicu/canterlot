import { describe, expect, it } from 'vitest';
import { formatCommand, splitCommand, SHELL_WRAP } from '../../../../src/lib/timeline/cards/shell';

/** Длинная команда: правило переноса включается только за пределом ширины. */
const pad = (text: string) => (text.length > SHELL_WRAP ? text : `${text} ${'x'.repeat(SHELL_WRAP - text.length + 1)}`);

describe('splitCommand', () => {
  it('splits_at_top_level_list_and_pipe_operators', () => {
    expect(splitCommand('cd web && npm ci || echo fail; ls | head')).toEqual([
      { op: '', text: 'cd web' },
      { op: '&&', text: 'npm ci' },
      { op: '||', text: 'echo fail' },
      { op: ';', text: 'ls' },
      { op: '|', text: 'head' }
    ]);
  });

  it('keeps_single_and_double_quotes_whole', () => {
    expect(splitCommand(`echo 'a && b; c | d' && echo "x || y; z | w"`)).toEqual([
      { op: '', text: `echo 'a && b; c | d'` },
      { op: '&&', text: 'echo "x || y; z | w"' }
    ]);
  });

  it('respects_escaped_quotes_and_escaped_operators', () => {
    expect(splitCommand('echo "say \\"a && b\\"" && find . -exec rm {} \\; && echo a\\|b')).toEqual([
      { op: '', text: 'echo "say \\"a && b\\""' },
      { op: '&&', text: 'find . -exec rm {} \\;' },
      { op: '&&', text: 'echo a\\|b' }
    ]);
  });

  it('does_not_split_inside_command_substitution_subshells_or_backticks', () => {
    expect(splitCommand('x=$(cd a && ls | wc -l) && (cd b; make) && echo `date; whoami` && echo "$(echo ")" && true)"')).toEqual([
      { op: '', text: 'x=$(cd a && ls | wc -l)' },
      { op: '&&', text: '(cd b; make)' },
      { op: '&&', text: 'echo `date; whoami`' },
      { op: '&&', text: 'echo "$(echo ")" && true)"' }
    ]);
  });

  it('does_not_split_inside_double_brackets_braces_or_arithmetic', () => {
    expect(splitCommand('[[ -f a && -n "$b" || -z $c ]] && echo ${x:-a;b} && echo $(( 1 | 2 ))')).toEqual([
      { op: '', text: '[[ -f a && -n "$b" || -z $c ]]' },
      { op: '&&', text: 'echo ${x:-a;b}' },
      { op: '&&', text: 'echo $(( 1 | 2 ))' }
    ]);
  });

  it('treats_redirections_with_ampersands_and_clobber_pipes_as_words', () => {
    expect(splitCommand('make 2>&1 &> log >| out && cmd |& tee x & sleep 1')).toEqual([
      { op: '', text: 'make 2>&1 &> log >| out' },
      { op: '&&', text: 'cmd' },
      { op: '|&', text: 'tee x' },
      { op: '&', text: 'sleep 1' }
    ]);
  });

  it('keeps_compound_commands_whole', () => {
    expect(splitCommand('for f in *.ts; do echo $f; done && if [ -f a ]; then echo y; fi && { a; b; }')).toEqual([
      { op: '', text: 'for f in *.ts; do echo $f; done' },
      { op: '&&', text: 'if [ -f a ]; then echo y; fi' },
      { op: '&&', text: '{ a; b; }' }
    ]);
    expect(splitCommand('case $x in a) echo a;; b) echo b;; esac; echo done')).toEqual([
      { op: '', text: 'case $x in a) echo a;; b) echo b;; esac' },
      { op: ';', text: 'echo done' }
    ]);
  });

  it('stops_at_a_comment_and_at_a_heredoc', () => {
    expect(splitCommand('ls && echo # a && b')).toEqual([
      { op: '', text: 'ls' },
      { op: '&&', text: 'echo # a && b' }
    ]);
    expect(splitCommand('echo a#b && ls')).toEqual([
      { op: '', text: 'echo a#b' },
      { op: '&&', text: 'ls' }
    ]);
    expect(splitCommand('cat <<EOF && echo a | b')).toEqual([{ op: '', text: 'cat <<EOF && echo a | b' }]);
    expect(splitCommand('cat <<<"a && b" && ls')).toEqual([
      { op: '', text: 'cat <<<"a && b"' },
      { op: '&&', text: 'ls' }
    ]);
  });

  it('gives_up_on_unbalanced_input', () => {
    expect(splitCommand(`echo 'oops && ls`)).toBeNull();
    expect(splitCommand('echo $(oops && ls')).toBeNull();
    expect(splitCommand('echo "oops && ls')).toBeNull();
  });
});

describe('formatCommand', () => {
  it('leaves_short_commands_alone', () => {
    expect(formatCommand('git add -A && git commit -m wip')).toBe('git add -A && git commit -m wip');
    expect(formatCommand('ls | head')).toBe('ls | head');
    expect(formatCommand('')).toBe('');
  });

  it('breaks_a_long_and_chain_with_continuation_indent', () => {
    const command = 'cd web && npm ci --no-audit --no-fund && npm run lint:tokens && npm run test:unit -- --reporter=dot';
    expect(formatCommand(command)).toBe(['cd web', '  && npm ci --no-audit --no-fund', '  && npm run lint:tokens', '  && npm run test:unit -- --reporter=dot'].join('\n'));
  });

  it('puts_semicolon_commands_on_their_own_unindented_lines', () => {
    const command = 'cd /home/user/projects/canterlot/web; npm run build; ls -la build/ | head -5';
    expect(formatCommand(command)).toBe(['cd /home/user/projects/canterlot/web;', 'npm run build;', 'ls -la build/ | head -5'].join('\n'));
  });

  it('breaks_long_pipelines_one_stage_per_line', () => {
    const command = 'git log --format="%h %s" --since=2.weeks | grep -v Merge | sort -k2 | uniq -c | head -20';
    expect(formatCommand(command)).toBe(['git log --format="%h %s" --since=2.weeks', '  | grep -v Merge', '  | sort -k2', '  | uniq -c', '  | head -20'].join('\n'));
  });

  it('mixes_lists_and_pipelines_and_keeps_short_pipelines_on_one_line', () => {
    const command = 'cd web && rg -n "todo" src | wc -l || echo "no matches found in the source tree"; npm test 2>&1 | tail -5';
    expect(formatCommand(command)).toBe(['cd web', '  && rg -n "todo" src | wc -l', '  || echo "no matches found in the source tree";', 'npm test 2>&1 | tail -5'].join('\n'));
  });

  it('indents_a_long_pipeline_inside_a_continuation_one_level_deeper', () => {
    const command = 'cd web && npm run test:unit -- --reporter=verbose tests/unit/timeline 2>&1 | grep -E "FAIL|PASS" | tail -40';
    expect(formatCommand(command)).toBe(['cd web', '  && npm run test:unit -- --reporter=verbose tests/unit/timeline 2>&1', '    | grep -E "FAIL|PASS"', '    | tail -40'].join('\n'));
  });

  it('keeps_a_background_job_marker_on_its_line', () => {
    const command = pad('npm run dev -- --port 5173 & sleep 2 && curl -s localhost:5173');
    expect(formatCommand(command).split('\n')).toEqual(['npm run dev -- --port 5173 &', 'sleep 2', `  && ${command.slice(command.indexOf('curl'))}`]);
  });

  it('leaves_already_multiline_input_as_is', () => {
    const command = 'cat > /tmp/a.txt <<EOF\nline && one | two\nEOF\nls && pwd';
    expect(formatCommand(command)).toBe(command);
    expect(formatCommand('npm ci \\\n  && npm test')).toBe('npm ci \\\n  && npm test');
  });

  it('does_not_touch_quotes_subshells_or_brackets_in_a_long_command', () => {
    const command = `[[ -d node_modules && -f package-lock.json ]] && (cd web; npm ci) && echo "ready: $(date +%T; whoami)" && echo 'a | b'`;
    expect(formatCommand(command)).toBe(['[[ -d node_modules && -f package-lock.json ]]', '  && (cd web; npm ci)', '  && echo "ready: $(date +%T; whoami)"', `  && echo 'a | b'`].join('\n'));
  });

  it('keeps_a_very_long_single_token_whole', () => {
    const token = `https://example.com/${'a'.repeat(400)}`;
    expect(formatCommand(`curl -sSL ${token}`)).toBe(`curl -sSL ${token}`);
    expect(formatCommand(`echo ${'b'.repeat(300)} && ls`)).toBe(`echo ${'b'.repeat(300)}\n  && ls`);
  });

  it('leaves_unbalanced_input_as_is', () => {
    const command = pad(`echo 'unterminated && ls -la /home/user/projects`);
    expect(formatCommand(command)).toBe(command);
  });

  it('normalises_spaces_around_split_operators_but_not_inside_words', () => {
    const command = pad('npm   ci   &&    npm  test  ;   ls');
    expect(formatCommand(command).split('\n')).toEqual(['npm   ci', '  && npm  test;', command.slice(command.indexOf('ls'))]);
  });

  it('trims_trailing_separators_and_whitespace', () => {
    const command = `${pad('cd web && npm ci && npm run build --workspace a')};  `;
    expect(formatCommand(command).endsWith(';')).toBe(true);
    expect(formatCommand(command).split('\n')).toHaveLength(3);
  });
});
