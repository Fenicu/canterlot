# Этап 0: адаптер Codex (`codex app-server`)

Исследовательский отчёт этапа 0 (ТЗ §20) по интеграции Codex через официальный `codex app-server` поверх stdio. Прототип: `spikes/codex-probe` (Rust, tokio). Доказательства: `docs/stage0/evidence/codex/*.jsonl`.

Итог: все обязательные возможности §10.2, кроме структурированных вопросов, подтверждены реальным CLI на стабильной поверхности протокола. Структурированные вопросы работают только через экспериментальную поверхность: PARTIAL, требуется решение заказчика (раздел 7, риск 1). Сам `codex app-server` поставщик помечает как экспериментальный (риск 2).

## 1. Версия и стенд

| Параметр | Значение |
| --- | --- |
| CLI | `codex-cli 0.164.0` (`codex --version`), npm-пакет `@openai/codex`, `/usr/bin/codex` -> Node-обёртка, запускающая нативный `vendor/x86_64-unknown-linux-musl/bin/codex` |
| Node.js (обёртка) | v24.22.1 |
| ОС | Debian 13 (trixie), Linux 6.12.74+deb13+1-amd64, x86_64 |
| Дата | 2026-10-25 |
| Учётная запись | ChatGPT (plan `plus`), вход локальный, `account/read` без обновления токена |
| Модель для доказательств | `gpt-6-luna` (в `model/list`: «Fast and affordable model for easier tasks»), effort `low` |
| Прототип | `spikes/codex-probe` 0.1.0, Rust 1.99.1, edition 2024 |

Расход квоты: 30 запросов `turn/start`/`turn/steer` за всю работу (27 в итоговых файлах evidence и 3 в первом прогоне `interrupt`, перезаписанном повторным прогоном). Из них до модели не дошли 2 (отклонённый запрос с экспериментальным полем и недоступный endpoint), 1 отклонён API с HTTP 400 без генерации, 1 `turn/start` и 1 `turn/steer` влились в уже идущий ход. Реальных ходов модели: 25. Токены по `thread/tokenUsage/updated`: 346 226 всего (в основном входной контекст ~10,5k на запрос), из них 835 выходных.

## 2. Используемая поверхность интерфейса

Источники: [Codex App Server](https://developers.openai.com/codex/app-server) (далее «AS»; та же страница доступна как [learn.chatgpt.com/docs/app-server](https://learn.chatgpt.com/docs/app-server.md)), [Rules](https://learn.chatgpt.com/docs/agent-configuration/rules.md), [Agent approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security.md), [Configuration Reference](https://learn.chatgpt.com/docs/config-file/config-reference.md), [AGENTS.md](https://learn.chatgpt.com/docs/agent-configuration/agents-md.md), [Environment variables](https://learn.chatgpt.com/docs/config-file/environment-variables.md), `codex app-server --help`, `codex app-server generate-json-schema [--experimental]`, `generate-ts [--experimental]`.

Источник истины по форме сообщений: JSON Schema, сгенерированная CLI 0.164.0. Схема весит 4,5 МиБ (стабильная) и 5,7 МиБ (с экспериментальными полями), поэтому в репозитории лежат только манифесты sha256 и команда перегенерации: `docs/stage0/evidence/codex/schema-0.164.0/json-schema-{stable,experimental}.sha256`. Повторная генерация побайтно совпала с манифестом.

Стабильность. `codex app-server --help` начинается с `[experimental] Run the app server or related tooling`; `generate-ts` и `generate-json-schema` тоже помечены `[experimental]`. AS: «The app-server command and WebSocket transport are experimental and aren't supported for production workloads». Внутри протокола есть отдельный механизм `initialize.params.capabilities.experimentalApi`: без него сервер отклоняет экспериментальные методы и поля (`<descriptor> requires experimentalApi capability`, проверено в `question.jsonl`).

| Элемент | Как используется | Документ | Статус |
| --- | --- | --- | --- |
| Транспорт | `codex app-server --listen stdio://`, JSONL, JSON-RPC 2.0 без поля `jsonrpc` | AS: Protocol | весь app-server experimental по `--help` |
| Рукопожатие | `initialize {clientInfo}` -> `initialized` | AS: Initialization | стабильно |
| Конфигурация | `-c key=value`, `--disable <feature>`; проверка `config/read {includeLayers:true}` | `--help`, Config Reference, AS: Config RPC | стабильно |
| Готовность | `account/read {refreshToken:false}`, `model/list`, `mcpServerStatus/list` | AS: Auth endpoints, Models | стабильно |
| Тред | `thread/start`, `thread/resume {excludeTurns:true}`, `thread/turns/list`, `thread/items/list` | AS: Threads | стабильно |
| Ход | `turn/start {input, clientUserMessageId, model, effort}` | AS: Turns | стабильно; `clientUserMessageId` есть в схеме, в тексте AS не описан |
| Вмешательство | `turn/steer {expectedTurnId, input}` | AS: Steer an active turn | стабильно |
| Прерывание | `turn/interrupt {threadId, turnId}` | AS: Interrupt a turn | стабильно |
| События | `turn/*`, `item/*`, `item/agentMessage/delta`, `thread/status/changed`, `thread/tokenUsage/updated`, `error`, `serverRequest/resolved` | AS: Events, Items, Errors | стабильно |
| Разрешения | server request `item/commandExecution/requestApproval`, `item/fileChange/requestApproval` -> `{decision}` | AS: Approvals | стабильно |
| Вопросы | server request `item/tool/requestUserInput` -> `{answers}` | AS: `tool/requestUserInput` | EXPERIMENTAL (типы помечены `EXPERIMENTAL` в схеме; появляется только в plan mode или с фичей в статусе under development) |
| Plan mode | `turn/start.collaborationMode {mode:"plan"}`, `collaborationMode/list` | AS: Turns, API overview | EXPERIMENTAL (`experimentalApi`) |
| Фоновые терминалы | `thread/backgroundTerminals/list`, `.../clean` | AS: Clean background terminals | EXPERIMENTAL (`experimentalApi`) |

Не использовались: `--listen ws://`/`unix://` (сетевой и сокетный транспорт), `codex app-server daemon`/`proxy`, `thread/shellCommand` (выполняется вне песочницы), `process/*`, `command/exec`. Недокументированные механизмы не применялись.

`codex app-server daemon` (bootstrap/start/stop/update/enable-remote-control) и `proxy` (stdio <-> control socket) описаны только в `--help`; в руководстве Codex они упоминаются как механизм remote control, который «aren't a replacement for `codex app-server --listen` when you're building a local protocol client». Для воркера не подходят: процесс CLI должен принадлежать сопровождению сессии (§4.4, §9.5).

### 2.1. Изоляция от глобальной конфигурации владельца

Глобальный `~/.codex/config.toml` владельца задаёт `approval_policy="never"`, `sandbox_mode="danger-full-access"`, дорогую модель с `xhigh`, MCP-серверы `nanokvm` (физический KVM) и `openaiDeveloperDocs`, плагин `antislop@anti-slop`, `sandbox_workspace_write.network_access=true` с лишними `writable_roots`. Каждый запуск пробы переопределяет это и до первого хода модели проверяет результат через `config/read` (запуск прерывается при любом расхождении, см. `isolation` в каждом evidence-файле):

- `-c approval_policy="on-request"`, `-c sandbox_mode="read-only"`, `-c sandbox_workspace_write.network_access=false`, `-c sandbox_workspace_write.writable_roots=[]`; в `thread/start`/`thread/resume` дублируются `approvalPolicy:"on-request"`, `approvalsReviewer:"user"`, `sandbox:"read-only"`, `model`.
- `-c model="gpt-6-luna"`, `-c model_reasoning_effort="low"`.
- `-c mcp_servers.nanokvm.enabled=false`, `-c mcp_servers.openaiDeveloperDocs.enabled=false`; `mcpServerStatus/list` показывает `runtimeStatus:null`, `serverInfo:null`, уведомлений `mcpServer/startupStatus/updated` нет.
- `-c plugins.antislop@anti-slop.enabled=false` и `--disable` для `plugins remote_plugin apps memories multi_agent goals hooks computer_use browser_use browser_use_external in_app_browser image_generation tool_suggest skill_mcp_dependency_install`.
- `-c web_search="disabled"`, `-c project_doc_max_bytes=0`, `-c check_for_update_on_startup=false`, `-c analytics.enabled=false`, `-c history.persistence="none"`.
- Guard-правила execpolicy в scratch-проекте (`.codex/rules/canterlot-guard.rules`, `decision="forbidden"` для `uv npm sudo ssh scp curl codex git add|commit|push`) и `-c projects.<scratch>.trust_level="trusted"`, без которого проектный слой не грузится. Причина: пользовательский `~/.codex/rules/default.rules` разрешает часть этих префиксов, а по документу Rules `allow` означает «Run the command outside the sandbox without prompting». При нескольких совпадениях побеждает самое строгое решение; `codex execpolicy check` с обоими файлами даёт `forbidden` (`diagnostics.jsonl`).
- Окружение дочернего процесса очищается (`env_clear`), передаются только `HOME`, `USER`, `LANG=C.UTF-8`, `TERM=dumb`, `PATH=/usr/local/bin:/usr/bin:/bin`. Сетевых портов не открывается: только `--listen stdio://`.
- `~/.codex/config.toml` и `auth.json` не изменялись и не копировались; `--startup-sign-out` не использовался.

Что изолировать документированным способом не удалось (риск 7): глобальный `~/.codex/AGENTS.md` всегда попадает в `instructionSources` (`project_doc_max_bytes=0` его не отключает); пользовательские `.rules` нельзя выключить, можно только перекрыть более строгими; `approval_policy="untrusted"` снят с поддержки (CLI не стартует: `approval_policy = "untrusted" is no longer supported`), хотя в схеме `AskForApproval` он остаётся; в `-c` нельзя адресовать ключи с точками или кавычками в сегменте пути (`plugins."a@b"` даёт ключ с буквальными кавычками), поэтому путь проекта с точкой нельзя пометить trusted через `-c`.

## 3. Матрица возможностей

Обязательные возможности §10.2 и список этапа 0 (§20). Ограничение описано -> не PASS (§25.9).

| Возможность | Статус | Механизм | Evidence | Ограничения |
| --- | --- | --- | --- | --- |
| Диагностика без платной задачи | PASS | `codex --version`; `initialize`; `account/read {refreshToken:false}` (`type:"chatgpt"`, `requiresOpenaiAuth:true`); `model/list`; `config/read`; `mcpServerStatus/list`; `thread/start` (ephemeral) не вызывает модель | `diagnostics.jsonl` | `account/read` не проверяет квоту; лимиты есть в `account/rateLimits/read` |
| Начало разговора, native ID | PASS | `thread/start {cwd, model, approvalPolicy, sandbox}` -> `thread.id` (UUIDv7), `thread.path` (rollout JSONL) | `start_stream.jsonl` | Тред попадает в общий `~/.codex/sessions` владельца с `source:"vscode"` |
| Текстовый ввод | PASS | `turn/start {input:[{type:"text"}]}` | `start_stream.jsonl` | - |
| Несколько последовательных вводов | PASS | 3 хода подряд в одном процессе, разные `turnId`, контекст сохранён (ALPHA) | `multi_input.jsonl` | `turn/start` во время активного хода не ставится в очередь и не отклоняется, а вливается в активный ход (раздел 4.3) |
| Потоковый текст | PASS | `item/agentMessage/delta` по `itemId`; авторитетный текст в `item/completed` | `start_stream.jsonl` | Конкатенация дельт совпала с итогом; AS предупреждает, что для `plan` это не гарантировано |
| Инструменты и ошибки | PASS | `commandExecution` `item/started` -> `item/completed {status, exitCode, aggregatedOutput}`; неуспешная команда (`exitCode:2`, `status:"failed"`) не ломает ход | `tools.jsonl` | Для коротких команд `item/commandExecution/outputDelta` не пришёл, вывод только в `aggregatedOutput` |
| Разрешения | PASS | `item/commandExecution/requestApproval` -> `accept`: `probe.txt` появился только после решения; `item/fileChange/requestApproval` -> `decline`: элемент `fileChange` завершён `status:"declined"`, файла нет; оба запроса несут `threadId`, `turnId`, `itemId`; `serverRequest/resolved {requestId}` через 1 мс | `permissions.jsonl` | `availableDecisions` команды: `accept`, `acceptWithExecpolicyAmendment`, `cancel` (без `decline`); повторный ответ на закрытый запрос молча игнорируется |
| Структурированные вопросы | PARTIAL | `item/tool/requestUserInput` (вопрос с `options`, `isOther`) -> `{answers:{<id>:{answers:["Blue"]}}}`; модель ответила `BLUE` | `question.jsonl`, `question_default_mode.jsonl` | Только экспериментальная поверхность: `experimentalApi` + `collaborationMode:"plan"`, либо фича `default_mode_request_user_input` (under development). На стабильной поверхности модель отвечает «request_user_input ... unavailable in Default mode», `collaborationMode` без opt-in отклоняется |
| Прерывание | PASS | `turn/interrupt` -> ответ `{}` и `turn/completed status:"interrupted"` через 15-16 мс; ожидающее одобрение снимается (`serverRequest/resolved`), поздний `accept` не исполняется; следующий ход в треде проходит | `interrupt.jsonl`, `interrupt_cleanup.jsonl` | Запущенная команда после interrupt продолжает работать (живой `sleep` через 10 с, элемент не завершён) как фоновый терминал. Остановить её можно экспериментальным `thread/backgroundTerminals/clean` или убийством дерева процессов |
| Возобновление | PASS | Новый процесс, `thread/resume {threadId}` -> ход вспомнил `ZEBRA-42`; после SIGKILL процесса тред возобновился, прерванный ход сохранён как `interrupted` с исходным `clientId` | `resume.jsonl` | SIGKILL Node-обёртки оставляет нативный app-server и команды живыми (раздел 6) |
| Доказуемая семантика доставки | PASS | `clientUserMessageId` возвращается в `userMessage.clientId` на `item/started`/`item/completed` и в `thread/items/list` | `delivery_ack.jsonl`, `resume.jsonl` | Сервер не дедуплицирует `clientUserMessageId`: повтор создаёт новый ход |
| Отказ внешнего сервиса | PASS | Недоступный endpoint: уведомления `error {willRetry:true, codexErrorInfo:{responseStreamDisconnected}}` с паузами 3,4/11,5/24,6/47,6/90,7/153,8 с, терминального отказа нет, ход завершён клиентским `turn/interrupt`. Неверная модель: `error {willRetry:false}` и `turn/completed status:"failed"` через 1,7 с, HTTP 400 `model_not_found` внутри `message` | `service_failure.jsonl` | Переподключение бесконечное: фича `unbounded_connection_retries` включена; решение о сдаче принимает клиент |

Дополнительно без расхода квоты: `lifecycle.jsonl` (EOF/SIGINT/SIGTERM), фоновые процессы `background.jsonl`, изображение `image_input.jsonl`.

## 4. Семантика доставки

### 4.1. Пользовательский ввод: что доказывает «принято агентом» (§6.2)

Наблюдаемые факты по одному вводу (`delivery_ack.jsonl`, первый ход треда):

| Время, мс | Факт | Что доказывает |
| --- | --- | --- |
| 0 | запись строки `turn/start` в stdin | ничего (§6.2: запись байтов не даёт права на «принято») |
| +6 | ответ на `turn/start`: `turn.id`, `status:"inProgress"` | сервер принял запрос и привязал ввод к ходу `turn.id`. Это может быть уже идущий ход (раздел 4.3) |
| +134 | `turn/started` с тем же `turn.id` | ход начат (не пришло для ввода, влитого в активный ход) |
| +1239 | `item/started` `userMessage` c `clientId` = наш `clientUserMessageId` | ввод записан в историю треда как элемент `itemId` этого хода |
| +2636 | `turn/completed` | завершение ответа, не состояние доставки |

Предлагаемое правило адаптера: «Принято агентом» = получен `item/started` (или `item/completed`) типа `userMessage` с `clientId`, равным ID ввода, в ожидаемом треде. Это сильнейший документированный факт: тот же элемент виден в `thread/items/list` без resume и переживает SIGKILL процесса (`resume.jsonl`: прерванный ход хранит `userMessage.clientId="cu-killed-turn"`). Ответ на `turn/start` с `turn.id` можно показывать как промежуточное «передаётся агенту/запрос принят сервером»: он приходит раньше, но не доказывает запись ввода в историю.

Что не доказано: что модель уже прочитала ввод (это видно только по последующим элементам ответа); что запись `userMessage` на диск происходит раньше уведомления `item/started` (порядок «сначала fsync, потом уведомление» не документирован).

### 4.2. Решение по разрешению или вопросу

- Запрос сервера несёт JSON-RPC `id` (в пределах соединения), `threadId`, `turnId`, `itemId`; для команд дополнительно `approvalId` (у обычных команд `null`), `command`, `cwd`, `reason`, `availableDecisions`. Связка решения с конкретным элементом: `itemId` совпадает с `item/started` элемента.
- После ответа клиента сервер шлёт `serverRequest/resolved {threadId, requestId}`, затем `item/completed` с итоговым статусом (`completed`/`declined`/`failed`). «Агент применил решение» = `serverRequest/resolved` по нашему `requestId` и `item/completed` этого `itemId` с соответствующим статусом.
- `serverRequest/resolved` приходит и без ответа клиента, если запрос снят прерыванием, началом или завершением хода (`interrupt.jsonl`, фаза B; AS: `tool/requestUserInput`). Различить «снят моим ответом» и «снят очисткой» по одному уведомлению нельзя: смотреть на статус хода и элемента.
- Повторный или поздний ответ на закрытый запрос сервер молча игнорирует: нет ошибки, нет повторного исполнения (`permissions.jsonl`, `interrupt.jsonl`). Подтверждения «ответ отвергнут» клиент не получает.
- Для вопросов допустимые ответы ограничены `options[].label` (и свободным вводом при `isOther:true`); проба выбирает только предложенный вариант (§11).

### 4.3. Ввод во время активного хода

`turn/start` при активном ходе не ставится в очередь и не отклоняется: ответ содержит `turn.id` текущего хода, ввод становится ещё одним `userMessage` этого хода, и модель учитывает его в текущем ответе (`multi_input.jsonl`: ответ хода 4 «EXTRA STEERED» вместо «DONE4»). В схеме это отражено фразой «Ignored when this request adds input to an active turn» в описании `parentTurnId`. `turn/steer` делает то же самое явно, с предусловием `expectedTurnId`, и возвращает `{turnId}`.

Следствие для воркера: обычная отправка (очередь следующего запуска, §7.3) реализуется только на стороне воркера: `turn/start` вызывается после `turn/completed` и `thread/status/changed -> idle`. Явное вмешательство (§7.3) соответствует `turn/steer`. У `turn/start` нет предусловия «тред свободен», поэтому гонка «ход только что начался по другой причине» незаметно превратит обычную отправку во вмешательство (риск 4).

### 4.4. Сбой между записью и подтверждением (§7.2)

Если сопровождение упало после записи `turn/start`, но до `userMessage` с нашим `clientId`:

- Сверка: новый процесс, `thread/resume` (или `thread/turns/list`/`thread/items/list` без resume) и поиск `userMessage.clientId`. Найден -> положительное доказательство приёма, ход и его статус известны (после SIGKILL ход помечен `interrupted`).
- Не найден -> «приём не подтверждён». Документация не гарантирует, что принятый ввод записывается до уведомлений, поэтому отсутствие записи не доказывает неприём.
- Автоматический повтор запрещён ещё и потому, что сервер не дедуплицирует `clientUserMessageId` (`delivery_ack.jsonl`: второй `turn/start` с тем же ID создал новый ход и был исполнен).
- Для решений по разрешениям после сбоя повторить ответ невозможно: запрос живёт только в соединении, при resume ход прерванный. Решение «не применено» устанавливается по статусу элемента в истории.

## 5. Необязательные возможности (§10.2)

| Возможность | Статус | Механизм и доказательство |
| --- | --- | --- |
| Изображения | supported | `input` с `{type:"localImage", path}` (и `{type:"image", url}` по AS); модель ответила `Red` на красный PNG (`image_input.jsonl`); `model/list.inputModalities` = `text, image` |
| Файлы | unsupported (нет типа ввода) | `UserInput`: `text`, `image`, `localImage`, `audio`, `localAudio`, `skill`, `mention`. Файл нужно положить в рабочий каталог и сослаться на путь в тексте; агент читает его своими инструментами |
| Выбор модели | supported | `model/list` (id, efforts, modalities, `isDefault`); `thread/start.model`, `turn/start.model`; неверная модель принимается `thread/start` с `warning` и падает на ходе (`service_failure.jsonl`) |
| Effort | supported | `model/list.supportedReasoningEfforts`; `turn/start.effort`, `thread/start` возвращает `reasoningEffort` |
| Plan mode | supported, EXPERIMENTAL | `turn/start.collaborationMode {mode:"plan"}`, пресеты `collaborationMode/list` (`question.jsonl`); без `experimentalApi` поле отклоняется |
| Вмешательство в активный запуск | supported | `turn/steer {expectedTurnId}` -> `{turnId}`, ввод учтён в текущем ответе (`multi_input.jsonl`) |
| Фоновые работы | supported частично, управление EXPERIMENTAL | Команды unified exec продолжают жить после ответа модели и после `turn/interrupt`; `thread/backgroundTerminals/list` (`processId` внутренний, `osPid:null`), `clean` останавливает (`background.jsonl`, `interrupt_cleanup.jsonl`); стабильного способа узнать о фоновых процессах нет |
| Usage/tokens | supported | `thread/tokenUsage/updated` (`total`, `last`, `modelContextWindow`) на каждый ход; `account/rateLimits/updated` (окна 300 мин и 7 дней, `usedPercent`); `account/rateLimits/read`, `account/usage/read` |

## 6. Факты уровня процесса

- Native ID разговора: `thread.id` из ответа `thread/start` (и `thread/started`), UUIDv7; `thread.sessionId` у корневого треда равен `thread.id`. ID хода: UUIDv7. ID элементов: `userMessage` UUIDv7, `agentMessage` `msg_...`, команды `exec-<uuid>`, вызовы инструментов `call_...`.
- Конец хода: `turn/completed {turn.status: completed|interrupted|failed, turn.error}` и `thread/status/changed -> {type:"idle"}`. Во время работы `thread/status/changed -> {type:"active", activeFlags:[...]}`. Уведомления несут `emittedAtMs` (время сервера).
- Дерево процессов: `node /usr/bin/codex app-server` (обёртка) -> нативный `codex app-server` -> `codex-code-mode-host` (своя группа процессов) и на каждую команду `bwrap --new-session --die-with-parent` -> `codex-linux-sandbox` -> команда (своя сессия). Процессы CLI не остаются в группе, созданной воркером (`resume.jsonl`, `background.jsonl`).
- stdin EOF: без хода выход за ~20 мс с кодом 0; с живым фоновым терминалом выход за ~1 с, фоновый процесс завершён (`lifecycle.jsonl`, `background.jsonl`).
- SIGINT/SIGTERM на обёртку без активного хода: выход с кодом 0 за 22-32 мс, нативный процесс тоже завершается, сирот нет (`lifecycle.jsonl`). Поведение при SIGTERM во время хода не проверялось.
- SIGKILL обёртки во время команды: нативный app-server, `codex-code-mode-host`, `bwrap`, sandbox и `sleep` остаются жить как сироты (`resume.jsonl`, `survivors_after_sigkill_of_direct_child`). Убийство группы процессов воркера их не накрывает. Нужен контейнер на уровне cgroup v2 или рекурсивное завершение дерева по pid + время старта (§9.5).
- `turn/interrupt` не завершает уже запущенную команду: процесс жив минимум 10 с после `turn/completed interrupted`, его элемент завершается только при смерти процесса, уже после терминального статуса хода (поздний факт, §9.1).
- Нативная история: `~/.codex/sessions/YYYY/MM/DD/rollout-<время>-<threadId>.jsonl` (путь приходит в `thread.path` ответа `thread/start`); рядом в `CODEX_HOME` лежат служебные SQLite-файлы (наблюдение по каталогу, формат не документирован); режим истории новых тредов `paginated`. Читается без resume через `thread/read`, `thread/turns/list`, `thread/items/list`. Треды, созданные через app-server, видны владельцу в истории Codex с `source:"vscode"`; проба оставила там 14 тестовых тредов (13 в итоговых evidence и 1 из перезаписанного первого прогона `interrupt`).
- Один процесс app-server обслуживает много тредов; проба держала один тред на процесс, как требует §4.4 для первой версии.

## 7. Риски и открытые вопросы (по убыванию влияния)

1. **Структурированные вопросы только через экспериментальную поверхность.** Нужен `experimentalApi` и plan mode (`collaborationMode`) либо фича `default_mode_request_user_input` в статусе under development; типы `ToolRequestUserInput*` помечены `EXPERIMENTAL`. Plan mode меняет поведение агента: свои developer instructions режима, агент планирует, а не исполняет. Вопрос к заказчику: допускается ли включение экспериментального API для этой обязательной возможности (§10.4 требует изолировать экспериментальные части и не включать их без необходимости; необходимость есть) или Codex получает BLOCKED по AT-G-04. Отдельно: `isBlocking` в plan mode `true`, в Default mode с фичей `false`, смысл флага не документирован.
2. **Весь `codex app-server` поставщик помечает как experimental** («aren't supported for production workloads»). Протокол меняется между версиями. Нужны закрепление версии CLI, контрактные тесты по сгенерированной схеме и образцам из `spikes/codex-probe/tests/fixtures`, проверка схемы при обновлении.
3. **Удержание процессов.** CLI запускает потомков в новых группах и сессиях; SIGKILL обёртки оставляет сирот; `turn/interrupt` не останавливает команду; стабильной операции «остановить фоновые процессы» нет (только экспериментальный `thread/backgroundTerminals/clean`). Для §7.4 и §9.5 нужен ADR с cgroup v2 (например, отдельный systemd scope на сессию) и принудительной остановкой дерева через 10 с.
4. **`turn/start` при активном ходе вливает ввод в этот ход.** Без предусловия «тред свободен» есть окно гонки, в котором обычная отправка станет вмешательством. Воркер должен выдавать следующий ввод строго после `turn/completed`/`idle` и сравнивать `turn.id` из ответа с известными ходами; несовпадение ожиданий считать сбоем протокола и останавливать очередь.
5. **Нет серверной дедупликации `clientUserMessageId`.** Любой повтор после неопределённости создаёт новый ход. Сверка по `thread/items/list` до любой повторной отправки обязательна; отсутствие записи остаётся «приём не подтверждён».
6. **Бесконечные повторы при сетевом отказе.** Ход не завершается сам, приходят `error {willRetry:true}` с растущими паузами (до ~1 мин и больше). Воркеру нужна своя политика: показать «переподключение к модели», через порог предложить прерывание; не объявлять зависание и не повторять ввод.
7. **Изоляция от настроек владельца неполная.** Глобальный `AGENTS.md` всегда загружается; пользовательские `.rules` с `allow` выполняют команды вне песочницы без вопроса (перекрыто guard-правилами только в проекте с trust); `-c` не умеет пути с точками. Вариант: отдельный `CODEX_HOME` воркера со своим входом через `account/login/start` (device code). Требует решения владельца: второй вход в аккаунт и AT-G-05.
8. **Набор решений по команде задаёт сервер.** В проверке `availableDecisions` = `accept`, `acceptWithExecpolicyAmendment`, `cancel`, без `decline`. UI должен строить кнопки только из `availableDecisions`. Семантика `cancel` (отказ с прерыванием хода?) не проверена.
9. **Неоднозначность «решение применено».** `serverRequest/resolved` приходит и при очистке; поздние и повторные ответы игнорируются без ошибки. Состояние «агент применил решение» выводится из пары `serverRequest/resolved` + `item/completed` по `itemId`.
10. **Побочные эффекты в данных владельца.** Треды пишутся в общий `~/.codex/sessions` и видны в истории Codex с `source:"vscode"`; `thread/start.threadSource` позволяет пометить источник для аналитики, но не скрыть тред. Удаление (`thread/delete`) возможно, но по §22 нативная история по умолчанию сохраняется.
11. **Node-обёртка.** `codex` из npm является Node-скриптом, который запускает нативный бинарник из `vendor/`. SIGINT/SIGTERM обёртка передаёт, SIGKILL нет. Запуск нативного бинарника напрямую сэкономил бы слой и прояснил владение процессом, но путь внутри пакета не документирован.

Открытые вопросы без проверки: поведение при SIGTERM во время активного хода; гарантирован ли порядок «запись `userMessage` в историю, затем уведомление»; можно ли передавать доверие проекту и прочие ключи через `thread/start.config` вместо `-c` (для путей с точками); серверная очередь `thread/queue/*` есть только в экспериментальной схеме и в AS не описана.

## 8. Воспроизведение

```bash
cd spikes/codex-probe
cargo test                                  # фрейминг, протокол, корреляция, реальные образцы
cargo clippy --all-targets -- -D warnings
cargo run -- list                           # сценарии и число запросов к модели
cargo run -- diagnostics                    # без расхода квоты; также: lifecycle
cargo run -- <scenario>                     # пишет docs/stage0/evidence/codex/<scenario>.jsonl
codex app-server generate-json-schema --out <dir>   # сверка с schema-0.164.0/*.sha256
```

Каждый сценарий создаёт новый каталог `/tmp/canterlot-stage0/codex/<scenario>/run-<ms>` с `git init` и guard-правилами, запускает app-server с изоляцией из раздела 2.1 и заканчивает файл записью `{"verdict": ..., "reason": ...}`.

## 9. Сверка 0.167.0 (09.11.2026)

Схемы 0.164.0 и 0.167.0 сравнены поле за полем (стабильная и экспериментальная). Манифесты новой версии: `evidence/codex/schema-0.167.0/`; `scripts/codex-schema-check.sh` выбирает каталог по версии CLI, поэтому проходят обе. Стабильная схема: 320 → 322 файла, экспериментальная: 458 → 470. Удалённых файлов нет.

| Изменение | Где | Класс |
| --- | --- | --- |
| `ThreadItem.startedAtMs`, `completedAtMs` (необязательные), `generationId` у картинки | `item/*`, `turn/started`, `turn/completed`, ответы `thread/*` и `turn/start` | добавление, адаптер читает элементы как `Value` |
| `CodexErrorInfo`: `modelNotFound`, `policyCapacityUnavailable` | `error`, `turn/completed` | добавление; неизвестный вид идёт общей веткой ошибки хода |
| `UserInput` вида `audio`: `url` или `fileId` | `turn/start`, `turn/steer` | для нас не важно: аудио адаптер не шлёт, поля `text` и `localImage` не менялись |
| `AdditionalContextEntry.name` | `turn/start`, `turn/steer` | добавление, не используется |
| `turn/start.effort`, `turn/steer.expectedTurnId` | — | без изменений |
| `CommandExecutionApprovalKind` += `humanCommand`, `InitializeCapabilities.requiredHumanApproval` | запрос разрешения на команду | не важно: вид приходит только с этой способностью, адаптер её не объявляет и поле `kind` не разбирает |
| `ChatgptAccount.workspaceName`, учётная запись `gcp` | `account/read` | добавление; адаптер не разбирает ответ |
| `ReviewDelivery` без `detached` | `review/start` | не используется |
| Новые методы `accountSession/*`, `account/gcp/setup`, `thread/memory/read`, `thread/guardianReview`, уведомление `thread/section/changed` | — | не используются; неизвестное уведомление пропускается |
| `HookRunSummary.sourcePath`, `HooksListParams.threadId`, `ProcessExitedNotification.outputDrainTimedOut`, `ThreadUsage.nativeUsageUsdMicros`, `otel` в требованиях конфига | — | не используются |

Ломающих изменений для адаптера нет, правок протокола не понадобилось, минимум остаётся 0.164.0. 0.167.0 внесена в проверенные версии после живого зонда `live_probe` (`codex_e2e.rs`): на 0.164.0 и 0.167.0 одинаково проходят короткий ход, смена усилия (`turn_context`: `low`, затем `medium`), команда оболочки с вливанием `turn/steer` посреди хода (доказательство `codex_user_message_item`, ответ учёл влитый ввод) и прерывание.
