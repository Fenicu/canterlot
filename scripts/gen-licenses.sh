#!/usr/bin/env bash
set -euo pipefail

# Перегенерация перечня лицензий сторонних зависимостей (docs/licenses/THIRD_PARTY.md).
# Не требует сети; использует `cargo metadata --locked` и `web/package-lock.json`.

# shellcheck source=/dev/null
source ~/.cargo/env 2>/dev/null || true
export PATH="$HOME/.cargo/bin:$PATH"
export PYTHONDONTWRITEBYTECODE=1

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [[ "${1:-}" == "--self-test" ]]; then
  exec python3 "$ROOT_DIR/scripts/test-licenses.py"
fi

# Run self-test first to ensure parser integrity
python3 "$ROOT_DIR/scripts/test-licenses.py" -q

mkdir -p docs/licenses

python3 - << 'PY_EOF'
import sys, os, json, subprocess, re
from collections import Counter

sys.path.insert(0, os.path.abspath('scripts'))
from spdx import parse_spdx, ALLOWLIST, rust_allowlist_dependency_phrase

def clean_repo(repo_val):
    if not repo_val:
        return '-'
    if isinstance(repo_val, dict):
        url = repo_val.get('url', '')
    elif isinstance(repo_val, str):
        url = repo_val
    else:
        return '-'
    url = re.sub(r'^git\+', '', url)
    url = re.sub(r'^git://', 'https://', url)
    url = re.sub(r'^ssh://git@github\.com/', 'https://github.com/', url)
    url = re.sub(r'^git@github\.com:', 'https://github.com/', url)
    url = re.sub(r'\.git$', '', url)
    if url.startswith('github:'):
        url = 'https://github.com/' + url[7:]
    if re.match(r'^[a-zA-Z0-9_.-]+/[a-zA-Z0-9_.-]+$', url):
        url = f'https://github.com/{url}'
    return url or '-'

def format_reason(lic_str, unallowed):
    if not lic_str or lic_str == '-':
        return 'Лицензия не указана'
    if unallowed:
        return f'Лицензия вне allowlist: {", ".join(unallowed)}'
    return 'Не удовлетворяет критериям allowlist'

# 1. Cargo metadata
cmd = ['cargo', 'metadata', '--format-version', '1', '--locked']
try:
    proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=True)
    meta = json.loads(proc.stdout)
except Exception as e:
    sys.exit(f'Error running cargo metadata: {e}')

rust_crates = []
for p in meta.get('packages', []):
    name = p['name']
    if name.startswith('canterlot-') or 'spike' in name:
        continue
    version = p['version']
    license_expr = p.get('license') or '-'
    repo = clean_repo(p.get('repository'))
    ok, chosen, unallowed = parse_spdx(license_expr)
    rust_crates.append({
        'name': name,
        'version': version,
        'license': license_expr,
        'chosen': chosen if ok else '—',
        'repository': repo,
        'acceptable': ok,
        'unallowed': unallowed
    })

rust_crates.sort(key=lambda x: (x['name'].lower(), x['version']))

# 2. NPM packages
lock_file = 'web/package-lock.json'
if not os.path.exists(lock_file):
    sys.exit(f'File not found: {lock_file}')

with open(lock_file, 'r', encoding='utf-8') as f:
    lock_data = json.load(f)

npm_runtime = []
npm_dev = []

packages = lock_data.get('packages', {})
for path_key, entry in packages.items():
    if not path_key:
        continue
    name = path_key.split('node_modules/')[-1]
    version = entry.get('version', '')

    pkg_json_path = os.path.join('web', path_key, 'package.json')
    lic = None
    repo_raw = None
    if os.path.exists(pkg_json_path):
        try:
            with open(pkg_json_path, 'r', encoding='utf-8') as pf:
                pj = json.load(pf)
                lic = pj.get('license') or pj.get('licenses')
                repo_raw = pj.get('repository')
        except Exception:
            pass

    if not lic:
        lic = entry.get('license')
    if not lic:
        lic = '-'

    repo = clean_repo(repo_raw)
    is_dev = entry.get('dev', False)
    ok, chosen, unallowed = parse_spdx(lic)

    pkg_info = {
        'name': name,
        'version': version,
        'license': str(lic),
        'chosen': chosen if ok else '—',
        'repository': repo,
        'path': path_key,
        'acceptable': ok,
        'unallowed': unallowed
    }

    if is_dev:
        npm_dev.append(pkg_info)
    else:
        npm_runtime.append(pkg_info)

npm_runtime.sort(key=lambda x: (x['name'].lower(), x['version']))
npm_dev.sort(key=lambda x: (x['name'].lower(), x['version']))

# 3. Summary counts per declared license expression
rust_lic_counts = Counter(x['license'] for x in rust_crates)
npm_run_lic_counts = Counter(x['license'] for x in npm_runtime)
npm_dev_lic_counts = Counter(x['license'] for x in npm_dev)

all_licenses = sorted(set(rust_lic_counts.keys()) | set(npm_run_lic_counts.keys()) | set(npm_dev_lic_counts.keys()))

# 4. Flagged packages (only those with NO acceptable license alternative)
flagged_rust = []
seen_rust = set()
for p in rust_crates:
    if not p['acceptable']:
        key = (p['name'], p['version'])
        if key not in seen_rust:
            seen_rust.add(key)
            flagged_rust.append({
                'name': p['name'],
                'version': p['version'],
                'license': p['license'],
                'reason': format_reason(p['license'], p['unallowed'])
            })

flagged_npm_run = []
seen_run = set()
for p in npm_runtime:
    if not p['acceptable']:
        key = (p['name'], p['version'])
        if key not in seen_run:
            seen_run.add(key)
            flagged_npm_run.append({
                'name': p['name'],
                'version': p['version'],
                'license': p['license'],
                'reason': format_reason(p['license'], p['unallowed'])
            })

flagged_npm_dev = []
seen_dev = set()
for p in npm_dev:
    if not p['acceptable']:
        key = (p['name'], p['version'])
        if key not in seen_dev:
            seen_dev.add(key)
            flagged_npm_dev.append({
                'name': p['name'],
                'version': p['version'],
                'license': p['license'],
                'reason': format_reason(p['license'], p['unallowed'])
            })

# 5. Format markdown
doc = []
doc.append('# Лицензии сторонних зависимостей\n')
doc.append('Данный реестр содержит лицензии сторонних компонентов (Rust-крейтов и npm-пакетов), используемых в Canterlot.')
doc.append('Файл сгенерирован автоматически скриптом `scripts/gen-licenses.sh`.\n')

doc.append('## Сводка по лицензиям\n')
doc.append('| Лицензия | Rust | npm (runtime) | npm (dev) | Всего |')
doc.append('| --- | ---: | ---: | ---: | ---: |')

tot_rust = len(rust_crates)
tot_run = len(npm_runtime)
tot_dev = len(npm_dev)
tot_all = tot_rust + tot_run + tot_dev

for lic in all_licenses:
    rc = rust_lic_counts[lic]
    rnc = npm_run_lic_counts[lic]
    ndc = npm_dev_lic_counts[lic]
    tot = rc + rnc + ndc
    doc.append(f'| `{lic}` | {rc} | {rnc} | {ndc} | {tot} |')

doc.append(f'| **ИТОГО** | **{tot_rust}** | **{tot_run}** | **{tot_dev}** | **{tot_all}** |\n')

# Section: Требуют внимания
doc.append('## Требуют внимания\n')
doc.append('В данном разделе перечислены зависимости, у которых лицензия не указана либо не может быть удовлетворена ни одной лицензией из разрешённого списка (allowlist):')
doc.append('`MIT`, `Apache-2.0`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Zlib`, `Unicode-3.0`, `Unicode-DFS-2016`, `MPL-2.0`, `CC0-1.0`, `0BSD`, `BSL-1.0`, `Unlicense`, `MIT-0` (включая оговорку `WITH LLVM-exception` для `Apache-2.0`).')
doc.append('Для выражений `A OR B` достаточно удовлетворения одной из альтернатив; для `A AND B` обязательно удовлетворение обеих сторон.\n')

if not flagged_rust and not flagged_npm_run and not flagged_npm_dev:
    doc.append('Все зависимости удовлетворяют списку разрешённых лицензий.\n')
else:
    if flagged_rust:
        doc.append('### Rust-крейты\n')
        doc.append('| Крейт | Версия | Лицензия | Причина / примечание |')
        doc.append('| --- | --- | --- | --- |')
        for item in flagged_rust:
            doc.append(f"| `{item['name']}` | {item['version']} | `{item['license']}` | {item['reason']} |")
        doc.append('')
    else:
        doc.append('### Rust-крейты\n')
        phrase = rust_allowlist_dependency_phrase(len(rust_crates))
        doc.append(
            f'Среди Rust-крейтов рабочего пространства пакеты с неразрешёнными лицензиями отсутствуют ({phrase}).\n'
        )

    if flagged_npm_run:
        doc.append('### npm (runtime)\n')
        doc.append('| Пакет | Версия | Лицензия | Причина / примечание |')
        doc.append('| --- | --- | --- | --- |')
        for item in flagged_npm_run:
            doc.append(f"| `{item['name']}` | {item['version']} | `{item['license']}` | {item['reason']} |")
        doc.append('')

    if flagged_npm_dev:
        doc.append('### npm (dev-only, не поставляются)\n')
        doc.append('Пакеты используются исключительно для сборки, линтинга и тестирования; не поставляются пользователю.\n')
        doc.append('| Пакет | Версия | Лицензия | Причина / примечание |')
        doc.append('| --- | --- | --- | --- |')
        for item in flagged_npm_dev:
            doc.append(f"| `{item['name']}` | {item['version']} | `{item['license']}` | {item['reason']} |")
        doc.append('')

# Section: Крейты Rust
doc.append(f'## Крейты Rust\n')
doc.append(f'Зависимости рабочего пространства Cargo (исключая собственные крейты `canterlot-*` и прототипы `spikes`). Всего: {len(rust_crates)}.\n')
doc.append('| Крейт | Версия | Лицензия | Выбрано | Репозиторий |')
doc.append('| --- | --- | --- | --- | --- |')
for p in rust_crates:
    repo_link = f"[{p['repository']}]({p['repository']})" if p['repository'] != '-' else '-'
    doc.append(f"| `{p['name']}` | {p['version']} | `{p['license']}` | `{p['chosen']}` | {repo_link} |")
doc.append('')

# Section: Runtime-пакеты npm
doc.append(f'## Runtime-пакеты npm\n')
doc.append(f'Пакеты веб-интерфейса `web/`, поставляемые в составе клиентского приложения. Всего: {len(npm_runtime)}.\n')
doc.append('| Пакет | Версия | Лицензия | Выбрано | Репозиторий |')
doc.append('| --- | --- | --- | --- | --- |')
for p in npm_runtime:
    repo_link = f"[{p['repository']}]({p['repository']})" if p['repository'] != '-' else '-'
    chosen_str = f"`{p['chosen']}`" if p['chosen'] != '—' else '—'
    doc.append(f"| `{p['name']}` | {p['version']} | `{p['license']}` | {chosen_str} | {repo_link} |")
doc.append('')

# Section: Dev-пакеты npm
doc.append(f'## Пакеты npm для разработки\n')
doc.append(f'Пакеты, используемые только на этапе сборки, линтинга и тестирования (`dev: true`); не поставляются конечным пользователям. Всего записей: {len(npm_dev)}.\n')

seen = set()
for p in npm_dev:
    key = (p['name'], p['version'], p['license'])
    if key in seen:
        continue
    seen.add(key)
    chosen_note = f" (выбрано: `{p['chosen']}`)" if p['acceptable'] and p['chosen'] != p['license'] else ""
    doc.append(f"- `{p['name']}` ({p['version']}) — `{p['license']}`{chosen_note}")
doc.append('')

# Section: Цветовые схемы
doc.append('## Цветовые схемы\n')
doc.append('Палитры `web/src/lib/styles/tokens.css`, взятые из сторонних схем; значения подогнаны под проверки контраста. Остальные палитры оригинальные.\n')
doc.append('| Палитра | Схема | Лицензия | Источник |')
doc.append('| --- | --- | --- | --- |')
doc.append('| «Норд» (`nord`) | Nord, Sven Greb (Arctic Ice Studio) | `MIT` | [github.com/nordtheme/nord](https://github.com/nordtheme/nord) |')
doc.append('')

output_path = 'docs/licenses/THIRD_PARTY.md'
with open(output_path, 'w', encoding='utf-8') as out:
    out.write('\n'.join(doc) + '\n')

print(f'{output_path} updated:')
print(f'  Rust crates: {len(rust_crates)}')
print(f'  npm runtime packages: {len(npm_runtime)}')
print(f'  npm dev-only packages: {len(npm_dev)}')
print(f'  Total packages: {tot_all}')
flagged_total = len(flagged_rust) + len(flagged_npm_run) + len(flagged_npm_dev)
print(f'  Flagged packages: {flagged_total} (Rust: {len(flagged_rust)}, npm runtime: {len(flagged_npm_run)}, npm dev: {len(flagged_npm_dev)})')
PY_EOF

chmod 644 docs/licenses/THIRD_PARTY.md
