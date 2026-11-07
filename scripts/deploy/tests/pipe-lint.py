#!/usr/bin/env python3
"""Статическая проверка скриптов выката на классы ошибок, которые уже роняли прод.

Правила (код — в выводе, подавление — комментарий `# lint: <код>-ok <причина>`
на той же строке):

  sigpipe       `cmd | head|grep -q|grep -m|sed …q|awk …exit` под pipefail: ранний
                выход потребителя убивает производителя SIGPIPE (141), и pipefail
                превращает успех в провал (урок 1.19.0).
  wait-subst    незащищённое `VAR="$(…)"` в теле цикла ожидания (цикл со sleep)
                под set -e: первый пустой опрос молча обрывает прогон (1.18.0, 1.18.1).
  test-break    `[ … ] && break|continue` или `[ … ] && cmd` последней командой тела
                цикла: ложный тест становится статусом цикла (1.17.0, 1.18.0).
  source-exec   `source` исполняемого файла или точки входа выката: исполняемый
                скрипт выполняется целиком (урок 1.16.1: source cutover остановил воркер).
  self-match    `pgrep -f`, `ps … | grep <литерал без [x]>` и подстрока
                `*canterlot-worker*` по склеенной командной строке: скан ловит
                собственную обёртку (урок 1.16.0).
  run-write     пишущая команда в скриптах выката не через run_write/run_write_pipe:
                репетиция обязана напечатать каждую запись, а не выполнить её.

Использование: pipe-lint.py [--prod] [ФАЙЛ…]; без аргументов — scripts/deploy/*.sh и tests/*.sh.
Код выхода 1, если есть нарушения.
"""
import os
import re
import stat
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DEPLOY = os.path.dirname(HERE)
REPO = os.path.dirname(os.path.dirname(DEPLOY))

PROD = ["cutover.sh", "lib.sh", "verify.sh", "retain.sh", "preflight.sh",
        "target-check.sh", "stage.sh", "rollback-db.sh"]
ENTRYPOINTS = {"cutover.sh", "verify.sh", "retain.sh", "preflight.sh",
               "target-check.sh", "stage.sh", "rollback-db.sh"}
# Функции, которые сами по себе пишут: их вызов обязан идти через run_write, а
# тела (определения) не проверяются.
WRITER_FUNCS = {"hub_locked", "web_locked", "worker_locked", "reserve_releases",
                "unreserve_releases", "recv_to", "run_write", "run_write_pipe",
                "_rehearse_note"}


def mask(text):
    """Вернуть текст той же длины: содержимое кавычек -> 'x' (код в $(…) внутри
    двойных кавычек остаётся видимым), комментарии и тела heredoc -> пробелы."""
    out = list(text)
    i, n = 0, len(text)
    stack = ["N"]          # N — код, S — '…', D — "…", C — $( … ) с глубиной скобок
    depth = [0]
    heredocs = []          # ожидающие тела: (разделитель, с табами)
    line_start = True
    while i < n:
        c = text[i]
        top = stack[-1]
        if c == "\n":
            if heredocs and top in ("N", "C"):
                # Тела heredoc начинаются со следующей строки.
                j = i + 1
                while heredocs:
                    delim, dash = heredocs.pop(0)
                    while j < n:
                        k = text.find("\n", j)
                        if k < 0:
                            k = n
                        body = text[j:k]
                        cmp = body.lstrip("\t") if dash else body
                        for p in range(j, k):
                            out[p] = " "
                        j = k + 1
                        if cmp == delim:
                            break
                i = j
                line_start = True
                continue
            i += 1
            line_start = True
            continue
        if top in ("N", "C"):
            if c == "#" and (line_start or text[i - 1] in " \t;|&(") :
                k = text.find("\n", i)
                k = n if k < 0 else k
                for p in range(i, k):
                    out[p] = " "
                i = k
                continue
            if c == "\\" and i + 1 < n:
                i += 2
                line_start = False
                continue
            if c == "'":
                stack.append("S")
            elif c == '"':
                stack.append("D")
            elif text.startswith("$(", i) and not text.startswith("$((", i):
                stack.append("C")
                depth.append(0)
                i += 2
                line_start = False
                continue
            elif text.startswith("<<", i) and not text.startswith("<<<", i):
                m = re.match(r"<<(-?)\s*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\2", text[i:])
                if m:
                    heredocs.append((m.group(3), m.group(1) == "-"))
                    i += m.end()
                    continue
            elif top == "C" and c == "(":
                depth[-1] += 1
            elif top == "C" and c == ")":
                if depth[-1] == 0:
                    stack.pop()
                    depth.pop()
                else:
                    depth[-1] -= 1
            if c not in " \t":
                line_start = False
            i += 1
            continue
        if top == "S":
            if c == "'":
                stack.pop()
            else:
                out[i] = "x"
            i += 1
            continue
        if top == "D":
            if c == "\\" and i + 1 < n:
                out[i] = "x"
                if text[i + 1] != "\n":
                    out[i + 1] = "x"
                i += 2
                continue
            if c == '"':
                stack.pop()
                i += 1
                continue
            if text.startswith("$(", i) and not text.startswith("$((", i):
                stack.append("C")
                depth.append(0)
                i += 2
                continue
            out[i] = "x"
            i += 1
            continue
    return "".join(out)


def logical_lines(orig, masked):
    """Склеить продолжения строк (\\ в конце). Возврат: [(номер, ориг, маска)]."""
    o_lines = orig.split("\n")
    m_lines = masked.split("\n")
    res = []
    i = 0
    while i < len(o_lines):
        start = i
        o, m = o_lines[i], m_lines[i]
        while m.rstrip().endswith("\\") and i + 1 < len(o_lines):
            i += 1
            o += " " + o_lines[i]
            m += " " + m_lines[i]
        res.append((start + 1, o, m))
        i += 1
    return res


def suppressed(orig, code):
    return re.search(r"#\s*lint:.*\b" + re.escape(code) + r"-ok\b", orig) is not None


SIGPIPE_STAGE = re.compile(
    r"^\s*(?:head\b|grep\s+(?:-[A-Za-z]*[qlmL][A-Za-z]*\b|--quiet|--max-count|--files-with-matches)"
    r"|sed\b[^|]*?(?:^|[;'\" ])\d*q\b|sed\s+-n\s+['\"]?\d+q|awk\b.*\bexit\b)")


def pipe_stages(m):
    """Позиции одиночных | вне кавычек в маскированной строке."""
    pos = []
    for k, c in enumerate(m):
        if c == "|" and (k + 1 >= len(m) or m[k + 1] != "|") and (k == 0 or m[k - 1] != "|"):
            pos.append(k)
    return pos


def stage_text(o, m, start):
    end = len(m)
    for k in range(start, len(m)):
        if m[k] in "|;)" or m.startswith("&&", k):
            end = k
            break
    return o[start:end]


def loops(lines):
    """Тела циклов: список (индекс строки открытия, индекс строки done)."""
    stack, res = [], []
    for idx, (_, _, m) in enumerate(lines):
        words = re.findall(r"(?<![\w$-])(do|done)(?![\w-])", m)
        for w in words:
            if w == "do":
                stack.append(idx)
            elif stack:
                res.append((stack.pop(), idx))
    return res


def functions(lines):
    """Пары (имя, первая строка, последняя строка) по балансу фигурных скобок."""
    res = []
    idx = 0
    while idx < len(lines):
        m = lines[idx][2]
        fm = re.match(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*\(\)\s*\{", m)
        if fm:
            bal = 0
            for j in range(idx, len(lines)):
                bal += lines[j][2].count("{") - lines[j][2].count("}")
                if bal <= 0:
                    res.append((fm.group(1), idx, j))
                    break
        idx += 1
    return res


CMD_POS = r"(?:^|[;&|({!]|\bthen\b|\bdo\b|\belse\b|\$\(|\|\||&&)\s*"
WRITE_CMD = re.compile(
    CMD_POS + r"(?P<cmd>systemctl\s+(?:--user\s+)?(?:start|stop|restart|enable|disable|mask|unmask|daemon-reload)\b"
    r"|ln\s|mv\s|rm\s|cp\s|chmod\s|chown\s|mkdir\s|install\s|rsync\s|tar\s+-?[a-z]*x"
    r"|docker\s+(?:compose\s+up|rm|stop|restart|kill)\b|kill\s|dropdb\b|createdb\b|pg_dump\b|vzdump\b"
    r"|pg_restore\b(?![^;|&]*(?:--list|-f\s+/dev/null))"
    r"|(?:hub_locked|web_locked|worker_locked|reserve_releases|unreserve_releases|recv_to)\b)")
SSH_CMD = re.compile(CMD_POS + r"(?P<cmd>ssh_(?:hub|web|pmx)\b)")
REMOTE_WRITE = re.compile(
    r"(?<![\w-])(?:rm|mv|ln|cp|chmod|chown|install|mkdir|tee|pg_dump|vzdump|dropdb|createdb)\s"
    r"|systemctl\s+(?:restart|start|stop)|docker\s+compose\s+up|[^2&]>\s*(?!/dev/null)['\"]?[/$]")
RUN_WRITE_PREFIX = re.compile(r"run_write(?:_pipe)?\s+\"x*\"\s*$")


def check_file(path, problems, force_prod=False):
    name = os.path.basename(path)
    orig = open(path, encoding="utf-8").read()
    masked = mask(orig)
    lines = logical_lines(orig, masked)
    is_prod = force_prod or (os.path.dirname(os.path.abspath(path)) == DEPLOY and name in PROD)

    def report(lineno, code, msg, o):
        if not suppressed(o, code):
            problems.append(f"{path}:{lineno}: [{code}] {msg}")

    # source-exec — во всех скриптах (прод и тесты).
    for lineno, o, m in lines:
        sm = re.match(r"^\s*(?:source|\.)\s+(\S+)", m)
        if not sm:
            continue
        target = sm.group(1).strip('"')
        cand = None
        tgt_o = o[sm.start(1):].split(" #")[0].strip()
        if len(tgt_o) >= 2 and tgt_o[0] == tgt_o[-1] == '"':
            tgt_o = tgt_o[1:-1]
        for var in ("${SCRIPT_DIR}", "${DEPLOY_DIR}", "${HARNESS_DIR}", '$(dirname "${BASH_SOURCE[0]}")'):
            if tgt_o.startswith(var):
                base = os.path.dirname(os.path.abspath(path))
                if var == "${DEPLOY_DIR}":
                    base = DEPLOY
                cand = base + tgt_o[len(var):]
        if cand is None and tgt_o.startswith("scripts/"):
            cand = os.path.join(REPO, tgt_o)
        if cand is None:
            continue
        if os.path.basename(cand) in ENTRYPOINTS:
            report(lineno, "source-exec", f"sources the entry point {os.path.basename(cand)}: execute it instead", o)
        elif os.path.exists(cand) and os.stat(cand).st_mode & (stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH):
            report(lineno, "source-exec", f"sources executable {cand}: libraries must not be executable", o)

    if not is_prod:
        return

    funcs = functions(lines)
    in_writer = set()
    for fname, a, b in funcs:
        if fname in WRITER_FUNCS:
            in_writer.update(range(a, b + 1))

    for idx, (lineno, o, m) in enumerate(lines):
        # sigpipe
        for p in pipe_stages(m):
            st = stage_text(o, m, p + 1)
            if SIGPIPE_STAGE.match(st):
                report(lineno, "sigpipe", f"early-exit consumer after a pipe under pipefail: '|{st.strip()[:40]}'", o)
        # test-break
        if re.search(r"(\]\]?|\btest\b[^;&|]*)\s*&&\s*(break|continue)\b", m):
            report(lineno, "test-break", "'[ … ] && break/continue': use an explicit if", o)
        # self-match
        if re.search(r"\bpgrep\s+(-[A-Za-z]*f|--full)", m):
            report(lineno, "self-match", "pgrep -f matches its own wrapper: scan argv exactly", o)
        for p in pipe_stages(m):
            st = stage_text(o, m, p + 1).strip()
            gm = re.match(r"grep\s+((?:-\S+\s+)*)(\S+)", st)
            if gm and re.search(r"\bps\b", m[:p]) and "v" not in gm.group(1):
                pat = gm.group(2).strip("'\"")
                if not pat.startswith("[") and not pat.startswith("$") and pat != "--":
                    report(lineno, "self-match", f"'ps | grep {pat}' matches the grep itself: bracket the first character", o)
        if re.search(r"\*canterlot-worker\*", m):
            report(lineno, "self-match", "substring *canterlot-worker* over a joined command line: match argv exactly", o)
        # run-write
        if idx not in in_writer:
            for rx in (WRITE_CMD, SSH_CMD):
                for wm in rx.finditer(m):
                    start = wm.start("cmd")
                    if RUN_WRITE_PREFIX.search(m[:start]):
                        continue
                    if rx is SSH_CMD:
                        rest = o[wm.end("cmd"):]
                        cut = re.search(r"\s(\|\||&&|\|)\s", m[wm.end("cmd"):])
                        if cut:
                            rest = rest[:cut.start()]
                        if not REMOTE_WRITE.search(rest):
                            continue
                    if re.search(r"#\s*lint:\s*(read-only|local-temp)", o):
                        continue
                    report(lineno, "run-write", f"write command '{wm.group('cmd').strip()}' bypasses run_write", o)

    # wait-subst и последняя команда тела цикла
    for a, b in loops(lines):
        body = lines[a + 1:b]
        tail = [x for x in body if x[2].strip()]
        if tail:
            lineno, o, m = tail[-1]
            if re.match(r"^\s*\[\[?.*\]\]?\s*&&", m):
                report(lineno, "test-break", "'[ … ] && cmd' as the last command of a loop body: its false status ends the loop", o)
        if not any(re.search(r"(?<![\w-])sleep\b", m) for _, _, m in lines[a:b + 1]):
            continue
        for lineno, o, m in body:
            am = re.match(r"^\s*(?:local\s+)?[A-Za-z_][A-Za-z0-9_]*=(.*)$", m)
            val = am.group(1).strip() if am else ""
            if am and not val.startswith("$((") and re.search(r"\$\((?!\()", val) and "||" not in val:
                report(lineno, "wait-subst", "unguarded $(…) assignment in a wait loop under set -e: add '|| VAR=…'", o)


def main(argv):
    files = argv[1:]
    force_prod = False
    if files and files[0] == "--prod":
        # Для тестов самого линтера: перечисленные файлы проверяются как скрипты выката.
        force_prod = True
        files = files[1:]
    if not files:
        files = sorted(os.path.join(DEPLOY, f) for f in os.listdir(DEPLOY) if f.endswith(".sh"))
        files += sorted(os.path.join(HERE, f) for f in os.listdir(HERE) if f.endswith(".sh"))
    problems = []
    for f in files:
        check_file(f, problems, force_prod)
    for p in problems:
        print(p)
    print(f"pipe-lint: {len(files)} files, {len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
