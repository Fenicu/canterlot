# Сконструированные фикстуры Codex (задача 1)

Записанные свидетельства лежат в `docs/stage0/evidence/codex/` и перечисления не
требуют. Файлы ниже **сконструированы по записям, подтвердить в live smoke**.
Каждый повторяет форму записанного кадра; источники указаны под файлом.

- `handshake_owner.jsonl` — сконструировано, подтвердить в live smoke. Две строки,
  сырые JSON без обёртки `{"t_ms","dir","data"}` (помощник повтора появится
  в задаче 2). Источники формы: `config/read` — запись `ev/start_stream.jsonl:12`
  (поля `approval_policy`, `sandbox_mode`, `sandbox_workspace_write` плюс поле
  `developer_instructions` из схемы `Config`); `account/read` —
  `ev/diagnostics.jsonl:23` (`account` с `type`/`planType`, `requiresOpenaiAuth`).
  Строка 1 — ответ `config/read` владельца (`never` + `danger-full-access`,
  собственный текст инструкций). Строка 2 — ответ `account/read` без входа
  (`account: null`, `requiresOpenaiAuth: true`). Значения `<redacted>` оставлены
  как в записях.
