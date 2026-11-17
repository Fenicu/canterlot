#!/usr/bin/env bash
# Быстрая проверка выбора тестов Playwright по расположению file:line: без браузера
# и без прогона тестов, только `playwright test --list`. Гейт выбирает упавшие тесты
# именно так (file:line + --project), потому что --grep сверяется со всей строкой
# «проект файл describe заголовок теги» и не годится для точного заголовка.
# Проверяем на настоящих спеках e2e/ui: вложенный в describe тест, заголовок с « » и
# обычный тест; сгруппированные селекторы должны вывести ровно тесты этих расположений.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
cd web
[[ -x node_modules/.bin/playwright ]] || {
    echo 'НУЖЕН web/node_modules: сначала npm ci в web/' >&2
    exit 1
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

npx --no-install playwright test e2e/ui --list >"$tmp/all" 2>/dev/null

# Выбираем три настоящие идентичности и пишем селекторы, сгруппированные по проекту.
python3 - "$tmp/all" "$tmp/chosen" "$tmp/selectors" <<'PY'
import re
import sys

pattern = re.compile(r'^\s+\[([^\]]+)\] › ([^ ›]+) › (.+)$')
entries = []
for raw in open(sys.argv[1]):
    match = pattern.match(raw.rstrip('\n'))
    if not match:
        continue
    project, loccol, title = match.groups()
    location = loccol.rpartition(':')[0]
    entries.append((project, location, title, raw.rstrip('\n')))
used = set()


def pick(predicate):
    for entry in entries:
        project, location, title, _ = entry
        if (project, location) in used:
            continue
        if predicate(entry):
            used.add((project, location))
            return entry
    return None


nested = pick(lambda e: ' › ' in e[2])
special = pick(lambda e: '«' in e[2] or '»' in e[2])
plain = pick(lambda e: ' › ' not in e[2] and '«' not in e[2] and '»' not in e[2])
chosen = [item for item in (nested, special, plain) if item]
if len(chosen) != 3:
    sys.exit('не нашлись три подходящих теста e2e/ui с разными расположениями')
with open(sys.argv[2], 'w') as out:
    for project, location, title, _ in chosen:
        out.write(f'{project}|{location}|{title}\n')
with open(sys.argv[3], 'w') as out:
    for project, location in sorted({(p, l) for p, l, _, _ in chosen}):
        out.write(f'{project}|{location}\n')
for project, location, title, _ in chosen:
    print(f'  выбран: {project} | {location} | {title}')
PY

declare -A by_project=()
while IFS='|' read -r project location; do
    by_project[$project]+="${by_project[$project]:+ }${location}"
done <"$tmp/selectors"
: >"$tmp/selected"
for project in "${!by_project[@]}"; do
    read -r -a selectors <<<"${by_project[$project]}"
    npx --no-install playwright test "${selectors[@]}" "--project=$project" --list >>"$tmp/selected" 2>/dev/null
done

python3 - "$tmp/all" "$tmp/selected" "$tmp/selectors" "$tmp/chosen" <<'PY'
import re
import sys

pattern = re.compile(r'^\s+\[([^\]]+)\] › ([^ ›]+) › (.+)$')


def parse(path):
    result = []
    for raw in open(path):
        match = pattern.match(raw.rstrip('\n'))
        if match:
            project, loccol, title = match.groups()
            result.append((project, loccol.rpartition(':')[0], title))
    return result


selected_locations = set()
for line in open(sys.argv[3]):
    project, location = line.rstrip('\n').split('|')
    selected_locations.add((project, location))
expected = [item for item in parse(sys.argv[1]) if (item[0], item[1]) in selected_locations]
actual = parse(sys.argv[2])
if sorted(expected) != sorted(actual):
    print('ОЖИДАЛОСЬ:', *sorted(expected), sep='\n  ', file=sys.stderr)
    print('ПОЛУЧЕНО:', *sorted(actual), sep='\n  ', file=sys.stderr)
    sys.exit('селекторы file:line вывели не те тесты')
chosen = [tuple(line.rstrip('\n').split('|')) for line in open(sys.argv[4])]
for identity in chosen:
    if identity not in actual:
        sys.exit(f'выбранный тест не попал в выборку: {identity}')
PY
printf 'Выбор Playwright по file:line: %s теста(ов) из %s расположений, ровно как задумано\n' "$(wc -l <"$tmp/chosen" | tr -d ' ')" "$(wc -l <"$tmp/selectors" | tr -d ' ')"
