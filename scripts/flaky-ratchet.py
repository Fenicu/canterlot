#!/usr/bin/env python3
"""Храповик флейковых паттернов в тестах: база count на правило и файл.

Проверка не пускает новые вхождения известных паттернов в тесты и не даёт
перенести нарушение из одного файла в другой: отдельно считается каждый файл.
База меняется только явной командой update (и только вниз, либо с разрешением
--allow-increase с причиной). Стандартная библиотека, никаких зависимостей.
"""

import argparse
import json
import re
import sys
from pathlib import Path

RULES = (
    'rust_sleep', 'rust_clock', 'rust_ignore',
    'ts_wait', 'ts_setTimeout', 'ts_skip', 'ts_fixme', 'web_retries',
    'nextest_retries',
)

RUST_PATTERNS = {
    'rust_sleep': r'\b(?:tokio::time::sleep|std::thread::sleep)\s*\(',
    'rust_clock': r'\b(?:Instant|SystemTime)::now\s*\(\s*\)',
    'rust_ignore': r'#\s*\[\s*ignore\s*\]',
}
TS_PATTERNS = {
    'ts_wait': r'\bpage\.waitForTimeout\s*\(',
    'ts_setTimeout': r'(?<!test\.)\bsetTimeout\s*\(',
}

CFG_TEST_RE = re.compile(r'#\s*\[\s*cfg\s*\(\s*(?:all\s*\(\s*)?test\b')
SKIP_RE = re.compile(r'\btest\s*\.\s*(skip|fixme)\s*\(')
WEB_RETRIES_RE = re.compile(r'\bretries\s*[:=]\s*[1-9]\d*')
NEXTEST_HEADER_RE = re.compile(r'^\s*\[\[.*overrides\]\]')
NEXTEST_SECTION_RE = re.compile(r'^\s*\[')
NEXTEST_RETRIES_RE = re.compile(r'^\s*retries\s*=\s*[1-9]\d*')


def _blank_quoted(text, out, start, quote):
    """Закрасить строку до закрывающей кавычки, сохранив переводы строк."""
    i = start + 1
    out[start] = ' '
    n = len(text)
    while i < n:
        ch = text[i]
        if ch == '\n':
            break
        if ch == '\\' and i + 1 < n and text[i + 1] != '\n':
            out[i] = out[i + 1] = ' '
            i += 2
            continue
        out[i] = ' '
        i += 1
        if ch == quote:
            break
    return i


def strip_noise(text, lang):
    """Заменить комментарии и литералы пробелами, сохранив позиции символов."""
    out = list(text)
    n = len(text)
    i = 0
    in_block = False
    while i < n:
        ch = text[i]
        if ch == '\n':
            i += 1
            continue
        if in_block:
            if text.startswith('*/', i):
                out[i] = out[i + 1] = ' '
                i += 2
                in_block = False
            else:
                out[i] = ' '
                i += 1
            continue
        if text.startswith('//', i):
            while i < n and text[i] != '\n':
                out[i] = ' '
                i += 1
            continue
        if text.startswith('/*', i):
            out[i] = out[i + 1] = ' '
            i += 2
            in_block = True
            continue
        if ch == '#' and lang == 'toml':
            while i < n and text[i] != '\n':
                out[i] = ' '
                i += 1
            continue
        if ch == 'r' and lang == 'rust':
            raw = re.match(r'r(#+)?["\']', text[i:])
            if raw:
                hashes = raw.group(1) or ''
                closer = raw.group(0)[-1] + hashes
                end = text.find(closer, i + len(raw.group(0)))
                end = n if end < 0 else end + len(closer)
                for k in range(i, end):
                    if text[k] != '\n':
                        out[k] = ' '
                i = end
                continue
        if ch == '"' or (ch in ("'", '`') and lang != 'rust') or (ch == "'" and lang == 'rust'
                                                                  and re.match(r"'(?:\\.|[^'\\\n])'", text[i:])):
            i = _blank_quoted(text, out, i, ch)
            continue
        i += 1
    return ''.join(out)


def rust_test_mask(lines, clean):
    """Номера строк (с нуля), входящих в модули #[cfg(test)]."""
    mask = set()
    i = 0
    n = len(lines)
    while i < n:
        if CFG_TEST_RE.search(clean[i]):
            depth = 0
            started = False
            j = i
            while j < n:
                for ch in clean[j]:
                    if ch == '{':
                        depth += 1
                        started = True
                    elif ch == '}':
                        depth -= 1
                if started:
                    mask.add(j)
                    if depth <= 0:
                        break
                elif j > i + 6:
                    break
                j += 1
            i = j + 1
            continue
        i += 1
    return mask


def _has_reason(orig, start):
    """Есть ли у skip/fixme строковый аргумент-причина после первого аргумента."""
    depth = 0
    i = start
    quote = None
    while i < len(orig):
        ch = orig[i]
        if quote:
            if ch == '\\':
                i += 2
                continue
            if ch == quote:
                quote = None
            i += 1
            continue
        if ch in "'\"`":
            quote = ch
            i += 1
            continue
        if ch in '([{':
            depth += 1
        elif ch in ')]}':
            if depth == 0:
                return False
            depth -= 1
        elif ch == ',' and depth == 0:
            return bool(re.search(r'''['"`]''', orig[i + 1:]))
        i += 1
    return False


def _has_quarantine_above(lines, idx, marker):
    """Блок комментариев непосредственно над строкой содержит QUARANTINE."""
    j = idx - 1
    while j >= 0 and not lines[j].strip():
        j -= 1
    seen = False
    while j >= 0 and lines[j].lstrip().startswith(marker):
        if 'QUARANTINE' in lines[j]:
            seen = True
        j -= 1
    return seen


def scan_file(rel, text):
    """Слово rule -> список номеров строк (с единицы) с нарушениями."""
    lines = text.splitlines()
    lang = 'rust' if rel.endswith('.rs') else ('toml' if rel.endswith('.toml') else 'ts')
    clean = strip_noise(text, lang).splitlines()
    found = {}

    def add(rule, idx):
        found.setdefault(rule, []).append(idx + 1)

    if lang == 'rust':
        mask = None if re.search(r'(^|/)tests/', rel) else rust_test_mask(lines, clean)
        for idx, line in enumerate(clean):
            if mask is not None and idx not in mask:
                continue
            for rule, pattern in RUST_PATTERNS.items():
                if re.search(pattern, line):
                    add(rule, idx)
        return found

    if lang == 'toml':
        i = 0
        while i < len(clean):
            if NEXTEST_HEADER_RE.match(clean[i]):
                covered = _has_quarantine_above(lines, i, '#')
                j = i + 1
                while j < len(clean) and not NEXTEST_SECTION_RE.match(clean[j]):
                    if NEXTEST_RETRIES_RE.match(clean[j]) and not covered:
                        add('nextest_retries', j)
                    j += 1
                i = j
                continue
            i += 1
        return found

    for idx, line in enumerate(clean):
        for rule, pattern in TS_PATTERNS.items():
            if re.search(pattern, line):
                add(rule, idx)
        for match in SKIP_RE.finditer(line):
            if not _has_reason(lines[idx], match.end()):
                add('ts_skip' if match.group(1) == 'skip' else 'ts_fixme', idx)
        if WEB_RETRIES_RE.search(line) and not _has_quarantine_above(lines, idx, '//'):
            add('web_retries', idx)
    return found


def scan_tree(root):
    """Просканировать дерево: путь относительно root -> rule -> строки."""
    root = Path(root)
    files = []
    crates = root / 'crates'
    if crates.is_dir():
        files += sorted(crates.glob('*/tests/**/*.rs'))
        files += sorted(crates.glob('*/src/**/*.rs'))
    for sub in ('tests',):
        directory = root / 'web' / sub
        if directory.is_dir():
            files += sorted(directory.rglob('*.ts'))
            files += sorted(directory.rglob('*.tsx'))
    config = root / '.config' / 'nextest.toml'
    if config.is_file():
        files.append(config)
    result = {}
    for path in files:
        rel = path.relative_to(root).as_posix()
        found = scan_file(rel, path.read_text(encoding='utf-8', errors='replace'))
        if found:
            result[rel] = found
    return result


def load_baseline(path):
    if not Path(path).exists():
        return None
    return json.loads(Path(path).read_text(encoding='utf-8'))


def save_baseline(path, data):
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(
        json.dumps(data, ensure_ascii=False, indent=2, sort_keys=True) + '\n',
        encoding='utf-8',
    )


def compare(current, counts):
    """Вернуть (росты, уменьшения): (файл, правило, база, сейчас, строки)."""
    increases = []
    decreases = []
    for file in sorted(set(current) | set(counts)):
        now_rules = current.get(file, {})
        base_rules = counts.get(file, {})
        for rule in sorted(set(now_rules) | set(base_rules)):
            lines = now_rules.get(rule, [])
            now = len(lines)
            base = base_rules.get(rule, 0)
            if now > base:
                increases.append((file, rule, base, now, lines))
            elif now < base:
                decreases.append((file, rule, base, now))
    return increases, decreases


def build_update(current, baseline, grants):
    """Новая база и список неразрешённых ростов."""
    counts = baseline.get('counts', {}) if baseline else {}
    allowances = baseline.get('allowances', {}) if baseline else {}
    new_counts = {}
    new_allowances = {}
    errors = []
    for file in sorted(current):
        for rule in sorted(current[file]):
            lines = current[file][rule]
            now = len(lines)
            base = counts.get(file, {}).get(rule, 0)
            if now <= base:
                if now:
                    new_counts.setdefault(file, {})[rule] = now
                continue
            grant = grants.get(file, {}).get(rule)
            existing = allowances.get(file, {}).get(rule)
            if grant or existing:
                new_counts.setdefault(file, {})[rule] = now
                reason = grant['reason'] if grant else existing['reason']
                new_allowances.setdefault(file, {})[rule] = {'count': now, 'reason': reason}
            else:
                errors.append((file, rule, base, now))
    for file, rules in allowances.items():
        for rule, entry in rules.items():
            if file in new_counts and rule in new_counts[file] and rule not in new_allowances.get(file, {}):
                new_allowances.setdefault(file, {})[rule] = entry
    return {'version': 1, 'counts': new_counts, 'allowances': new_allowances}, errors


def source_line(root, file, number):
    path = Path(root) / file
    if not path.is_file():
        return ''
    lines = path.read_text(encoding='utf-8', errors='replace').splitlines()
    return lines[number - 1].strip() if 0 < number <= len(lines) else ''


def cmd_check(args):
    baseline = load_baseline(args.baseline)
    if baseline is None:
        print('база отсутствует: python3 scripts/flaky-ratchet.py update')
        return 1
    current = scan_tree(args.root)
    increases, decreases = compare(current, baseline.get('counts', {}))
    total = sum(len(lines) for rules in current.values() for lines in rules.values())
    for file, rule, base, now, lines in increases:
        print(f'РОСТ: {file}: {rule}: {now} > {base}')
        for number in lines:
            print(f'  строка {number}: {source_line(args.root, file, number)}')
    for file, rule, base, now in decreases:
        print(f'МЕНЬШЕ: {file}: {rule}: {now} < {base} — обновите базу: '
              'python3 scripts/flaky-ratchet.py update')
    if increases:
        print(f'храповик: нарушений {len(increases)}, совпадений {total}')
        return 1
    print(f'храповик: нарушений нет, совпадений {total}')
    return 0


def cmd_update(args):
    baseline = load_baseline(args.baseline)
    grants = {}
    for file, rule, reason in args.allow_increase or ():
        grants.setdefault(file, {})[rule] = {'reason': reason}
    current = scan_tree(args.root)
    errors = []
    if baseline is None:
        counts = {
            file: {rule: len(lines) for rule, lines in rules.items() if lines}
            for file, rules in current.items()
        }
        new = {'version': 1, 'counts': counts, 'allowances': {}}
    else:
        new, errors = build_update(current, baseline, grants)
    if errors:
        for file, rule, base, now in errors:
            print(f'НУЖНО РАЗРЕШЕНИЕ: {file}: {rule}: {now} > {base}')
        return 1
    save_baseline(args.baseline, new)
    entries = sum(len(rules) for rules in new['counts'].values())
    verb = 'создана' if baseline is None else 'обновлена'
    print(f'база {verb}: {len(new["counts"])} файлов, {entries} записей')
    return 0


def main(argv=None):
    default_root = str(Path(__file__).resolve().parent.parent)
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    for name in ('check', 'update'):
        command = commands.add_parser(name)
        command.add_argument('--root', default=default_root)
        command.add_argument(
            '--baseline', default=str(Path(__file__).resolve().parent / 'flaky-ratchet.baseline.json')
        )
        if name == 'update':
            command.add_argument(
                '--allow-increase', nargs=3, action='append', default=[],
                metavar=('FILE', 'RULE', 'REASON'),
            )
    args = parser.parse_args(argv)
    return cmd_check(args) if args.command == 'check' else cmd_update(args)


if __name__ == '__main__':
    sys.exit(main())
