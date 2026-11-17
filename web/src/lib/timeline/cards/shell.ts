/**
 * Команда оболочки для чтения с телефона. Модели пишут цепочку в одну строку; длинную строку
 * лента переносит по `&&`, `||`, `;`, `&` и `|` верхнего уровня. Внутри кавычек, `$(…)`,
 * обратных кавычек, `[[ ]]`, `${…}`, `$((…))`, подоболочек и составных команд (`if … fi`,
 * `for … done`, `{ … }`) переносов нет. Многострочная команда (heredoc, ручные `\`) остаётся
 * как есть: её разметку автор уже выбрал. Копируется всегда исходная строка.
 */

/** Ширина, после которой строка переносится: столько знаков моноширинного шрифта видно на телефоне. */
export const SHELL_WRAP = 48;

export type ShellOp = '' | '&&' | '||' | ';' | '&' | '|' | '|&';

/** Часть команды и оператор перед ней; у первой части оператора нет. */
export interface ShellPiece {
  op: ShellOp;
  text: string;
}

type Context = 'squote' | 'dquote' | 'backtick' | 'subst' | 'subshell' | 'arith' | 'param' | 'test';

/** Слова, после которых снова идёт команда. */
const LEADS = new Set(['then', 'do', 'else', 'elif', 'if', 'while', 'until', '!', '{', 'time']);
const OPENS = new Set(['if', 'for', 'while', 'until', 'select', 'case', '{']);
const CLOSES = new Set(['fi', 'done', 'esac', '}']);
const WORD_END = /[\s;&|()<>]/;

/**
 * Части команды на верхнем уровне. `null` — кавычка или скобка не закрыта: такую строку
 * лента не трогает. Комментарий и heredoc обрывают разбор: дальше всё идёт одной частью.
 */
export function splitCommand(command: string): ShellPiece[] | null {
  const stack: Context[] = [];
  /** Глубина арифметики: свои скобки внутри `$((…))`. */
  const parens: number[] = [];
  const cuts: { at: number; end: number; op: ShellOp }[] = [];
  let depth = 0;
  let commandStart = true;
  let stopped = false;
  let i = 0;
  const top = () => stack[stack.length - 1];
  const code = () => stack.length === 0 || top() === 'subst' || top() === 'subshell';
  const wordStart = (at: number) => at === 0 || WORD_END.test(command[at - 1]!);

  while (i < command.length && !stopped) {
    const ch = command[i]!;
    const next = command[i + 1] ?? '';
    const ctx = top();

    if (ctx === 'squote') {
      if (ch === "'") stack.pop();
      i += 1;
      continue;
    }
    if (ch === '\\') {
      i += 2;
      continue;
    }
    if (ctx === 'backtick') {
      if (ch === '`') stack.pop();
      i += 1;
      continue;
    }
    if (ctx === 'dquote') {
      if (ch === '"') stack.pop();
      else if (ch === '$' && next === '(' && command[i + 2] === '(') {
        stack.push('arith');
        parens.push(0);
        i += 3;
        continue;
      } else if (ch === '$' && next === '(') {
        stack.push('subst');
        i += 2;
        continue;
      } else if (ch === '$' && next === '{') {
        stack.push('param');
        i += 2;
        continue;
      } else if (ch === '`') stack.push('backtick');
      i += 1;
      continue;
    }

    // Дальше — контексты, где кавычки и подстановки открываются заново.
    if (ch === "'") {
      stack.push('squote');
      i += 1;
      continue;
    }
    if (ch === '"') {
      stack.push('dquote');
      i += 1;
      continue;
    }
    if (ch === '`') {
      stack.push('backtick');
      i += 1;
      continue;
    }
    if (ch === '$' && next === '(' && command[i + 2] === '(') {
      stack.push('arith');
      parens.push(0);
      i += 3;
      continue;
    }
    if (ch === '$' && next === '(') {
      stack.push('subst');
      i += 2;
      continue;
    }
    if (ch === '$' && next === '{') {
      stack.push('param');
      i += 2;
      continue;
    }
    if (ctx === 'param') {
      if (ch === '}') stack.pop();
      i += 1;
      continue;
    }
    if (ctx === 'arith') {
      const level = parens.length - 1;
      if (ch === '(') parens[level]! += 1;
      else if (ch === ')' && parens[level]! > 0) parens[level]! -= 1;
      else if (ch === ')' && next === ')') {
        stack.pop();
        parens.pop();
        i += 2;
        continue;
      }
      i += 1;
      continue;
    }
    if (ctx === 'test') {
      if (ch === ']' && next === ']' && wordStart(i) && (i + 2 >= command.length || WORD_END.test(command[i + 2]!))) {
        stack.pop();
        i += 2;
        continue;
      }
      i += 1;
      continue;
    }

    // Код: верхний уровень, подоболочка или `$(…)`.
    if (ch === '#' && wordStart(i)) {
      if (stack.length === 0) stopped = true;
      else {
        const end = command.indexOf('\n', i);
        i = end < 0 ? command.length : end;
      }
      continue;
    }
    if (ch === '[' && next === '[' && wordStart(i) && /\s/.test(command[i + 2] ?? '')) {
      stack.push('test');
      i += 2;
      continue;
    }
    if (ch === '(') {
      stack.push('subshell');
      commandStart = true;
      i += 1;
      continue;
    }
    if (ch === ')') {
      // Скобка без пары бывает в шаблоне `case`: она ничего не закрывает.
      if (ctx === 'subst' || ctx === 'subshell') stack.pop();
      commandStart = true;
      i += 1;
      continue;
    }
    if (ch === '<' && next === '<' && command[i + 2] !== '<') {
      if (stack.length === 0) stopped = true;
      i += 2;
      continue;
    }
    if (ch === '<' && next === '<') {
      i += 3;
      continue;
    }
    if (!code()) {
      i += 1;
      continue;
    }

    const prev = command[i - 1] ?? '';
    const top0 = stack.length === 0 && depth === 0;
    if (ch === '&' && next === '&') {
      if (top0) cuts.push({ at: i, end: i + 2, op: '&&' });
      commandStart = true;
      i += 2;
      continue;
    }
    if (ch === '|' && next === '|') {
      if (top0) cuts.push({ at: i, end: i + 2, op: '||' });
      commandStart = true;
      i += 2;
      continue;
    }
    if (ch === '|' && next === '&') {
      if (top0) cuts.push({ at: i, end: i + 2, op: '|&' });
      commandStart = true;
      i += 2;
      continue;
    }
    if (ch === '|') {
      // `>|` — перенаправление с перезаписью, а не конвейер.
      if (top0 && prev !== '>') cuts.push({ at: i, end: i + 1, op: '|' });
      commandStart = prev !== '>';
      i += 1;
      continue;
    }
    if (ch === ';' && next === ';') {
      commandStart = true;
      i += 2;
      continue;
    }
    if (ch === ';') {
      if (top0) cuts.push({ at: i, end: i + 1, op: ';' });
      commandStart = true;
      i += 1;
      continue;
    }
    if (ch === '&') {
      // `2>&1`, `&>`, `<&3` — перенаправления, а не фоновый запуск.
      const redirect = prev === '>' || prev === '<' || next === '>';
      if (top0 && !redirect) cuts.push({ at: i, end: i + 1, op: '&' });
      commandStart = !redirect;
      i += 1;
      continue;
    }
    if (/\s/.test(ch)) {
      i += 1;
      continue;
    }

    // Слово: ключевые слова считаются только там, где ждут команду.
    let end = i;
    while (end < command.length && !/[\s;&|()<>'"`$\\]/.test(command[end]!)) end += 1;
    const word = command.slice(i, end);
    if (end === i) {
      i += 1;
      commandStart = false;
      continue;
    }
    const plain = end >= command.length || WORD_END.test(command[end]!);
    if (stack.length === 0 && commandStart && plain) {
      if (OPENS.has(word)) depth += 1;
      else if (CLOSES.has(word) && depth > 0) depth -= 1;
    }
    commandStart = plain && LEADS.has(word);
    i = end;
  }

  if (!stopped && stack.length > 0) return null;
  const pieces: ShellPiece[] = [];
  let from = 0;
  let op: ShellOp = '';
  for (const cut of cuts) {
    pieces.push({ op, text: command.slice(from, cut.at).trim() });
    op = cut.op;
    from = cut.end;
  }
  const rest = command.slice(from).trim();
  if (rest || cuts.length > 0) pieces.push({ op, text: rest });
  return pieces;
}

function isList(op: ShellOp): boolean {
  return op === '' || op === '&&' || op === '||' || op === ';' || op === '&';
}

function joinStages(stages: ShellPiece[]): string {
  return stages.map((stage, index) => (index === 0 ? stage.text : `${stage.op} ${stage.text}`)).join(' ');
}

/** Команда для показа: длинная строка разложена по операторам верхнего уровня. */
export function formatCommand(command: string, width = SHELL_WRAP): string {
  const text = command.trim();
  if (text.includes('\n')) return command.replace(/\s+$/, '');
  if (text.length <= width) return text;
  const pieces = splitCommand(text);
  if (!pieces || pieces.length < 2) return text;

  const items: { op: ShellOp; stages: ShellPiece[] }[] = [];
  for (const piece of pieces) {
    if (isList(piece.op) || items.length === 0) items.push({ op: piece.op, stages: [piece] });
    else items[items.length - 1]!.stages.push(piece);
  }

  const lines: string[] = [];
  for (const item of items) {
    let indent = 0;
    let prefix = '';
    if (item.op === ';' || item.op === '&') {
      if (lines.length > 0) lines[lines.length - 1] += item.op === ';' ? ';' : ' &';
    } else if (item.op === '&&' || item.op === '||') {
      indent = 2;
      prefix = `${item.op} `;
    }
    const first = item.stages[0]!;
    if (!first.text && item.stages.length === 1) continue;
    const pipeline = joinStages(item.stages);
    const pad = ' '.repeat(indent);
    if (item.stages.length > 1 && pipeline.length > width) {
      lines.push(`${pad}${prefix}${first.text}`);
      for (const stage of item.stages.slice(1)) lines.push(`${pad}  ${stage.op} ${stage.text}`);
    } else {
      lines.push(`${pad}${prefix}${pipeline}`);
    }
  }
  return lines.join('\n');
}
