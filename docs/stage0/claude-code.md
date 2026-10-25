# Этап 0. Claude Code: проверка официального интерфейса из Rust

Отчёт по задаче 1 этапа 0 (ТЗ §20). Прототип: `spikes/claude-probe` (Rust 1.99.1, tokio). Доказательства: `docs/stage0/evidence/claude-code/<сценарий>.jsonl`, формат строки `{"t_ms","dir","data"}`, последняя строка каждого файла содержит `verdict`.

Итог: из десяти обязательных возможностей §10.2 девять подтверждены реальным CLI через документированный интерфейс (PASS), диагностика и отказ внешнего сервиса (§20) тоже PASS. Прерывание получило PARTIAL: документированный SIGINT останавливает ход, но завершает процесс CLI, а прерывание без потери процесса работает только через управляющий запрос, форма которого в документации не описана. Полный обязательный набор для AT-G-02 на версии 2.1.310 поэтому не закрыт; решение по §7 ниже.

## 1. Версия, стенд, модель

| Параметр | Значение |
| --- | --- |
| CLI | `2.1.310 (Claude Code)` (вывод `claude --version`); `/home/fenicu/.local/bin/claude` → `~/.local/share/claude/versions/2.1.310` |
| ОС | Debian 13 (trixie), Linux 6.12.74, x86-64, VM |
| Дата прогонов | 25.10.2026 |
| Вход | `claude auth status --json`: `loggedIn: true`, `authMethod: claude.ai`, `apiProvider: firstParty`, `subscriptionType: max`; в `system/init` `apiKeySource: none` |
| Модель | алиас `--model haiku` → `claude-haiku-5-5` (поле `model` в `system/init` и `modelUsage`), `--effort low` |
| Расход | 35 запусков модели из лимита 40 (учёт в §8); по оценке CLI каждый ход стоил 0,0002–0,004 USD |

Глобальная установка не обновлялась и не перенастраивалась; в дочернем окружении стоит `DISABLE_AUTOUPDATER=1`.

## 2. Используемая поверхность интерфейса

### 2.1. Запуск

```
claude -p --input-format stream-json --output-format stream-json --verbose \
  --include-partial-messages --replay-user-messages \
  --setting-sources project --strict-mcp-config --mcp-config <ipc>/mcp.json \
  --permission-prompt-tool mcp__canterlot__approve --permission-mode manual \
  --model haiku --effort low [--session-id <uuid>] [--resume <session_id>]
```

- Рабочий каталог: свежий `git init` в `/tmp/canterlot-stage0/claude-code/<сценарий>/`, без `.claude/`.
- `mcp.json`: единственный stdio-сервер `canterlot` = тот же бинарник `claude-probe mcp-permission --socket <ipc>/perm.sock`. Сокет супервизора лежит вне рабочего каталога агента, каталог `0700`.
- Окружение: `env_clear()` и белый список. Из родителя: `HOME`, `USER`, `LANG`, `TERM`. Явно: `PATH=<каталог CLI>:/usr/local/bin:/usr/bin:/bin`, `DISABLE_AUTOUPDATER=1`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`. Только в `service_failure` добавлялись `HTTPS_PROXY`/`HTTP_PROXY=http://127.0.0.1:9` и `CLAUDE_CODE_MAX_RETRIES=1`. Переменные `CLAUDECODE`, `CLAUDE_CODE_*`, `ANTHROPIC_*` родительской сессии в ребёнка не попадают (проверяется юнит-тестом `config::tests::child_env_is_allowlist_plus_fixed`).
- Процесс CLI запускается в собственной группе процессов (`setpgid`), stdin/stdout/stderr через pipe; сетевых портов прототип не открывает.

### 2.2. Изоляция от глобальной конфигурации владельца

`--setting-sources project` при пустом проекте и `--strict-mcp-config` дают (`start_stream.jsonl`, заметка `isolation`):

- `mcp_servers`: только `canterlot` (`status: connected`, `source: dynamic`);
- `plugins`: два встроенных в CLI (`cc-plugin-agents-md`, `cc-plugin-plugin-authoring`, `path: builtin`), ни одного из девяти плагинов владельца;
- `skills`/`slash_commands`: только встроенные, ни одного навыка из `~/.claude/skills`, ни одного namespaced-элемента плагина;
- `tools`: встроенные инструменты и `mcp__canterlot__approve`, чужих MCP-инструментов нет;
- `permissionMode: default` (это и есть `manual`); с `--include-hook-events` не пришло ни одного `hook_*` события, то есть пользовательский хук `UserPromptSubmit` не исполнялся.

Остаточные связи с окружением владельца: Bash-инструмент подгружает снимок оболочки из `~/.claude/shell-snapshots` (строится из `~/.bashrc`), транскрипты пишутся в `~/.claude/projects`, CLI открывает входящий сокет межсессионных сообщений `/tmp/cc-socks/<pid>.sock` (риски 5 и 6 в §7).

### 2.3. Элементы протокола и их источник

| Элемент | Источник | Статус |
| --- | --- | --- |
| Флаги запуска из §2.1 | [CLI reference](https://code.claude.com/docs/en/cli-reference), [headless](https://code.claude.com/docs/en/headless) | документировано |
| Ввод `{"type":"user","uuid","message":{"role":"user","content"},"parent_tool_use_id":null}` | [Agent SDK TS: SDKUserMessage](https://code.claude.com/docs/en/agent-sdk/typescript), [Streaming input](https://code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode) | документировано |
| Вывод `system/init`, `stream_event`, `assistant`, `user`, `result`, `system/api_retry`, `system/task_started`, `system/background_tasks_changed`, `rate_limit_event` | [Agent SDK TS: SDKMessage](https://code.claude.com/docs/en/agent-sdk/typescript), [headless](https://code.claude.com/docs/en/headless) | документировано |
| `user_message_uuid` / `user_message_uuids` на первом кадре ответа и в `result` | [Agent SDK TS: user_message_uuid](https://code.claude.com/docs/en/agent-sdk/typescript) | документировано |
| Эхо `--replay-user-messages` (`isReplay: true`, наш `uuid`) | [CLI reference](https://code.claude.com/docs/en/cli-reference), тип `SDKUserMessageReplay` | документировано |
| `--permission-prompt-tool`: сам флаг | [CLI reference](https://code.claude.com/docs/en/cli-reference) | документировано |
| Аргументы вызова prompt tool `{tool_name, input, tool_use_id}` | нигде не описаны; получены наблюдением (`permissions.jsonl`) | **не описано в документации** |
| Ответ prompt tool: JSON `PermissionResult` (`behavior`, `updatedInput`, `message`) текстом в `content[0]` | тип `PermissionResult` описан для `canUseTool` ([TS reference](https://code.claude.com/docs/en/agent-sdk/typescript)); кодировка в MCP-результате выведена | частично документировано |
| Ответ на `AskUserQuestion`: `updatedInput = {questions, answers}` | [Handle approvals and user input](https://code.claude.com/docs/en/agent-sdk/user-input) | документировано |
| SIGINT завершает ход, SIGTERM даёт код 143 и убивает деревья Bash | [headless: Stop a run with SIGTERM](https://code.claude.com/docs/en/headless) | документировано (поведение процесса после SIGINT не описано) |
| `{"type":"control_request","request_id":…,"request":{"subtype":"interrupt","cancel_queued":true}}` | в TS reference упомянуты конверт `{type:"control_request", request_id, request}`, «interrupt control request», поле `cancel_queued` и квитанция `SDKControlInterruptResponse`; ключ `subtype` и полная форма запроса не описаны | **UNDOCUMENTED** (форма выведена) |
| `command_lifecycle` (`queued`/`started`/`completed`/`cancelled` по `command_uuid`), `system/status` со `status:"requesting"`, `tool_result_meta`, `api_message_id`, capabilities `msg_lifecycle_v1`, `interrupt_send_now_v1`, `interrupt_requeue_taken_v1`, `request_marker_lists_v1`, `lifecycle_wakeup_source_v1`, `ui_surface_v1`, `mcp_call_result_not_for_model_v1` | в документации отсутствуют | **UNDOCUMENTED**, только наблюдение, в вердиктах не используется |
| Транскрипт `~/.claude/projects/<cwd>/<session_id>.jsonl` | [Sessions](https://code.claude.com/docs/en/sessions): расположение описано, формат записей «internal… changes between versions» | расположение документировано, формат нет |
| MCP stdio: `initialize`, `tools/list`, `tools/call`, `ping`, `notifications/cancelled` | спецификация MCP (JSON-RPC 2.0) | стандарт MCP |

MCP-сервер разрешений написан вручную (≈150 строк JSON-RPC), без `rmcp`: нужны четыре метода и побайтовая трассировка каждого кадра в доказательства, а зависимость от макросов и меняющегося API SDK на прототипе не окупается. Для продукта `rmcp` можно рассмотреть отдельно. Наблюдение: CLI сначала шлёт `server/discover` (черновик MCP `2026-07-28`); на ответ `-32601` он откатывается к `initialize` с `protocolVersion: 2025-11-25`.

## 3. Матрица возможностей

### 3.1. Обязательные (ТЗ §10.2 и §20)

| Возможность | Статус | Механизм | Доказательство | Ограничения |
| --- | --- | --- | --- | --- |
| Диагностика без платного запроса | PASS | `claude --version`; `claude auth status --json`; «сухой» старт stream-json процесса без ввода и закрытие stdin | `diagnostics.jsonl` | `system/init` до первого ввода не выдаётся (20 с ожидания, `frames: []`), поэтому состав инструментов и MCP без ввода не проверить; MCP-сервер при этом поднимается сразу. EOF без ввода: выход с кодом 0 за 89 мс |
| Начало разговора, native ID | PASS | stream-json процесс; ID назначает воркер через `--session-id <uuid>`, CLI подтверждает его в `system/init`, во всех кадрах и в `result`; транскрипт создаётся под этим ID | `start_stream.jsonl` (`native_session_id`) | `system/init` приходит только после первого ввода (через ≈0,8 с после записи, сразу за `command_lifecycle started`) |
| Текстовый ввод | PASS | `SDKUserMessage` строкой NDJSON в stdin | `start_stream.jsonl`, `multi_input.jsonl` | битая JSON-строка завершает процесс (§6) |
| Несколько последовательных вводов | PASS | ALPHA, BRAVO, CHARLIE в одном процессе и одной сессии, каждый после `result` предыдущего; ответы в порядке, `user_message_uuids` совпадают | `multi_input.jsonl` | ввод во время хода встаёт в очередь CLI (§4.3) |
| Потоковый текст | PASS | `stream_event` с `content_block_delta/text_delta` (`--include-partial-messages`), затем полное `assistant`, затем `result`; склейка дельт совпадает с финалом | `start_stream.jsonl` | дельты крупные (4 дельты на 15 чисел), объединять в UI не требуется |
| Инструменты и ошибки | PASS | `tool_use` в `assistant` ↔ `tool_result` в `user` по `tool_use_id`; `is_error: true` у неуспешного Read; вход инструмента приходит `input_json_delta` и собирается только на `content_block_stop` (12 частей на вызов) | `tools.jsonl` | `ls` и чтение в cwd выполняются без запроса разрешения (встроенный read-only набор) |
| Разрешения | PASS | `--permission-prompt-tool` → наш MCP-сервер на Rust → супервизор по Unix-сокету. Запрос несёт `tool_name`, точный `input`, `tool_use_id` (совпадает с `tool_use`). `probe.txt` отсутствует в момент решения и появляется только после allow; deny с нашим текстом возвращается модели как `tool_result` с `is_error`, вызов попадает в `result.permission_denials` | `permissions.jsonl` | форма аргументов вызова в документации не описана; в обход запроса работают встроенные read-only команды (в том числе `sleep`), а `sleep 30` CLI блокирует сам (`Blocked: standalone sleep 30`) |
| Структурированные вопросы | PASS | `AskUserQuestion` приходит в тот же prompt tool (`tool_name: AskUserQuestion`, `input.questions`); ответ `allow` с `updatedInput.answers`: одиночный выбор строкой, множественный массивом меток, свободный текст значением вне списка. Модель получила `"Which fruits do you like?"="Apple,Cherry"`, `"Which city should we use?"="Reykjavik"` и ответила `COLOR=Green; FRUITS=Apple,Cherry; CITY=Reykjavik` | `question.jsonl` | вопросы доступны в `-p` только при наличии prompt tool; нельзя предложить вариант ответа, которого нет у модели, кроме свободного текста (так и требует §11) |
| Прерывание | PARTIAL | A) SIGINT (документирован): ход остановлен за 35 мс, `result` с `subtype: error_during_execution`, `terminal_reason: aborted_tools`, дочерний `sleep` убит, но процесс CLI завершается (код 0). B) `control_request` `interrupt` с `cancel_queued: true` (форма UNDOCUMENTED): квитанция `control_response` за 8 мс (`cancelled: [<uuid стоявшего в очереди ввода>]`, `still_queued: []`), `result` за 29 мс, `sleep` убит, процесс жив, следующий ввод отработал, отменённый ввод не исполнялся. C) прерывание при неотвеченном запросе разрешения: `result` через 13 мс (`aborted_tools`, вызов в `permission_denials`), CLI прислал MCP `notifications/cancelled` с `requestId`; позднее `allow` после этого ничего не исполнило, `held.txt` не появился | `interrupt.jsonl` | без перезапуска процесса прерывание возможно только через недокументированную форму управляющего запроса; документированный путь равен «остановка хода + выход процесса + `--resume`» |
| Возобновление | PASS | `--resume <session_id>`: тот же ID, модель вспомнила кодовое слово (`PELICAN-SEVEN`). После SIGKILL посреди Bash-вызова `--resume` тоже работает: CLI достраивает в транскрипте синтетический `tool_result` и ответ, новый ввод исполняется | `resume.jsonl` | история при resume в поток не переигрывается; прерванный ход не продолжается (это включает только `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1`); после SIGKILL дочерние `bash` и `sleep` остаются жить (§6) |
| Доказуемая семантика доставки | PASS | положительное подтверждение: первый кадр ответа (`stream_event`/`assistant`) и `result` несут наш `uuid` в `user_message_uuid(s)`; эхо `--replay-user-messages` с нашим `uuid` и тем же текстом приходит в ту же миллисекунду. Неопределённость между записью и подтверждением фиксируется как «приём не подтверждён» (§4) | `delivery_ack.jsonl`, `multi_input.jsonl` | эхо само по себе не доказывает приём (повтор UUID даёт эхо без исполнения); самый ранний признак приёма `command_lifecycle queued` не документирован; сверка после сбоя возможна только по транскрипту внутреннего формата |
| Отказ внешнего сервиса | PASS | несуществующая модель: `assistant.error = model_not_found`, `result` с `is_error: true`, `terminal_reason: api_error`, `api_error_status: 404` через 1,6 с, стоимость 0. Недоступный прокси: `system/api_retry` (`attempt`, `max_retries`, `retry_delay_ms`, `error_status: null`, `error: unknown`), затем `assistant.error = server_error` «Connection refused… (ECONNREFUSED)» и `result` с `is_error`; с `CLAUDE_CODE_MAX_RETRIES=1` итог через 1,3 с | `service_failure.jsonl` | по умолчанию 10 повторов с ростом задержки 0,5 → 1,1 → 2,3 → 4,4 → 9,7 → 18,9 → 35,1 → 38,2 с; за 75 с дошло до 8-й попытки, полное исчерпание дольше 3 минут, всё это время итога хода нет. После ошибочного итога EOF даёт код выхода 1 |

### 3.2. Необязательные (ТЗ §10.2)

| Возможность | Статус | Механизм | Проверка |
| --- | --- | --- | --- |
| Изображения | supported | блок `{"type":"image","source":{"type":"base64","media_type":"image/png","data":…}}` в `message.content` | PASS: PNG 32×32 красный, ответ `red` (`image_input.jsonl`) |
| Файлы | supported, без нативной передачи | путь в тексте ввода и инструменты Read/Glob; упоминания `@path` разворачиваются CLI (отключается `client_composed: true`); `--file` работает только с файлами облака (`file_id`) | не проверялось |
| Выбор модели | supported | `--model` при запуске (фактическая модель видна в `system/init.model` и `modelUsage`); смена в сессии командой `/model <имя>` как обычным вводом в `-p` (headless) | проверен только запуск |
| Effort | supported, применение не наблюдаемо | `--effort low…max`, `/effort <уровень>` вводом; поле `effort` в `system/init` для SDK-клиента не выдаётся (документировано) | передан `--effort low`, эффект из потока не подтверждается |
| Plan mode | supported | `--permission-mode plan`; одобрение плана приходит в prompt tool (Sessions: resume в plan mode с `-p`) | не проверялось |
| Вмешательство в активный запуск | supported | поле `priority` у `SDKUserMessage`: `next` (по умолчанию) — модель читает сообщение в том же ходе после текущих вызовов инструментов; `now` — прерывание или перенос работы в фон; `later` — новый ход после завершения текущего | проверено только поведение по умолчанию на текстовом ходе: ввод встал в очередь и стал отдельным ходом (§4.3) |
| Фоновые работы | supported | `run_in_background` у Bash: `tool_result` с ID задачи и путём к файлу вывода, `system/task_started` и `system/background_tasks_changed` с `task_id`; при EOF CLI ждёт фоновую работу, SIGTERM даёт код 143 и убивает её | PASS (`background.jsonl`); остановка отдельной задачи (`stopTask`) идёт через управляющий протокол, форма которого не описана |
| Usage и токены | supported | `result.usage` (за ход), `result.modelUsage` и `total_cost_usd` (накопительно, клиентская оценка), `rate_limit_event` | данные есть во всех `result` |

## 4. Семантика доставки

### 4.1. Что доказывает «принято агентом» (ТЗ §6.2)

Наблюдаемая последовательность для ввода с нашим UUID (`delivery_ack.jsonl`, `multi_input.jsonl`, `start_stream.jsonl`):

| t после записи | Кадр | Документирован |
| --- | --- | --- |
| +0,55…0,65 с у первого ввода процесса (старт CLI), +2…5 мс у последующих | `command_lifecycle` `state: queued`, `command_uuid` = наш UUID | нет |
| +80…100 мс после queued у первого ввода, +1…3 мс у последующих | `command_lifecycle started`; у первого ввода через ≈60 мс после него `system/init`, затем `system/status` (`requesting`) | нет / да / нет |
| +1,4…2,5 с | эхо `type:user, isReplay:true, uuid:<наш>` и первый `stream_event` с `user_message_uuid(s)` (в одну миллисекунду) | да |
| конец хода | `result` с `user_message_uuid(s)` | да |

Предлагаемое правило для версии 2.1.310: «принято агентом» ставится по первому кадру ответа (`stream_event` или `assistant`), у которого `user_message_uuids` (или `user_message_uuid`) содержит UUID ввода. Эхо `--replay-user-messages` использовать только как подкрепляющий признак:

- повтор уже использованного UUID с другим текстом даёт мгновенное эхо (через 3 мс), но ход не запускается, `result` за 45 с не приходит, текста повтора нет в транскрипте (`duplicate_uuid_behaviour`). Значит, CLI отбрасывает ввод с известным UUID, а эхо ничего не доказывает;
- для нового UUID эхо приходит не в момент чтения строки, а при старте генерации ответа, то есть одновременно с документированным `user_message_uuid`.

Что не доказывается: запись в stdin и сам факт чтения строки (CLI не даёт документированного подтверждения раньше начала ответа). Окно неопределённости составляет 1,4–2,5 с для свободного агента и тянется до конца текущего хода для ввода, записанного во время работы. `command_lifecycle queued` закрыл бы это окно, но он не документирован (capability `msg_lifecycle_v1` в таблице документации отсутствует).

Отказ видно сразу и однозначно только для битой строки: stderr `Error parsing streaming input line (type=user, 25 chars): SyntaxError`, процесс завершается с кодом 1. Неизвестный `type` CLI молча игнорирует и продолжает работу.

### 4.2. Решение о разрешении и ответ на вопрос

- Решение передаётся синхронно: ответ на MCP `tools/call` и есть применение. Доказательство применения: `tool_result` с тем же `tool_use_id` после нашего ответа (allow даёт вывод инструмента, deny даёт `is_error` с нашим текстом), а deny ещё попадает в `result.permission_denials`. В кадре `user` при этом приходит `tool_result_meta.permission_decision` (`accept/user_temporary`, `reject/user_reject`), но это поле не документировано.
- Привязка: `tool_use_id` в аргументах вызова плюс JSON-RPC `id` и `_meta["claudecode/toolUseId"]` в самом `tools/call`. Решение относится к конкретному вызову, а не к «последнему окну».
- Истечение запроса: при прерывании хода CLI шлёт MCP `notifications/cancelled` с `requestId` и причиной `AbortError: remote-cancel`. Позднее решение после этого CLI не применяет (проверено: `allow` после отмены не создал файл). Это даёт адаптеру документированный по MCP сигнал «запрос больше не актуален» для деактивации кнопок (ТЗ §11).
- Запросы разрешений по документации не истекают по таймеру: без ответа ход ждёт бесконечно (TS reference: «permission prompts don't time out»).

### 4.3. Ввод во время работающего хода

Ввод E, записанный во время потоковой генерации D (`multi_input.jsonl`, `concurrent_input_behaviour`): `command_lifecycle queued` через 2 мс; D дописан полностью (1…60); E стартовал через 1 мс после итога D (`started`), его эхо и первый кадр пришли только при старте его собственного хода (через 2,7 с после записи); два отдельных `result`, слияния нет.

По документации поведение по умолчанию (`priority: next`) другое для хода с инструментами: модель прочитает сообщение в том же ходе сразу после текущих вызовов инструментов. Это вмешательство в работающий ход, а не очередь ТЗ §7.3. Поэтому воркер должен держать FIFO у себя и писать следующий ввод только после `result`, а вмешательство отправлять явно (`priority` или прерывание).

Поле `still_queued` квитанции прерывания перечисляет вводы, уже взятые CLI в очередь; с `cancel_queued: true` они переходят в `cancelled` и не исполняются (проверено, но через недокументированную форму запроса).

### 4.4. Сбой между записью и подтверждением (ТЗ §7.2)

- После SIGKILL в середине хода транскрипт содержит наш ввод (запись `type: user` с нашим `uuid`) и `tool_use` ассистента, `tool_result` отсутствует, `result` в потоке не было.
- По документации наш UUID становится идентификатором сообщения транскрипта (`forkSession(upToMessageId)` принимает «uuid you set on a streamed SDKUserMessage»).
- Сверка после сбоя возможна чтением `~/.claude/projects/<cwd>/<session_id>.jsonl` и поиском UUID ввода. Это положительное доказательство приёма, но формат транскрипта по документации внутренний и меняется между версиями, поэтому его разбор нужно закрепить за версией CLI и отключать при неизвестной версии.
- Отсутствие UUID в транскрипте не доказывает неприём (запись на диск асинхронна, пропуск не исключён), значит по ТЗ ввод остаётся «приём не подтверждён». Не проверено, что происходит с вводом, который был в очереди CLI (`queued`, но не `started`) в момент SIGKILL.
- Повтор того же UUID в живом процессе CLI отбрасывает. Для нового процесса после `--resume` поведение повтора не проверено. Повторять неопределённый ввод автоматически ТЗ и так запрещает.

## 5. Факты о процессе для проектирования воркера

| Вопрос | Факт | Доказательство |
| --- | --- | --- |
| Native conversation ID | `session_id`: назначается флагом `--session-id <uuid>` до запуска или берётся из `system/init`; есть в каждом кадре. `--resume` сохраняет тот же ID (`--fork-session` создаёт новый) | `start_stream`, `resume` |
| Признак конца хода | кадр `result` (`subtype`, `is_error`, `terminal_reason`: `completed`, `aborted_tools`, `api_error`…, `user_message_uuids`, `result_index`) | все сценарии |
| Фаза | документированы `stream_event`, `system/api_retry`, `system/task_*`; наблюдается недокументированный `system/status` со `status: requesting` и `command_lifecycle` | все сценарии |
| EOF на stdin | без работы: выход за 84–89 мс, код 0 (код 1, если последний `result` был ошибкой); при работающей фоновой команде CLI не завершается и ждёт её | `start_stream`, `service_failure`, `background` |
| SIGINT | во время хода: `result` с `aborted_tools` за 35 мс, дерево Bash убито, затем выход с кодом 0 | `interrupt` (случай A) |
| SIGTERM | выход с кодом 143 за ≈0,65 с, фоновая Bash-команда убита (как в документации) | `background` |
| SIGKILL | `result` нет; дочерние `bash` и `sleep` переживают CLI; MCP-сервер завершается по EOF своего stdin | `resume` (`sigkill_orphans`) |
| Отсоединённые потомки | каждый Bash-вызов запускается как `/bin/bash -c source ~/.claude/shell-snapshots/… && …` в собственной группе процессов, его дети тоже в своих группах. Поэтому `killpg` группы CLI их не достаёт | деревья процессов в `interrupt`, `resume`, `background` |
| Прочие процессы и файлы | MCP-сервер разрешений в группе CLI; вывод фоновых задач `/tmp/claude-<uid>/<проект>/<сессия>/tasks/<id>.output`; входящий сокет межсессионных сообщений `/tmp/cc-socks/<pid>.sock` (`messaging_socket_path` в init) | `background`, `start_stream` |
| Нативная история | `~/.claude/projects/<cwd, где не буквы и не цифры заменены на '-'>/<session_id>.jsonl`, хранение 30 дней (`cleanupPeriodDays`), формат внутренний; удаление штатно через `claude purge` | `delivery_ack`, `resume` (сводки `transcript*` без содержимого) |

## 6. Поведение при ошибках протокола

- Битая JSON-строка в stdin завершает процесс (код 1) и обрывает сессию; ход и очередь теряются, resume остаётся возможен. Воркер обязан сериализовать ввод только из проверенной структуры.
- Неизвестный `type` во вводе игнорируется, процесс работает дальше.
- Повтор UUID даёт эхо без исполнения (§4.1).
- CLI блокирует некоторые команды сам (`standalone sleep 30`) и пропускает без запроса встроенные read-only команды (`ls`, `sleep 20`). UI должен показывать решение агента и встроенную политику CLI раздельно (AT-D-06).

## 7. Риски и открытые вопросы (по влиянию)

1. **Прерывание без потери процесса не документировано.** Документированный SIGINT останавливает ход, но завершает CLI; каждая остановка оборачивается перезапуском с `--resume` (около 1 с старта и потеря очереди CLI, что по ТЗ §7.3 допустимо, поскольку очередь держит воркер). Полноценный путь `control_request interrupt` с квитанцией и `cancel_queued` работает, но форма запроса выведена из упоминаний в документации. Нужно решение владельца: (а) принять SIGINT + resume как контракт прерывания (тогда возможность становится PASS с ограничением по задержке); (б) разрешить частично документированный управляющий протокол с проверкой capability `interrupt_receipt_v1` и отдельным тестом на каждую версию CLI; (в) запросить у поставщика описание протокола.
2. **Дочерние процессы инструментов уходят из группы CLI.** После SIGKILL (или смерти сопровождения) `bash` и его дети остаются жить в собственных группах. Для ТЗ §9.5 нужна группировка через cgroup v2 (например, systemd scope на сессию) с учётом `pid + start_time`; одного `killpg` недостаточно.
3. **Ввод во время хода сливается с текущим ходом**, если в ходе есть вызовы инструментов (`priority: next` по умолчанию). Воркер должен выдавать следующий ввод только после `result`, а вмешательство отправлять осознанно.
4. **Сверка после сбоя опирается на внутренний формат транскрипта.** Если владелец запретит его разбор, все вводы, прерванные между записью и первым кадром ответа, останутся «приём не подтверждён». Открыто: поведение повторного UUID после `--resume` и судьба ввода в очереди CLI при SIGKILL.
5. **Входящий сокет межсессионных сообщений.** `-p` процесс открывает `/tmp/cc-socks/<pid>.sock` и по умолчанию принимает сообщения от других сессий Claude Code этого пользователя (в том числе от интерактивных сессий владельца). Это ввод, минующий хаб. Документированное отключение: `crossSessionInbound: "refuse"` и запрет `SendMessage`/`ListAgents`, например через `--settings`. В прототипе не включено и не проверялось; добавить в базовые флаги воркера и проверить на этапе 2.
6. **Утечка окружения владельца в инструменты.** Bash подгружает снимок `~/.bashrc` из `~/.claude/shell-snapshots`, транскрипты и состояние проектов пишутся в `~/.claude`. Полная изоляция требует отдельного `CLAUDE_CONFIG_DIR` с собственным входом; это меняет модель авторизации (ТЗ §15).
7. **Недокументированные кадры и поля** (`command_lifecycle`, `system/status: requesting`, `tool_result_meta`, часть capabilities) удобны, но опираться на них нельзя; неизвестные типы нужно пропускать с диагностикой (так делает прототип: `Kind::Other`).
8. **Долгие повторы при отказе API.** Без ограничения ход до трёх с лишним минут висит в `api_retry` без итога. Решить: показывать фазу «повтор запроса к API» из `system/api_retry` и/или задавать `CLAUDE_CODE_MAX_RETRIES`.
9. **Форма аргументов prompt tool не описана.** На 2.1.310 это `{tool_name, input, tool_use_id}`; изменение в новой версии сломает разрешения и вопросы без явной ошибки совместимости. Нужен контрактный тест на каждую закрепляемую версию.
10. **Проверено только на `claude-haiku-5-5`.** Эффект `--effort` из потока не наблюдаем. На других моделях поведение инструментов и вопросов может отличаться.
11. **Версия CLI меняется часто** (2.1.310 на дату проверки). Воркер должен запускать конкретный бинарник `versions/<версия>`, отключать автообновление и сверять `claude_code_version` из init с закреплённой матрицей.

## 8. Воспроизведение и учёт квоты

```
cd spikes/claude-probe
cargo test                                  # 48 тестов
cargo clippy --all-targets -- -D warnings
cargo run -- list                           # список сценариев
cargo run -- <сценарий>                     # один сценарий
cargo run -- all                            # все подряд (≈30 ходов модели)
```

Переменные пробы: `CANTERLOT_CLAUDE` (путь к CLI), `CANTERLOT_MODEL`, `CANTERLOT_EFFORT`, `CANTERLOT_EVIDENCE_DIR`. Каталоги сценариев пересоздаются в `/tmp/canterlot-stage0/claude-code/`; общий журнал ходов `/tmp/canterlot-stage0/claude-code/turns.log`.

| Сценарий | Прогонов | Запусков модели | Итог |
| --- | --- | --- | --- |
| diagnostics | 3 | 0 | PASS (первый прогон записал адрес почты из `auth status` построчно; маскирование исправлено, файл перезаписан до коммита) |
| start_stream | 3 | 3 | PASS (первый прогон FAIL из-за ложного срабатывания проверки изоляции на встроенных плагинах, последний добавил `--session-id`) |
| multi_input | 1 | 5 | PASS |
| delivery_ack | 2 | 3 (счётчик пробы показал 5: повтор UUID модель не вызывал) | PASS |
| tools | 1 | 1 | PASS |
| permissions | 1 | 1 | PASS |
| question | 1 | 1 | PASS |
| interrupt | 2 | 11 (счётчик пробы показал 10; первый прогон FAIL: проба ждала запроса разрешения, которого для `sleep` не бывает, и прерывала уже простаивающий процесс) | PARTIAL |
| resume | 2 | 8 (в первом прогоне CLI заблокировал `sleep 30`, SIGKILL пришёлся на простой) | PASS |
| service_failure | 1 | 0 (API отклонил запрос, стоимость 0, либо не был достигнут) | PASS |
| background | 1 | 1 | PASS |
| image_input | 1 | 1 | PASS |
| **Итого** |  | **35 из 40** (счётчик пробы: 36) |  |

Под «запуском модели» понимается пользовательский ввод, на который CLI начал ход; внутри хода с инструментами было до 3 обращений к API (`num_turns` в `result`).

Известная неточность в доказательстве: в `multi_input.jsonl` поле `d_text_complete_1_to_60: false` вычислено ошибочной проверкой (склеивала тексты двух ходов). Проверка исправлена после прогона; в той же заметке поле `results_after_d[0].summary.result` содержит полную последовательность 1…60. Повторный прогон (5 ходов) не делался ради экономии квоты.

Секреты: доказательства проверены на `token`, `Bearer`, `sk-`, `api_key`, `refresh`, адреса почты и идентификатор организации; поля `email`, `orgId`, `orgName` из `auth status` замаскированы при записи.
