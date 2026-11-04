# Живые пробы Antigravity после сброса квоты (04.11.2026)

Снято на настоящем `agy` с аккаунтом владельца (Google AI Pro). Телеметрия
выключена (`--disable-telemetry`), cwd — пустой `/tmp/agy-probe-0411`, запросы
на запись файлов не выдавались. Клиент — `probe_live.py`, MCP-сервер —
`mcp_echo.py`. Кадры — `p-live-*.jsonl` в формате этапа 0 (`ts`, `dir`, `line`),
заголовки `session/list` и пути владельца вычищены.

## Версия и автообновление

- 18:57 МСК: `agy --version` → `1.4.12\n`.
- 19:00:10 МСК автообновлятор заменил `~/.local/bin/agy`; дальше `--version` →
  `1.5.0\n` (см. `p-version.txt`, 6 байт, без префикса `agy`).
- Все пробы с 19:01:34 — на 1.5.0; проба вопроса в 18:58 — на 1.4.12.
- `VERIFIED_VERSIONS` в `version.rs` — только 1.4.12: на живой машине теперь
  `untested_cli_version`. Поведение вопросов между версиями изменилось (ниже),
  поэтому контрактные сценарии `fake-agy` по кадрам 1.4.12 для вопросов устарели.

## /usage до и после

| Момент | Gemini weekly | Gemini 5h |
| --- | --- | --- |
| до (18:57) | 100% | 100% |
| после (19:07) | 99,3373% | 98,7325% |

Стоимость всех проб — около 0,66% недельной и 1,3% пятичасовой квоты (9
ответов с usage, ~102 тыс. входных токенов суммарно). Формат ответа совпадает с разбором
`limits.rs`: `command.data.groups[].buckets[].window` (`5h`/`weekly`),
`remaining_fraction`, `reset_time`; группа называется `Gemini Models`.
Файлы: `p-live-usage-before.json`, `p-live-usage-after.json`.

## Вопрос (`ask_question`) — P6

Кадры: `p-live-question-v1412.jsonl` (1.4.12), `p-live-question-prompt.jsonl`
(1.5.0, ответ промптом), `p-live-question-cancel.jsonl` (1.5.0, spec-обход).

- Вопрос приходит как `tool_call name:"ask_question" kind:"other"`,
  `rawInput.questions[{question, options[], is_multi_select}]`; ни
  `session/request_permission`, ни `elicitation/create` за все прогоны не было.
- 1.4.12: ход с вопросом висел без `stopReason` ≥83 с. `session/cancel` дал
  `stopReason:"cancelled"`, а следующий `session/prompt` — ошибку `-32603`
  «Failed to respond to ask_question: ... input not registered for step 2».
  Обход §6 (cancel, затем служебный prompt) на 1.4.12 ломает сессию.
- 1.5.0: `session/prompt` возвращает `end_turn` в тот же миг, что и `tool_call`
  вопроса; вопрос при этом остаётся зарегистрированным. Ход не висит, ждать
  нечего (P6: срок ожидания = 0, таймаута нет).
- 1.5.0, ответ без cancel: `session/prompt` с текстом ответа, пока вопрос
  открыт, принимается как ответ на вопрос — `ask_question` уходит в `completed`
  через 2 мс, модель продолжает (в кадре `DONE-BLUE`). Эха `user_message_chunk`
  у этого промпта нет: доказательство — `tool_call_update` вопроса, не эхо.
- 1.5.0, cancel после уже пришедшего `end_turn`: вопрос уходит в `failed`,
  служебный prompt идёт обычным ходом (эхо есть) и модель отвечает. Работает
  только потому, что ход уже закрыт.

Следствие для адаптера: текущий `deliver_decision` (cancel → ждать закрытия
prompt → служебный prompt с доказательством по эху) на 1.5.0 не сработает:
`open_question` не удерживает логический ход, поэтому `end_turn` сразу
закрывает ход и карточку (`turn_ended`), а ответ владельца получает
`question expired`. Нужно: (1) в `open_question` (mod.rs:519) удерживать
логический ход, пока карточка открыта; (2) в `deliver_decision`
(`Answer::Question`, mod.rs:1255) не слать cancel, а слать ответ промптом и
доказывать `Applied` по `ask_question tool_call_update completed`; (3)
`QUESTIONS_CAPABILITY` (mod.rs:56) включать только после этой правки и
сценария `fake-agy` по кадрам 1.5.0. `refuse_request` остаётся cancel-only,
но prompt после него не слать.

## MCP и resume — P15/P16

Кадры: `p-live-mcp1.jsonl`, `p-live-mcp2.jsonl`, `p-live-mcp-global.txt`.

- `mcpServers: [{name, command, args, env: []}]` принимается на `session/new`.
  Инструмент в `tool_call` называется `mcp_echo-probe_echo` (не
  `call_mcp_tool`), `rawInput` — схема самого инструмента
  (`{"text":"pong-7391","toolSummary":"Echo probe call"}`), завершение без
  `rawOutput`, запроса разрешения нет.
- У владельца глобальных MCP нет (`agy mcp list` → «No MCP servers
  configured»), поэтому «заменяет или добавляет» на этом аккаунте не проверить;
  модель видит только сервер, переданный в `mcpServers` (назвала один
  идентификатор).
- `session/resume {sessionId, cwd, mcpServers, _meta…append}` после закрытия
  процесса: id есть в `session/list` (33 сессии), ответ — модели, режимы
  (`default`, `accept-edits`, `plan`), configOptions. Контекст сохранён
  (`BANANA-4417`), MCP-инструмент после рестарта работает, маркер
  `RESUME-APPEND-7F` из `append` появился в конце ответа.

Следствие: resume+append+MCP по §5/§14 подтверждены на 1.5.0. Разбор
инструментов не должен опираться на имя `call_mcp_tool`: MCP-инструменты
приходят как `mcp_<server>_<tool>`.

## Кэш и формула контекста — §8

Кадры: `p-live-cache.jsonl`. Ход 1 — 27,6 КБ текста, ход 2 — короткий вопрос.

- Ход 1: `inputTokens 13718`, `cachedReadTokens 0`, `cachedWriteTokens 0`.
- Ход 2: `inputTokens 13829` (весь контекст заново), cache-поля нули.

Следствие: кадра с cache-hit нет; во всех девяти ответах cache-поля нулевые.
Формула «`inputTokens` без сложения с `cachedReadTokens`» безопасна, пока кэш
не приходит; при первом ненулевом `cachedReadTokens` формулу надо перепроверить.

## Квота — P17

Ошибки квоты не было (квота была 100%). `QUOTA_FROM_PROMPT_ERROR`
(mod.rs:61) остаётся `false`: живого кадра нет.

## Что не снято

P4 (ненулевой `exit_code`), P7 (cancel посреди текста), P8/P9/P10 — не
проверялись: лимит ходов. P14 (effort) не перепроверялся живым ходом;
в кадрах этапа 0 рабочая форма — `session/set_config_option {configId}`, а
`set_model` с суффиксом `-low` меняет `thought_level`; для показа
переключателя нужен A/B-ход low/high с разными usage.
