# Этап 0: адаптер OpenCode 2 (`opencode serve --stdio`)

Исследовательский отчёт по интеграции OpenCode 2.1.4 через локальный HTTP/SSE-сервер под владением сопровождения. Доказательства: `docs/stage0/evidence/opencode/*.jsonl`. Поддержка версий OpenCode 1.x не рассматривается (указание владельца).

Обозначения у каждого утверждения:

- **[Ж: файл]** проверено живым прогоном, файл из `docs/stage0/evidence/opencode/`.
- **[И: путь]** прочитано в исходниках `anomalyco/opencode`, коммит `68bc26b7da1990ce10ffecfbc1e09eb13aa5d824` (тег `v2.1.4`), пути от корня репозитория.
- **[Н]** не проверено: предположение или вывод без прямого доказательства.

Итог. Режим интеграции: `opencode serve --stdio --port 0` плюс HTTP и `/api/event`. Запуск, доставка, поток, инструменты, разрешения, вопросы, субагенты, туду, картинки, компакция, прерывание и сверка после падения подтверждены реальным бинарником 2.1.4 с локальной заглушкой OpenAI-совместимого провайдера, а затем настоящей моделью на подписке Go (раздел 9). Не проверены на настоящем провайдере только отказы по квоте и `Retry-After` и всё, что требует заполненного окна. Требование к дизайну: своя база данных, не база владельца (раздел 6, риск 1).

## 1. Версия и стенд

| Параметр | Значение |
| --- | --- |
| CLI | `opencode v2.1.4` (`opencode --version`), npm-пакет `@opencode/cli` 2.1.4, `/usr/bin/opencode` -> `/usr/lib/node_modules/@opencode/cli/bin/opencode.exe` |
| Бинарник | один ELF на 240 МиБ, без обёртки на Node; пути стека `/$bunfs/root/...` указывают на сборку Bun |
| Источник | `anomalyco/opencode`, тег `v2.1.4`, `68bc26b7`, ветка по умолчанию `dev` |
| ОС | Debian 13 (trixie), Linux 6.12.74, x86-64 |
| Дата | 31.10.2026 |
| Учётные данные | нет: `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`, `HOME` и `TMPDIR` каждого прогона указывали во временный каталог |
| Провайдер | локальная заглушка OpenAI Chat Completions (SSE, сценарии, вызовы инструментов, отказы) на `127.0.0.1` |
| Запросы к внешним моделям | 3, все отказ `429 FreeUsageLimitError`, ни токена не потрачено (в evidence последние два); каталог `https://models.opencode.ai` читался без учётных данных |
| Живой смоук | раздел 9: `opencode-go/muse-spark-1.3-contributor`, вход владельца в базу canterlot |

Данные владельца. Логи и база `~/.local/share/opencode/` читались только для раздела 5.3: логи поиском по тексту ошибки, база в режиме `immutable`, только типы и числа без содержимого сообщений. `auth.json` и таблицы учётных данных не открывались.

## 2. Используемая поверхность интерфейса

### 2.1. Сравнение точек входа

| Вход | Что это | Решение | Причина |
| --- | --- | --- | --- |
| `opencode serve --stdio --port 0` + HTTP и SSE | Тот же сервер, что запускает TUI для приватной сессии. Готовность: одна строка `{"url":"http://127.0.0.1:PORT"}` в stdout; пароль приходит в `OPENCODE_PASSWORD`; EOF stdin завершает сервер | **Выбрано** | Процесс принадлежит сопровождению, вход не зависит от клиента, есть идемпотентный приём ввода, разрешения, формы, очередь. [Ж: start_stream.jsonl, lifecycle.jsonl] [И: packages/cli/src/server-process.ts:206, packages/cli/src/services/standalone.ts] |
| `opencode serve` без `--stdio` | Тот же API, порт от 4096, пароль печатается в stdout | Отвергнут | Нет аренды через stdin: смерть сопровождения оставляет сервер живым. [И: packages/cli/src/server-process.ts:208-212] |
| Фоновая служба `opencode service`, `--server` | Управляемый общий сервер, общая база | Отвергнут | Нет владения процессом; при старте подхватывает чужие живые исполнения (раздел 6, риск 1). [Ж: shared_db.jsonl] |
| `opencode acp` | JSON-RPC по stdio | Отвергнут | Лишний слой: процесс запускает дочерний `opencode.exe serve --stdio` и переводит его события. Жёстко `delivery: "steer"`, поэтому нет обычной очереди. [Ж: modes.jsonl] [И: packages/cli/src/acp/turn.ts:257] |
| `opencode run --format json` | Разовый клиент | Отвергнут | Текст приходит целым блоком, дельт нет; разрешения только автоответом (`once` при `--auto`, иначе `reject`); клиент завершается после хода. [Ж: modes.jsonl] [И: packages/cli/src/run/noninteractive.ts:166-183] |
| SDK `@opencode/client` | Клиент TypeScript, сгенерирован из той же схемы | Отвергнут как зависимость | Воркер на Rust. Контракт тот же HTTP; пакет полезен как образец. [И: packages/client, specs/v2/README.md] |

### 2.2. Запуск

```text
env -i PATH=... HOME=... XDG_DATA_HOME=<свой> XDG_CONFIG_HOME=<свой> \
    OPENCODE_PASSWORD=<случайный> OPENCODE_DISABLE_AUTOUPDATE=1 \
    opencode serve --stdio --port 0
```

- Готовность: 0,4-0,6 с до строки в stdout, миграция пустой базы из 50 шагов около 20 мс. [Ж: start_stream.jsonl, lifecycle.jsonl]
- Сервер слушает только `127.0.0.1` на свободном порту. Вход: HTTP Basic, пользователь `opencode`, пароль из окружения; без заголовка 401. `GET /` без пароля отдаёт веб-интерфейс (200), `/api/*` и `/openapi.json` закрыты. [Ж: modes.jsonl, start_stream.jsonl] [И: packages/server/src/auth.ts:20]
- В режиме `--stdio` пароль удаляется из окружения процесса, и `shell` его не наследует; `/proc/<pid>/environ` сервера при этом не очищается, команда того же пользователя может его прочитать. В `shell` есть `OPENCODE_SESSION_ID`. [Ж: lifecycle.jsonl] [И: packages/cli/src/server-process.ts:87]
- `OPENCODE_DISABLE_AUTOUPDATE=1` отключает проверку обновлений. [И: packages/cli/src/services/updater.ts:534] Запускается ли проверка в `serve` без него, не проверялось: переменная задаётся всегда. [Н]

### 2.3. Используемые операции

Источник истины по форме сообщений: `GET /openapi.json` работающего сервера (290 180 байт, 124 пути). Манифест: `evidence/opencode/openapi-2.1.4.sha256`. Набор путей совпал с `packages/protocol/openapi.json`, байты файла и ответа различаются. [Ж: modes.jsonl] Заголовок схемы: «Experimental HttpApi surface»; разделы API подписаны как `Experimental ... routes`, версия схемы `0.0.1`. Это аналог `[experimental]` у `codex app-server`.

| Назначение | Операция | Статус в API |
| --- | --- | --- |
| Диагностика | `GET /api/info` (`version`, `pid`), `/api/agent`, `/api/model`, `/api/mcp` | стабильный путь |
| Сессия | `POST /api/session {id?, title, location, model?, agent?, permissions?}`; `GET`, `PATCH`, `DELETE` | стабильный путь |
| Ввод | `POST /api/session/{id}/prompt {id, text, files?, delivery, resume?, metadata?}` | стабильный путь |
| Очередь | `GET /api/session/{id}/inbox`, `DELETE`, `PATCH /inbox/{id}` | стабильный путь |
| История | `GET /api/session/{id}/message[/{messageID}]`, `/context` | стабильный путь |
| События | `GET /api/event` (SSE) | стабильный путь |
| Прерывание | `POST /api/session/{id}/interrupt` | стабильный путь |
| Настройки | `POST .../model {model}`, `POST .../agent {agent}`, `PATCH /api/session/{id} {permissions}` | стабильный путь |
| Разрешения | `GET .../permission`, `POST .../permission/{id}/reply {decision, message?}` | стабильный путь |
| Формы | `GET .../form`, `POST .../form/{id}/reply {answer}`, `DELETE .../form/{id}?message=` | стабильный путь |
| Компакция | `POST .../compact` | стабильный путь |
| Фоновые shell | `GET /api/shell` (с `pid`, `status`, `metadata.sessionID`), `DELETE /api/shell/{id}`, `GET /api/shell/{id}/output` | стабильный путь |

Экспериментальные и не используются в базовой версии: `/api/experimental/session/{id}/log` (пуст, раздел 4.5), `/api/experimental/session/{id}/instructions/entries`, `/api/experimental/mcp/{server}`, `/api/experimental/session/{id}/wait`. [И: V2_HTTP_API_AUDIT.md]

### 2.4. Изоляция от конфигурации владельца

Каждый прогон шёл с временными XDG-каталогами, `HOME` и `TMPDIR`. Без `TMPDIR` сервер использует общий `/tmp/opencode`, где лежат файлы других сессий. [Ж: start_stream.jsonl] Механизмы и порядок: раздел 5.5.

## 3. Матрица возможностей

| Возможность | Статус | Механизм | Evidence | Ограничения |
| --- | --- | --- | --- | --- |
| Диагностика без платной задачи | PASS | `opencode --version`; `GET /api/info`, `/api/agent`, `/api/model`, `/api/mcp` без обращения к модели | `start_stream.jsonl`, `models.jsonl` | Сразу после старта `/api/agent` пуст: каталог заполняется асинхронно (раздел 6) |
| Начало разговора, native ID | PASS | `POST /api/session` -> `ses_...`; можно задать свой `id`, повтор возвращает ту же сессию | `resume.jsonl` | `session.create` принимает несуществующую модель, отказ только на ходе (`provider.no-route`) |
| Текстовый ввод | PASS | `POST .../prompt {id, text}` | `start_stream.jsonl` | `id` обязан начинаться с `msg_`, UUID без префикса -> 400 |
| Несколько вводов | PASS | Последовательные и в активном ходе, `delivery: queue` и `steer` | `delivery_ack.jsonl` | Значение по умолчанию `steer`: без явного `queue` ввод вливается в идущую работу |
| Потоковый текст | PASS | `session.text.started/delta/ended`, `session.reasoning.*` | `start_stream.jsonl` | Дельты склеиваются сервером; `ended` несёт полный текст |
| Инструменты и ошибки | PASS | `session.tool.input.*`, `session.tool.called`, `session.tool.success`, `session.tool.failed` | `tools.jsonl` | Ненулевой выход `shell` приходит как `success` с `metadata.exit` |
| Разрешения | PASS | `permission.asked` -> `POST .../reply`; правила сессии `permissions` | `permissions.jsonl` | По умолчанию вопросов нет: агент `build` разрешает `*`; запрос живёт только в памяти |
| Структурированные вопросы | PASS | Инструмент `question` -> `form.created` -> `POST .../form/{id}/reply` | `question.jsonl` | Неполный ответ принимается без ошибки; отмена без `message` завершает ход |
| Прерывание | PASS | `POST .../interrupt`, `session.execution.interrupted {reason:"user"}` за 25 мс | `interrupt.jsonl` | Фоновые shell выживают, снимаются `DELETE /api/shell/{id}` |
| Возобновление | PASS | Новый процесс на той же базе, тот же `ses_...` | `resume.jsonl`, `crash.jsonl` | Подвисший инструмент закрывается только следующей работой |
| Доказуемая доставка | PASS | Ответ 200 с тем же `id` после записи в `session_inbox`; повтор идемпотентен | `delivery_ack.jsonl` | `synchronous=NORMAL`: выдержит падение процесса, сбой питания не проверен |
| Отказ внешнего сервиса | PASS | `session.retry.scheduled`, `session.step.failed`, `session.execution.failed` с типом ошибки | `service_failure.jsonl`, `real_free_model.jsonl`, `live/invalid_request_stuck.jsonl`, `live/auth.jsonl` | На настоящем провайдере проверены 401, 400 и 429 бесплатных моделей; 429 квоты Go только на заглушке |
| Фоновые работы | PASS | `shell {background:true}`, фоновый `subagent`; `GET /api/shell`, `DELETE /api/shell/{id}` | `interrupt.jsonl`, `lifecycle.jsonl` | Завершение и снятие будят сессию отдельным запуском (раздел 5.1) |
| Replay потока событий | **FAIL** | `/api/event` без replay; `/log` на CLI пуст | `reconnect.jsonl` | Восстановление только по проекциям (раздел 4.5) |

## 4. Семантика доставки

### 4.1. Что доказывает «принято агентом»

Факты одного ввода. [Ж: start_stream.jsonl, delivery_ack.jsonl]

| Шаг | Факт | Что доказывает |
| --- | --- | --- |
| 0 | `POST .../prompt {id:"msg_..."}` | ничего |
| +6-16 мс на прогретом сервере, первый ввод 0,5 с | 200, `data.id` равен нашему `id` | вход записан в `session_inbox` одной транзакцией; переживает SIGKILL [Ж: delivery_ack.jsonl] |
| следом | `session.inbox.enqueued` (seq 1) | то же, для слушателей потока |
| следом | `session.execution.started` | начат период работы |
| следом | `session.inbox.delivered {inboxID}` | вход стал видимым сообщением, строка очереди снята |
| следом | `session.step.started {assistantMessageID}` | запрос к модели отправлен |

Правило адаптера: `Accepted` при ответе 200 с совпавшим `id`. Это сильнее, чем у двух других адаптеров: приём синхронный, ответ приходит за миллисекунды, отдельное `InputAccepted` нужно только после обрыва соединения. Запись в SQLite идёт с `journal_mode=WAL`, `synchronous=NORMAL`. [И: packages/core/src/database/database.ts:40-41] Сбой питания между ответом и сбросом журнала не проверялся. [Н]

Идемпотентность. [Ж: delivery_ack.jsonl] [И: specs/v2/session.md]

- Повтор с тем же `id` и тем же текстом: 200, запись та же, модель не вызывается.
- Повтор с тем же `id` и другим текстом: 200, побеждает первая запись.
- Тот же `id` в другой сессии: 409 `ConflictError`.
- Четыре одновременных `POST` с одним `id`: все 200, одно `session.inbox.enqueued`, один запуск.
- Клиентский `id` в виде `msg_<32 hex>` не нарушает порядок истории: проверены значения ниже и выше серверных. Порядок задаёт `seq`, а не сортировка `id`.
- Повтор уже доставленного `id` всё равно будит исполнение: появляется пустая пара `execution.started/succeeded` и строка `idle`, модель не вызывается.
- `id` отменённого ввода можно занять снова, новая запись примет новый текст.

Отказ при приёме синхронный: нечитаемый файл или неподдерживаемый URI дают 400 `InvalidRequestError` до записи, неизвестная сессия 404. Это `TransmitOutcome::Rejected`. [Ж: image_input.jsonl, resume.jsonl]

### 4.2. Решение по разрешению или вопросу

- Запрос: `permission.asked {id:"per_...", sessionID, action, resources, save, metadata?, source:{type:"tool", messageID, id}}`. `source.id` связывает его с вызовом инструмента. [Ж: permissions.jsonl]
- Ответ: `POST /api/session/{sessionID}/permission/{id}/reply {"decision":"once"|"always"|"reject","message"?}` -> 204. Проверка «применено»: `permission.replied` и терминальное событие инструмента. Повтор на закрытый запрос: 404 `PermissionNotFoundError`, в отличие от тихого игнорирования у Codex. [Ж: permissions.jsonl]
- `reject` без `message` завершает ход: `session.tool.failed {type:"aborted"}`, `session.step.failed`, `session.execution.interrupted {reason:"shutdown"}`. Причина `shutdown` здесь не означает остановку сервера, а claim остаётся неснятым. С `message` ошибка уходит модели, ход продолжается. [Ж: permissions.jsonl] [И: packages/core/src/session/execution.ts:47,127, packages/core/src/permission.ts:267]
- `reject` снимает все ожидающие запросы этой сессии, а не один. [Ж: permissions.jsonl] [И: packages/core/src/permission.ts:267-286]
- `always` пишет правило в базу на весь проект, в примере маска `echo *`, и действует на другие сессии. [Ж: permissions.jsonl] Адаптер не должен его отправлять.
- `permission.asked` и `permission.replied` эфемерны, запрос хранится только в памяти процесса. [И: packages/schema/src/permission.ts, packages/core/src/permission.ts] Прерывание закрывает запрос без `permission.replied`, виден только `tool.failed {aborted}`; поздний ответ получает 404. [Ж: interrupt.jsonl]
- Форма вопроса: раздел 5.2.

### 4.3. Ввод во время активного хода

- `delivery:"steer"` (значение по умолчанию) доставляется на следующей границе шага и попадает в тот же ход. [Ж: delivery_ack.jsonl]
- `delivery:"queue"` ждёт конца работы, затем выполняется **в том же периоде** `execution`: терминальное событие одно на оба ввода. Границы вводов видны только по `session.inbox.delivered`. [Ж: delivery_ack.jsonl]
- `resume:false` записывает ввод без запуска; после SIGKILL он остаётся в очереди и сам не исполняется. [Ж: delivery_ack.jsonl]
- `DELETE .../inbox/{id}` идемпотентен, 204 даже для доставленного: успех отмены надо проверять по `GET .../inbox` до вызова или по событию `session.inbox.cancelled`. [Ж: delivery_ack.jsonl]
- Обычная отправка адаптера: всегда `delivery:"queue"`. Гонка «ход только что начался» тогда не превращает её во вмешательство, что закрывает риск 4 отчёта Codex. Явное вмешательство: `steer`.

### 4.4. Сбой между записью и подтверждением

- Обрыв до ответа: `Undetermined`. Сверка только чтением: строка в `GET .../inbox` или `GET .../message/{id}` даёт положительное доказательство. [Ж: delivery_ack.jsonl]
- Если обе проверки пусты на том же живом сервере, запрос мог ещё быть в обработке. После смерти сервера незавершённый `POST` не появится, поэтому `non_acceptance_proven` при этом условии достижим, чего нет у других адаптеров. [Н: гонка на уровне HTTP не воспроизводилась]
- Автоматический повтор технически безопасен, но `delivery.md` D-6 его запрещает, и пустой запуск от повтора дал бы ложный «конец хода». Сверка остаётся только чтением.
- После SIGKILL решение по разрешению повторить нельзя: запрос был в памяти, а инструмент останется в состоянии `running` (раздел 6).

### 4.5. Поток событий, порядок, переподключение

- `GET /api/event` отдаёт события всех локаций сервера, включая дочерние сессии; первым идёт `server.connected`, каждые 15 с `: heartbeat`. [Ж: start_stream.jsonl] [И: packages/server/src/handlers/event.ts:21]
- Устойчивые события несут `durable:{aggregateID, seq, version}`, `seq` растёт в пределах сессии. Эфемерные (`*.delta`, `session.tool.progress`, `session.usage.updated`, `permission.*`, `form.*`, `shell.*`) номера не имеют. [Ж: start_stream.jsonl] [И: packages/schema/src/session-event.ts]
- **Replay нет.** `Last-Event-ID` игнорируется. [Ж: delivery_console.jsonl] `/api/experimental/session/{id}/log` на сервере CLI возвращает только `log.synced {seq}`, ни одного события, в том числе с `follow=true`: шина работает с `persist=false`, а CLI параметр не передаёт. Значение `true` включено только в сборке для workerd. [Ж: reconnect.jsonl] [И: packages/core/src/bus.ts:208, packages/server/src/workerd.ts:69, packages/cli/src/server-process.ts] Таблица `event` в рабочей базе владельца пуста, что согласуется с этим.
- Отстающий подписчик отключается при переполнении очереди в 4096 кадров, остальные продолжают. [И: specs/v2/event-stream-architecture.md] Вживую не проверялось: `shell` кадров не шлёт, на ход приходится несколько десятков дельт. [Н]
- Восстановление после разрыва: подписаться заново **раньше** чтения, затем снять проекции `GET .../message`, `.../inbox`, `.../permission`, `.../form`, `/api/session/active`. Дельты потеряны, но `session.text.ended` и проекция несут полный текст. [Ж: reconnect.jsonl, crash.jsonl]

Запись `recovery.jsonl` (31.10, срез 13 адаптера, заглушка провайдера): разрыв потока посреди хода и за концом хода, запрос и форма в разрыве, потомок, родившийся в разрыве, EOF stdin и SIGKILL посреди инструмента и текста, ввод в очереди при перезапуске. Что она добавляет. [Ж: recovery.jsonl]

- `GET .../message` пишет текст и рассуждения только по концу блока: посреди потока там `{"type":"text","text":""}`, дельты в проекцию не попадают. Части без `ordinal`: это номер среди частей того же вида в сообщении. [И: packages/core/src/session/runner/publish-llm-event.ts]
- EOF stdin посреди текста: сервер сам закрывает шаг, проекция хранит накопленный текст, `finish: "error"`, `idle` не появляется. SIGKILL: текст пуст, `finish` нет.
- Ввод в очереди переживает и EOF, и SIGKILL, виден в `.../inbox` нового сервера, сам не исполняется; `DELETE .../inbox/{id}` снимает его.
- `GET /api/session?parentID=` отдаёт потомков, родившихся в разрыве; `.../permission` и `.../form` повторяют форму `permission.asked` и `form.created`.
- `AGENTS.md` наблюдатель перечитывает с дебаунсом 100 мс: запись за 100 мс до ввода первый шаг не видит, за 200 мс видит, сигнала нет. Удаление файла приходит модели как `Previously loaded instructions no longer apply`. [И: packages/core/src/config/plugin/instruction.ts:207]
- `OPENCODE_CONFIG` принимает один файл, а глобальный каталог читает и `opencode.json`, и `opencode.jsonc`, и плагины, в том числе через символические ссылки.
- `mcp.tools.changed` в поток `/api/event` не идёт.

## 5. Необязательные возможности и отображение

### 5.1. Поток и `AdapterEvent`

| OpenCode | `AdapterEvent` | Примечание |
| --- | --- | --- |
| `session.step.started {assistantMessageID, agent, model}` | `Phase::Generating` | Источник модели и агента для блока; шаг один, ID на шаг |
| `session.text.started/delta/ended {assistantMessageID, ordinal}` | `BlockStarted{Text}`, `BlockDelta`, `BlockFinal` | Ключ блока (`assistantMessageID`, `ordinal`); UUIDv5 от ключа даёт стабильный `block_id` |
| `session.reasoning.*` | те же с `BlockKind::Reasoning` | `reasoning.ended` может прийти после `text.started`: блоки не вложены [Ж: start_stream.jsonl] |
| `session.tool.input.started {id, name}` | `BlockStarted{ToolCall}` | Единственное событие с именем инструмента; `id` уникален только в шаге, ключ (`assistantMessageID`, `id`) [И: specs/v2/session.md] |
| `session.tool.called {input}` | `Phase::RunningTool`, сводка | Вызов записан до побочных эффектов |
| `session.tool.progress {metadata}` | `Phase`, связь с субагентом | Для `shell` только `shellID`: вывода нет [Ж: tools.jsonl] |
| `session.tool.success {content, metadata}` | `BlockStarted/Final{ToolResult}` | `shell`: `metadata.exit`; `edit`: `metadata.files[].patch`; `read` картинки: `content[{type:"file", uri:"data:..."}]` |
| `session.tool.failed {error, content?}` | `ToolResult` с ошибкой | Может прийти раньше соседнего `success`: параллельные вызовы [Ж: tools.jsonl] |
| `session.step.ended {finish, tokens, cost}` | накопление `ModelUsage` | Завершает шаг, идёт после всех инструментов |
| `session.step.failed {error}` | `Warning` или `Error` | Один шаг, ход может продолжиться повтором |
| `session.retry.scheduled {attempt, at, error}` | `Phase::Generating` с пометкой | Время следующей попытки в `at` |
| `session.execution.succeeded/failed/interrupted` | `RunFinished` | Терминал периода работы; `failed` может прийти без `step.failed` (`provider.no-route`) [Ж: service_failure.jsonl] |
| `session.compaction.started/ended/failed` | `Phase::ContextMaintenance` | `reason: auto, manual` |
| `permission.asked`, `permission.replied` | `RequestOpened{permission}`, `RequestClosed` | Закрытие также по `tool.failed`, см. 4.2 |
| `form.created`, `form.replied`, `form.cancelled` | `RequestOpened{question}`, `RequestClosed` | Только `metadata.kind=="question"` |
| `session.tool.success` у `todowrite` | `TodoUpdated` | Раздел 5.4 |
| `session.created {parentID}` и `tool.progress.metadata.sessionID` | ключ дорожки | Раздел 5.3 |
| `session.instructions.updated`, `session.inbox.*`, `session.step.streamed`, `session.usage.updated`, `shell.*` | служебные | `session.usage.updated` накопительный по сессии |

Токены и стоимость. [Ж: start_stream.jsonl] Заглушка вернула `prompt 100, completion 20, cached 30, reasoning 5`; OpenCode записал `input 70, output 15, reasoning 5, cache.read 30`. Значит, `input` и `output` уже без кэша и рассуждений, и суммирование без учёта этого завысит расход. `cost` в USD считается по каталогу и на подписке не равен списанию. [И: packages/schema/src/session-event.ts] `RunFinished.usage`: суммировать `step.ended` по паре (модель, агент). `context_tokens`: `input + cache.read + cache.write` последнего шага; окно берётся из `limit.context` каталога, в событиях его нет.

Причины завершения шага: `stop, length, tool-calls, content-filter, error, unknown`. [И: packages/schema/src/llm.ts] `length` сам продолжает ход: сервер вставляет синтетическое сообщение и делает ещё один шаг. `content-filter` даёт `step.failed` и `execution.failed` с типом `provider.content-filter`. [Ж: service_failure.jsonl]

Запуск без ввода. Завершение фонового `shell` или фонового субагента ставит в очередь `synthetic` и само запускает новый период `execution`. То же делает снятие через `DELETE /api/shell/{id}`: процесс убит, а модель получает уведомление и отвечает ещё раз. [Ж: subagent.jsonl, lifecycle.jsonl] `RunFinished.input_command_id` тогда не к чему привязать: вопрос вынесен к владельцу.

### 5.2. Вопросы

Инструмент `question`, у агента `build` разрешён, у `general` и `explore` запрещён. [И: packages/core/src/plugin/agent.ts] Вход `{questions:[{question, header, options:[{label, description}], multiple?}]}`. [И: packages/core/src/tool/plugin/question.ts]

Проводной формат. [Ж: question.jsonl]

```json
{"type":"form.created","data":{"form":{"id":"frm_...","sessionID":"ses_...","title":"Questions",
 "metadata":{"kind":"question","tool":{"messageID":"msg_...","id":"call_..."}},
 "fields":[{"key":"q0","title":"Colour","description":"Pick a colour?","type":"string",
            "options":[{"value":"Blue","label":"Blue","description":"cold"}],"custom":true},
           {"key":"q1","type":"multiselect","options":[...],"custom":true}]}}}
```

- Ответ: `POST .../form/{id}/reply {"answer":{"q0":"Blue","q1":["A","B"]}}` -> 204. Свободный текст разрешён всегда (`custom:true`).
- `{"answer":{"q0":"Blue"}}` без `q1` принят с 204, модель получила `"Unanswered"`. Следующий ответ: 409 `FormAlreadySettledError`. Полноту проверяет адаптер, это делает `valid_decision`.
- `DELETE .../form/{id}` без `message`: ход завершён как при `reject` без сообщения. С `?message=...`: `tool.failed {tool.execution}`, модель продолжает.
- В `valid_decision` лимит 4 вопроса; у OpenCode лимита нет. [И: packages/core/src/tool/plugin/question.ts] При большем числе адаптер обязан отказать через `DELETE` с сообщением, иначе ход зависнет. [Н: сценарий не прогонялся]
- Формы бывают и от MCP-элицитации, с `sessionID:"global"`. [И: packages/schema/src/form.ts] Их отклонять с диагностикой.

### 5.3. Субагенты

Инструмент `subagent {agent, description, prompt, model?, sessionID?, background?}`. Агенты: `general`, `explore`. Глубина по умолчанию 1, `experimental.subagent_depth` меняет её. [И: packages/core/src/tool/plugin/subagent.ts:17,129]

- Потомок это отдельная сессия: `session.created {parentID}`, затем обычные события с её `sessionID` в том же `/api/event`. Связь с вызовом: `session.tool.progress {id, metadata:{sessionID, status:"running"}}` у родителя. [Ж: subagent.jsonl]
- Ключ дорожки: блок вызова `subagent`. Порядок в захватах: `tool.called`, затем `session.created`, затем `tool.progress`. Сервер публикует `progress` раньше ввода потомка, поэтому событий потомка до него нет и держать нечего. [И: packages/core/src/tool/plugin/subagent.ts] В проекции `running`-вызова `metadata` пуста: `progress` эфемерен, после разрыва связь даёт `metadata` результата вызова. [Ж: recovery.jsonl] [И: packages/core/src/session/message-updater.ts]
- Запрос разрешения потомка несёт `sessionID` потомка, ответ идёт на его путь. [Ж: subagent.jsonl]
- Потомок наследует `permissions` родителя при создании, режимы `ask` и `read_only` не обходятся: в `ask` shell потомка спросил разрешение, в `read_only` инструмента у потомка не было. [Ж: subagent.jsonl] [И: packages/core/src/session.ts:298-301] Передаётся ли позднее `PATCH` родителя уже созданным потомкам, не проверено. [Н]
- `interrupt` родителя снимает основного и фонового потомка и их shell. [Ж: subagent.jsonl]
- Фоновый потомок: родитель получает `execution.succeeded` раньше потомка; итог приходит `synthetic`-вводом и запускает новый период. [Ж: subagent.jsonl]
- Модель и агент известны из `session.step.started` потомка.

### 5.4. Туду

Инструмент `todowrite` есть только при `experimental.todo: true`; без флага его нет в списке инструментов. [Ж: todo.jsonl] [И: packages/core/src/tool/plugin/todo.ts:11,50] Своего события нет. Источник: `session.tool.success.metadata.todos` с полным списком `[{content, status}]`, имя инструмента надо помнить с `session.tool.input.started`. Статусы: `pending, in_progress, completed, cancelled`; `cancelled` в `TodoStatus` отсутствует. [Ж: todo.jsonl] Дорожка: `sessionID` события через связь субагента. У субагента туду работает так же. [И: там же] Проверено живым тестом L2 `real_child_todo_list_goes_to_the_lane_of_its_call` на заглушке провайдера.

### 5.5. Инструкции и конфигурация

Карта по канарейкам, рабочий каталог `~/proj/repo/sub`. [Ж: instructions.jsonl]

| Источник | Читается | Отключается |
| --- | --- | --- |
| `$XDG_CONFIG_HOME/opencode/AGENTS.md` | да, первым | заменой каталога `OPENCODE_CONFIG_DIR` |
| `AGENTS.md` от рабочего каталога вверх до корня репозитория | да, ближний раньше | `OPENCODE_CONFIG_PROJECT_DISABLE=1` снимает все вместе |
| **`AGENTS.md` выше корня репозитория и `~/AGENTS.md`** | да, если каталог лежит под `$HOME`: обход идёт до дома, а не до корня git | вместе с репозиторными; отдельно только через `OPENCODE_TEST_HOME` |
| `CLAUDE.md` в репозитории, `~/.claude/CLAUDE.md` | **нет** | не нужно |
| `~/.agents/AGENTS.md`, `.opencode/AGENTS.md` | нет | не нужно |
| Навыки из `~/.claude/skills` | да, в список | `OPENCODE_DISABLE_CLAUDE_CODE=1` |

[И: packages/core/src/config/plugin/instruction.ts:40-41, packages/core/src/config/discovery.ts, packages/cli/src/server-process.ts]

Способы внедрить пресет, проверены вживую. [Ж: instructions.jsonl]

1. `OPENCODE_CONFIG_DIR=<каталог исполнения>` с `AGENTS.md` пресета плюс `OPENCODE_CONFIG=<путь к конфигу>`: глобальный каталог заменён целиком, репозиторные `AGENTS.md` остаются. Репозиторий не меняется. **Рекомендуется.**
2. `OPENCODE_CONFIG_CONTENT` с `instructions:[путь]`: добавляет файл, глобальный `AGENTS.md` остаётся.
3. `agents.build.system`: **заменяет весь промпт харнесса**, инструкции по инструментам и среде пропадают. Не подходит.
4. `PUT /api/experimental/session/{id}/instructions/entries/{key}`: в свежей сессии попадает в системный промпт, в начатой приходит пользовательским сообщением. Экспериментальный путь.

Смена без перезапуска. Правка `AGENTS.md`, в том числе в `OPENCODE_CONFIG_DIR`, подхватывается следящим процессом: на следующем шаге модель получает `<system-update>` с новым текстом, системный промпт не меняется. Перезапуск не нужен. [Ж: instructions.jsonl] Сессионные инструкции версионируются и поэтому не пропадают из контекста после компакции. [И: specs/v2/session.md]

Приоритет конфигурации: система, глобальный каталог, явный файл, каталоги проекта, `OPENCODE_CONFIG_CONTENT`. [Ж: instructions.jsonl для порядка записей `/api/config`] [И: packages/core/src/config/discovery.ts] Точный порядок слияния отдельных ключей не проверялся. [Н]

### 5.6. Модели

- `GET /api/model` отдаёт только доступные модели: без ключа бесплатные и заданные в конфиге, с `OPENCODE_API_KEY` дополнительно `opencode-go` (30) и `opencode` (80). [Ж: models.jsonl] Полный каталог: 234 провайдера с `https://models.opencode.ai`, кэш в таблице `kv`. [И: packages/core/src/models-dev.ts:280]
- Имя модели: `provider/model`, `Model.Ref {id, providerID, variant?}`. Усилие называется вариантом: `low, medium, high, xhigh, max`, у разных моделей свой набор. [Ж: models.jsonl]
- **`settings.apiKey` приходит в ответах `/api/model` и `/api/provider` открытым текстом**, если ключ задан в конфиге. [Ж: start_stream.jsonl, значение в evidence заменено] Адаптер берёт только белый список полей и не пишет ответ в журнал.
- Запись каталога для PWA: `{id:"opencode-go/muse-spark-1.3-contributor", title:"Muse Spark 1.3 Contributor", efforts:["minimal","low",...], limit_context:1048576, images:true}`. Поддержка картинок задаётся на модель (`capabilities.input`).
- `POST .../model {model:{id, providerID, variant}}`: действует со следующего ввода, идущий период не меняет. [Ж: models.jsonl] Несуществующий вариант принимается с 204 и падает на ходе `provider.no-route`. Проверку делает адаптер. [Ж: models.jsonl]
- Запроса `prompt` с моделью нет: смена только отдельным вызовом. [И: packages/protocol/src/groups/session.ts]
- Агенты: `build`, `plan`, `general`, `explore`, скрытые `compaction`, `title`, `summary`. `POST .../agent {agent}` работает со следующего запуска; `plan` не убирает `edit` из списка инструментов, ограничивает правилами. [Ж: models.jsonl]
- Без `title` при создании сессии идёт **лишний запрос к модели** за заголовком, расход попадает в итог сессии, но не в `step.ended`. [Ж: models.jsonl] Адаптер всегда задаёт `title`.

### 5.7. Разрешения и режимы canterlot

Вычисление: правила агента, затем `session.permissions`, последнее совпадение побеждает; без совпадения `ask`. [И: packages/core/src/permission.ts:74, packages/schema/src/session.ts] Встроенное правило `build`: `{"*","*","allow"}` плюс `ask` для `*.env`. [И: packages/schema/src/agent.ts:47] Действия: `shell, read, edit` (им же управляется `write`), `glob, grep, question, subagent, skill, webfetch, websearch, external_directory, todowrite`, MCP: `<сервер>_<инструмент>`. [И: packages/core/src/tool/plugin/*.ts, packages/core/src/file-access.ts]

| Режим canterlot | Правила сессии | Проверка |
| --- | --- | --- |
| `bypass` | `[{"*","*","allow"}]` | Вопросов нет, `.env` читается без запроса [Ж: permissions.jsonl, subagent.jsonl] |
| `ask` | `[{"*","*","ask"},{"question","*","allow"},{<canterlot>,"*","allow"},{"todowrite","*","allow"}]` | `shell`, `read`, `edit`, `external_directory` спрашивают [Ж: permissions.jsonl] |
| `read_only` | `[{"*","*","deny"}, allow для read, glob, grep, question, subagent]` | Запрещённые инструменты не предлагаются модели; прямой вызов: `No tool named "shell"` [Ж: permissions.jsonl] |
| `owner` | не задавать `permissions` | Действуют правила конфига владельца; при встроенном умолчании `shell`, `write` и чтение `/etc/hostname` прошли без запросов [Ж: lifecycle.jsonl] |

Смена режима: `PATCH /api/session/{id} {permissions}` -> 204, новое правило действует на следующей проверке, `session.permissions` подтверждает. [Ж: permissions.jsonl] Пометка «Разрешено правилами CLI»: вызов дошёл до `session.tool.success` без `permission.asked` с тем же `source.id`, режим не `bypass`.

Без `question` и `subagent` в разрешающем списке `read_only` теряет эти инструменты: список задан явно.

### 5.8. MCP

- Подключение на сессию: `mcp.servers.canterlot` в `OPENCODE_CONFIG_CONTENT` при запуске. Вид записи: `{"type":"local","command":[...],"environment":{...},"codemode":false}`. Переменные окружения доходят, рабочий каталог совпадает с локацией. [Ж: mcp.jsonl]
- **`codemode:false` обязателен.** Без него инструмент MCP не попадает в список инструментов, доступен только изнутри `execute`, прямой вызов падает `No tool named`. [Ж: mcp.jsonl]
- Имя инструмента: `canterlot_canterlot_limits`, то есть `<сервер>_<инструмент>`; при `SERVER_NAME=canterlot` имя удваивается. [Ж: mcp.jsonl] [И: packages/core/src/tool/mcp.ts:23]
- Инструкции MCP-сервера (`instructions`) доходят до системного промпта, в отличие от Codex. [Ж: mcp.jsonl]
- **Гонка холодного старта.** Первый шаг не видит инструмент: подключение MCP идёт асинхронно, а `mcpTools.flush` ждёт только начальный проход. После статуса `connected` инструмент появляется ещё через интервал порядка 100 мс: дебаунс `mcp.tools.changed`. Замер: ожидание 0 и 50 мс после `connected` дают «нет», 100, 200, 500 мс дают «да». Публичного сигнала нет. [Ж: mcp.jsonl] [И: packages/core/src/tool/mcp.ts:136, packages/core/src/session/context.ts:132]
- Сервер MCP запускается на локацию, а не на сессию. При схеме «один сервер на сессию» разницы нет. [И: packages/core/src/mcp/index.ts]
- Добавление на ходу: `PUT /api/experimental/mcp/{server} {"config":{...}}` работает, экспериментально. [Ж: resume.jsonl]

### 5.9. Вложения и картинки

- `files:[{uri, name?}]`: `file://` и `data:`, `https:` отвергается с 400. Файл читается при приёме, копируется в базу в base64, лимит 20 МиБ на вложение. [Ж: image_input.jsonl] [И: packages/core/src/session/prompt.ts:201]
- Картинка уходит модели как `image_url`, текстовый файл вставляется в сообщение с пометкой `Attached file:`, абсолютный путь виден модели. [Ж: image_input.jsonl]
- **Двоичный файл молча исчезает**: приём 200, модель ничего не получает. PDF и картинка у модели без такого ввода превращаются в текст `ERROR: Cannot read ... Inform the user`. [Ж: image_input.jsonl] Значит, как у Codex: двоичное кладётся на диск, путь идёт текстом. Лимит canterlot 100 МиБ выше лимита 20 МиБ: крупные файлы тоже только путём.
- Ответ на `POST .../prompt` возвращает вложения в base64; читать его с учётом размера.
- Картинка, на которую посмотрел агент: `session.tool.success.content` содержит `{type:"file", uri:"data:image/png;base64,...", mime, name}`, байты прямо в событии. Это `MediaSource::Bytes`. Модель получает картинку отдельным сообщением `user`. [Ж: image_input.jsonl]

### 5.10. Лимиты

- API квот в OpenCode нет: ни в `packages/protocol`, ни в ядре, серверная часть консоли в репозитории отсутствует. [И: packages/protocol/src/groups, packages/core/src/plugin/provider/opencode.ts] Заголовки `x-ratelimit-*` и `anthropic-ratelimit-*` разбираются только в контекст ошибки запроса; в публичную `SessionError` доходит лишь `retryAfterMs`. [И: packages/ai/src/route/executor.ts:47-56,118, packages/schema/src/session-error.ts]
- Для Go доступно: (а) отказ `429` с типом `GoUsageLimitError` даёт `provider.quota` и `retryAfterMs` из `Retry-After` в `session.execution.failed.error`; форма проверена на заглушке, настоящий ответ не наблюдался. [Ж: service_failure.jsonl] [И: packages/ai/src/provider-error.ts:124-128] (б) `cost` на шаг по каталогу; у `muse-spark-1.3-contributor` цены `input 0.1 / output 0.2` за миллион токенов (снимок каталога `models.opencode.ai` от 31.10, в evidence не вошёл). [Ж: снимок каталога в изолированном прогоне]
- Настоящий ответ бесплатных моделей: `provider.free-limit`, `retryAfterMs: 80814000` (22,4 ч), тело `FreeUsageLimitError`. [Ж: real_free_model.jsonl]
- Окна Go по тексту консоли: «Estimated requests per 5 hours and monthly usage limits by model». [И: packages/console/app/src/i18n/en.ts:219] Значит, есть месячное окно, которого нет в `LimitWindow`.
- `codexbar usage --provider opencodego` есть в справке 0.79.0 и соответствует `pilot.md` §7.1. Не запускался: читает учётные данные владельца. [Н]
- Решение владельца 31.10: только `Reached` из отказа, без `codexbar` и без месячного окна. Окна в отказе нет: ожидание длиннее пяти часов адаптер считает недельным.

### 5.11. Компакция, повторы, переполнение

- Порог автокомпакции: 85 % окна, не менее 16 тысяч запаса; `compaction.buffer` меняет. [И: packages/core/src/session/compaction.ts:128] Событие `session.compaction.started {reason:"auto"}`, полная история сохраняется, `/context` показывает сокращённую. [Ж: compaction.jsonl]
- Отказ с текстом из списка переполнения (`prompt is too long` и др.) до первых данных: сам запускает компакцию и повторяет шаг, `step.failed` не публикуется. [Ж: compaction.jsonl] [И: packages/core/src/session/runner/step.ts:158, packages/ai/src/provider-error.ts:18]
- Ручная: `POST .../compact {id?, delivery?}`, элемент очереди со своим `id`, повтор с тем же `id` возвращает тот же элемент. [И: packages/protocol/src/groups/session.ts, packages/core/src/session/inbox.ts] Шаблон сводки обязателен: иначе `session.compaction.failed`. [Ж: compaction.jsonl] Настоящие кадры: успех `session.compaction.delta`, затем `session.compaction.ended {model, text, tokens}`; отказ провайдера `session.compaction.failed {error}` и **`session.execution.succeeded`**, блока ошибки хода нет; нечего сжимать: `compaction.unavailable` «Nothing to compact yet»; прерывание: `session.compaction.failed {aborted, "Compaction cancelled"}`, затем `session.execution.interrupted {user}`. В проекции `{type:"compaction", status:"completed"|"failed", reason, model?, summary?, error?, metadata:{keepID}?}`. [Ж: live/compaction.jsonl, live/invalid_request_stuck.jsonl]
- Тип ошибки переполнение не отличает: `SessionError` это `provider.invalid-request`, признак `context-overflow` наружу не выходит. [И: packages/core/src/session/to-session-error.ts] Адаптер берёт признак из шаблонов текста версии 2.1.4; второе переполнение после сжатия проверено L2 `real_overflow_after_compaction_exhausts_the_context`.
- Повторы: до 10, паузы 2, 4, 8, затем по 10 с, `Retry-After` до 15 минут; живьём 6 подряд за 41 с. `specs/v2/session.md:84` с числом 4 устарел. [Ж: service_failure.jsonl] [И: packages/core/src/model-retry.ts:12,27]
- Не повторяются: `Authentication, QuotaExceeded, ContentPolicy, InvalidRequest, UnsupportedOperation, NoRoute, Timeout`. Неопознанные ошибки **повторяются**. [И: packages/ai/src/provider-error.ts:85]
- Обрыв потока: `provider.invalid-output`, новый шаг. [Ж: service_failure.jsonl]

Типы `session.execution.failed.error.type`, проверенные вживую: `provider.auth`, `provider.rate-limit`, `provider.quota`, `provider.free-limit`, `provider.internal`, `provider.invalid-request`, `provider.invalid-output`, `provider.content-filter`, `provider.no-route`. [Ж: service_failure.jsonl, real_free_model.jsonl]

На настоящем провайдере: `provider.free-limit` [Ж: real_free_model.jsonl], `provider.auth` 401 «Upstream request failed: Invalid credential» [Ж: live/auth.jsonl], `provider.invalid-request` 400 «Upstream request failed: Model is unavailable.», текст одинаков от попытки к попытке, идентификаторов запроса в нём нет [Ж: live/invalid_request_stuck.jsonl].

### 5.12. Отказ «invalid parameters»

Наблюдение на рабочей базе владельца (только числа). Сообщение дословно: `The request contains invalid parameters. Check the request body for any errors or inconsistencies.`, HTTP 400, тело `{"model":...,"error":{"code":null,"message":...,"param":null,"type":"invalid_request_error"}}`, маршрут `opencode-go/muse-spark-1.3-contributor`. В исходниках OpenCode этого текста нет: он приходит снаружи. [И: поиск по всему репозитории тега]

| Сессия | Строк | Вызовов инструментов | Компакций | Пик контекста | Элементов запроса (оценка) | Итог |
| --- | --- | --- | --- | --- | --- | --- |
| A | 922 | 921 | 0 | 874 863 | 2 813 | с этого шага каждый запрос 400, 10 подряд |
| B, та же модель | 1 365 | 1 352 | 1 | 890 770 | 3 179 | компакция на 891 005, работает дальше |
| C, другая модель | 4 799 | 4 761 | 14 | 132 803 | сбрасывается | отказов нет |

Выводы:

- Порог «около 4000 сообщений» данными не подтверждён: упавшая сессия короче, длинная цела.
- Размер как единственная причина тоже не подтверждён: B прошла дальше A и по токенам, и по элементам, а объём JSON у них 6,7 и 6,6 МБ.
- Пустой вызов `shell` с `Invalid arguments {}` стоял в последнем успешном шаге A, но такой же `read` на шаг раньше не помешал. Перед отказом было ещё 5 ответов `server_error: The model failed to generate a response`.
- **Первопричина на стороне провайдера не установлена.** [Н]

Механизм застревания установлен. Текст не совпадает ни с одним шаблоном переполнения, поэтому ошибка классифицируется как `InvalidRequest` без признака `context-overflow`, не повторяется и не запускает компакцию; контекст ниже порога 891 290 (окно минус 15 %), и автокомпакция тоже молчит. История не меняется, поэтому каждый новый ввод получает тот же отказ. [И: packages/ai/src/provider-error.ts, packages/core/src/session/runner/step.ts:158] Воспроизведено: заглушка отвечала тем же телом, пока в истории был конкретный вызов инструмента. Два хода подряд дают `execution.failed`, `POST .../compact` снимает проблему, следующий ввод успешен: `tool_calls` с этим вызовом больше не отправляется. [Ж: stuck_invalid_request.jsonl] Если вызов попадёт в сохраняемый хвост компакции, она может не помочь. [Н]

Следствие для адаптера: повтор `provider.invalid-request` с тем же сообщением подряд считать застрявшей сессией и предлагать компакцию, а не ещё один ввод. Статус `ContextExhausted` здесь неверен, контекст не исчерпан.

## 6. Факты уровня процесса

- Дерево: `opencode serve` -> `bash -c <команда>` -> команда, сервер MCP. **Каждый shell и MCP живут в своей сессии и своей группе процессов**, группа сервера их не накрывает. [Ж: lifecycle.jsonl] [И: packages/core/src/shell.ts:315]
- Память: 140-195 МБ RSS и 16 потоков у простаивающего сервера, 357 МБ и 52 потока с загруженной локацией и одним MCP. [Ж: lifecycle.jsonl]
- Завершение без работы: EOF stdin код 0 за 40 мс, SIGTERM и SIGINT код 130 за 40 мс. [Ж: lifecycle.jsonl]
- С работающим shell: EOF и SIGTERM завершают сервер за 110-160 мс и убивают shell, включая фоновые. Смерть владельца закрывает stdin, и сервер уходит сам. [Ж: crash.jsonl, interrupt.jsonl]
- **SIGKILL сервера оставляет shell живым сиротой** (родитель 1). После рестарта `/api/shell` пуст, сервер о нём не знает. Нужна сверка по списку `shell.created.pid`, снятому до падения, или cgroup. [Ж: crash.jsonl]
- После SIGKILL: `session/active` пуст, `outcome` старый (`succeeded`), инструмент в истории `running`. Режим `--stdio` ничего не продолжает сам ни при какой загрузке. Состояние лечит только следующий ввод: `session.tool.failed {aborted, "Tool execution interrupted: shell"}`. [Ж: crash.jsonl] [И: packages/server/src/process.ts:109, packages/core/src/session/execution/restart.ts:58]
- Закрытие stdout не мешает серверу, после строки готовности он в stdout не пишет. [Ж: lifecycle.jsonl]
- Холодный старт. `/api/agent` пуст, пока локация не поднята: она ленива и запускается первым запросом, привязанным к локации. Первый `POST /prompt` при этом ждёт готовности и завершается примерно за 0,6 с; сессия успевает отработать с агентом `build`. [Ж: lifecycle.jsonl]
- Локация выгружается через 60 минут простоя (`experimental.location_idle_timeout`). [И: packages/schema/src/config/experimental.ts] Выгрузка видна кадром `location.shutdown`, `GET /api/debug/location` пуст. Следующий ввод поднимает локацию, модель и MCP работают. [Ж: live/location_shutdown.jsonl, таймаут 20 с]
- `PUT .../environment` **заменяет** окружение shell целиком, `HOME` пропадает. [Ж: lifecycle.jsonl]
- Прерывание: основной shell снимается за 25 мс с `status:"killed"`, ход `interrupted {reason:"user"}`, частичный текст сохранён с `finish:"error"`. **Фоновый shell выживает** и продолжает работать; `DELETE /api/shell/{id}` убивает его, повтор идемпотентен, но запускает ответ модели (раздел 5.1). [Ж: interrupt.jsonl, lifecycle.jsonl] Следующий ввод работает сразу.
- Несколько сессий на сервере поддерживаются, но ввод одной сессии в двух процессах ломает её: см. риск 1.

Нативная история: SQLite `$XDG_DATA_HOME/opencode/opencode.db`, путь меняет `OPENCODE_DB`, проекции `session_v2`, `session_message`, `session_inbox`. Там же учётные данные: таблица `credential`, импорт из `auth.json` разовой миграцией. Переменные окружения провайдера читаются отдельно. [И: packages/cli/src/database-path.ts:6, packages/core/src/credential/sql.ts:5, packages/core/src/database/migration/20260805200742_import_legacy_credentials.ts, packages/core/src/integration.ts:404]

## 7. Риски и открытые вопросы (по убыванию влияния)

1. **Общая база небезопасна.** Управляемая служба при старте считает все незакрытые claim осиротевшими. Живой тест: служба поднялась на той же базе, пока сервер `--stdio` выполнял инструмент; она пометила инструмент сбоем, вставила `The server restarted while you were working` и вызвала модель, хотя первый процесс ещё работал. Второй `--stdio` на той же сессии делает то же самое при первом вводе, между процессами нет ни событий, ни общего `session/active`. Разные сессии в двух `--stdio`-процессах на одной базе работают без конфликтов. Значит, canterlot обязан иметь свою базу и выполнять каждую сессию одним процессом. [Ж: shared_db.jsonl] [И: packages/core/src/session/execution/restart.ts:58]
2. **Учётные данные лежат в той же базе.** Своя база означает отсутствие входов владельца. Для Go и бесплатных моделей хватает `OPENCODE_API_KEY`. [Ж: models.jsonl] OAuth других провайдеров так не перенести. Нужен выбор владельца.
3. **Настоящая модель проверена на одном маршруте.** Responses у `muse-spark-1.3-contributor` подтверждён вживую (раздел 9); маршрут Anthropic и ненулевой `cache.write` не наблюдались.
4. **Весь API помечен как экспериментальный**, версия схемы `0.0.1`. Нужны закрепление версии и сверка манифеста схемы.
5. **Нет replay.** Любой разрыв SSE теряет эфемерные события, включая `permission.asked` и `form.created`. Сверка проекциями обязательна и должна быть протестирована.
6. **Гонка MCP на старте** не имеет публичного сигнала; ожидание `connected` плюс запас остаётся эвристикой. При промахе инструменты canterlot недоступны на первом шаге.
7. **Застревание по `invalid-request`** воспроизводится при любом стойком отказе провайдера, текст которого не входит в шаблоны.
8. **Сироты после SIGKILL сервера**, как у Codex. Нужна сверка по PID или cgroup.
9. **Фоновые запуски без ввода** не укладываются в модель «ввод -> ход».
10. **Фоновые shell переживают `interrupt`**; смена пресета без перезапуска их тоже не останавливает, в отличие от описания `pilot.md` §4.
11. **Память**: около 350 МБ на сессию при схеме «процесс на сессию».
12. **`reason:"shutdown"` у отказа без сообщения** маскируется под остановку сервера и оставляет claim. В базе canterlot это безвредно, пока на ней нет управляемой службы.

## 8. Воспроизведение и непроверенное

Обвязка хранилась вне репозитория: сервер `serve --stdio` с изолированным окружением, запись потока, заглушка Chat Completions с протоколом сценариев (текст, рассуждения, вызовы инструментов, код ответа, разрыв, зависание) и минимальный stdio-сервер MCP. Эквивалент для тестов описан в спецификации адаптера. Конфигурация провайдера заглушки:

```json
{"providers":{"fake":{"name":"Fake","package":"@ai-sdk/openai-compatible",
 "settings":{"baseURL":"http://127.0.0.1:PORT/v1","apiKey":"x"},
 "models":{"fake-1":{"name":"Fake One","limit":{"context":200000,"output":8192},
  "capabilities":{"tools":true,"reasoning":{"supported":true,"effort":["low","medium","high"]}}}}}},
 "model":"fake/fake-1"}
```

Формат evidence: строки JSON с полями `t_ms`, `dir` (`note`, `http_out`, `http_in`, `sse`, `log`) и `data`; строки `dir:"console"` несут вывод сценария. Пути заменены на `<probe>`, порты, ключи и base64 обрезаны, шумовые события каталога убраны.

Не проверено:

- настоящее тело `GoUsageLimitError`, `Retry-After` у Go, `codexbar --provider opencodego`;
- первопричина `invalid parameters` у провайдера;
- автокомпакция и текст переполнения на настоящем провайдере: окно модели 1 МиБ токенов, цена проверки несоразмерна;
- переполнение очереди SSE на 4096 кадров;
- устойчивость к сбою питания при `synchronous=NORMAL`;
- передача `PATCH permissions` существующим потомкам;
- `session.move`, форки, откат;
- компакция с отравленным элементом в сохраняемом хвосте: настоящую порчу истории получить не удалось, битая картинка отвергается на приёме;
- миграция базы между версиями OpenCode;
- блоки потомка, выпавшие в разрыв потока: проекция читается только у корневой сессии, адаптер их не восстанавливает, расход таких шагов не считается.

Закрыто на этапе 2 живыми тестами L2 на заглушке: передача правил потомкам (`PATCH` на каждого, родительский не наследуется), туду у субагента, кадр `session.compaction.failed`, второе переполнение, `provider.quota` с `Retry-After`, `session.retry.scheduled` на настоящем сервере. Что осталось для настоящей модели, перечислено в чек-листе смоука.

Вход в базу canterlot только с `--standalone`: `opencode auth login --standalone`. Без флага команда работает через фоновую службу, а служба на этой базе нарушает ограничение из риска 1. [Ж: `opencode auth login --help` на 2.1.4, запуск с `--standalone` в изолированных каталогах создаёт `opencode.db` и не оставляет процессов]

## 9. Живой смоук (31.10.2026)

Настоящие хаб и воркер коммита этой ветки, одноразовая база, запуск и ввод через `ExecutionManager` и HTTP хаба, PWA через локальную кромку. Модель `opencode-go/muse-spark-1.3-contributor`, усилие `low`. Владелец дал разрешение на этот прогон. Модель учится на вводе, поэтому домашний каталог и конфигурация владельца подменялись одноразовыми, рабочий каталог лежал в `/tmp`, промпты содержали только тестовый текст. Записи: `evidence/opencode/live/`. Потрачено 61 тыс. токенов ввода, 389 тыс. чтения кэша, 6,6 тыс. вывода с рассуждениями, 0,008 USD по каталогу; подписка лимита не показала.

Отличия настоящих кадров от выдуманных и от заглушки:

| Что | Заглушка или выдумка | Настоящее |
| --- | --- | --- |
| Маршрут | Chat Completions | Responses: `native.itemId`, `native.responseId`, `reasoningEncryptedContent` |
| Рассуждения | текст в `reasoning.ended` | обычно `text: ""` и зашифрованный блоб; изредка краткая сводка дельтами. Пустой блок PWA не показывает |
| Дельты | мелкие | крупные куски, короткий текст приходит одной дельтой |
| `cache.write` | ожидался ненулевым | всегда 0, кэш виден только в `cache.read` |
| Запись Go в `/api/model` | состав выдуман | 28-30 записей; у части нет вариантов, усилие `default`; наборы вариантов разные (`none`, `thinking`, `max`) |
| Период отказа сжатия | `session.execution.failed` | `session.execution.succeeded`: без блока от сжатия причина терялась |
| Прерывание сжатия | терминал без кадра отказа | `session.compaction.failed {aborted}` раньше терминала |
| Проекция сжатия | `{id, type, status}` из исходников | подтверждено, плюс `error`, `summary`, `metadata.keepID` |
| Форма MCP | `kind: "elicitation"` | `kind: "mcp-elicitation"`, `sessionID: "global"`, `metadata.server`, `metadata.message` |
| Служебные кадры | нет | `location.shutdown`, `models-dev.refreshed`, `shell.deleted`, `session.compaction.delta` |

Найдено и исправлено:

1. **`GET /api/model` отвечает 503 после 5 с ожидания плагинов.** `{"_tag":"ServiceUnavailableError","message":"Location plugins did not finish activating","service":"plugin"}`. Так же ведёт себя `/api/model/default`, `/api/agent` не ждёт. [И: packages/server/src/location.ts] `discover` считал это отказом каталога: после перезапуска воркера OpenCode исчезал из формы, сессии становились «Неизвестный харнесс». На старте исполнения молча терялись варианты, окно контекста и признак картинок. У владельца плагин `superpowers` активируется дольше 5 с. Адаптер ждёт 503 в пределах срока. Проверено на стенде с плагином на 7 с: воркер доступен за 9,9 с, обычный старт 2,6 с, `DISCOVER_WAIT` 20 с менять незачем. [Ж: live/plugins_503.jsonl]
2. **Ложный `block_checksum_mismatch` у каждого блока без дельт.** Супервизор открывает блок ревизией 1, проверка хаба `block.rev > 0` срабатывала всегда: 34 диагностики на 78 финалов одной сессии. Касалось всех адаптеров, у OpenCode это каждый инструмент.
3. **Отказ ручного сжатия терял текст провайдера.** Теперь блок ошибки идёт от самого сжатия; отмена ошибкой не считается.
4. **Чтение истории на возобновлении оставляло фазу `generating`** у сессии, ждущей ввода.

Подтверждено вживую без правок: приём ввода и доказательство `opencode_inbox_admitted`, дельты до PWA, `RunFinished.usage` против `session.step.ended` (`input` без кэша, `output` с рассуждениями), окно контекста из каталога, отсутствие запроса за заголовком (сумма `session.usage.updated` равна сумме шагов), код выхода в строке инструмента, запись при `snapshots: false`, карточка разрешения, `once` без сохранённого правила, отказ с сообщением, `read_only` и смена режима, вопрос, пометка `cli_policy`, MCP `canterlot_limits` и `presets_list`, пресеты (пользовательский без перезапуска, «Как у владельца» с навыками, «Без инструкций» с плагинами и без навыков), дорожка субагента, панель делегирования, отдельная строка расхода, туду с `cancelled`, прерывание и фоновый shell с предупреждением, слитый период с одним терминалом, разрыв потока, перезапуск воркера посреди хода, `kill -9` сервера с возобновлением и снятием сироты, выгрузка локации, картинка на входе и в строке `read`.

Наблюдения для решения владельца:

- **`read_only` и обратно путает модель.** `read_only` снимает `execute`, сервер убирает `core/codemode`; при возврате раздел приходит пользовательским `<system-update>` «Use the `execute` tool ... They cannot be called directly». Инструменты на месте, L2 это подтверждает, но `muse-spark-1.3-contributor` решила, что `shell` и `question` недоступны, и вызывала только `execute`, также на следующем вводе с прямой просьбой. В свежей сессии `owner` всё работает. Запрет `execute` снял бы эффект, но отрезал бы MCP владельца с режимом кода по умолчанию. [Ж: live/code_mode_after_read_only.jsonl]
- **Первая модель каталога `iceman-contributor-free`**, она же выбор по умолчанию: контрибьюторские модели учатся на вводе.
- **Блок потомка, открытый к концу родителя, закрывается как неполный.** Фоновый потомок с идущим инструментом даёт это всегда: `tool_call` неполный, результат приходит позже и ложится в ту же дорожку. Запись `live/background_subagent.jsonl`.
- **Битая картинка отвергается на приёме** (400 из `image.normalize`): ввод `rejected`, в ленте без причины.
- **После перезапуска исполнения команды ещё нет:** период сжатия не приписать, расход сжатия не попадает в историю, прерывание сжатия через хаб даёт `not_applicable`. В пределах одного исполнения приписывание работает.
- `canterlot_limits` отвечает «no limit data yet»: источника лимитов нет (решение 6).
