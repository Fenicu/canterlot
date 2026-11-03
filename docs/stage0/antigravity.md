# Этап 0: адаптер Antigravity (`agy acp`)

Исследовательский отчёт по интеграции `agy 1.4.12` через скрытый ACP-сервер (`agy acp`, JSON-RPC 2.0 в stdio). Кода адаптера нет, только живые пробы. Доказательства: `docs/stage0/evidence/antigravity/*.jsonl` (каждый кадр с меткой времени и направлением `in`/`out`/`err`), скрипты проб там же.

Обозначения:

- **[Ж: файл]** проверено живым прогоном, файл из `docs/stage0/evidence/antigravity/`.
- **[Д: путь]** прочитано в исходниках/документации (HAPI: `/home/fenicu/myprojects/hapi/cli/src/agy/`, строки бинарника `~/.local/bin/agy`).
- **[Н]** не проверено.

Итог. Вход **A (`agy acp`) подтверждается как рабочий**: старт, resume, поток, инструменты, разрешения allow/deny/cancel, прерывание с `stopReason:"cancelled"` без убийства процесса, `usage` в ответе prompt, `/compact` обычным ходом, фоновые задачи и субагенты частично видны. Вопросы (`ask_question`) зависают без сигнала клиенту — главный риск для первой версии. Квота на момент работ: Gemini weekly ~1% → ~0.8% (колеблется между опросами), сброс 2026-11-04T15:55Z; Claude/GPT weekly 0.26%.

## 1. Версия и стенд

| Параметр | Значение |
| --- | --- |
| CLI | `agy 1.4.12`, Go ELF 234 МБ, `~/.local/bin/agy` [Ж: help.txt, acp-help.txt] |
| Интерфейс | `agy acp [--gemini-dir ...] [--log-file ...] [--disable-telemetry] [--dangerously-skip-permissions] [--sandbox] [--process-per-session]`, stdio JSON-RPC |
| ОС | Debian 13, x86-64, та же VM |
| Дата прогонов | 03.11.2026 |
| Модель проб | `gemini-3.8-flash-low` (через `session/set_model`), короткие промпты, временный git-репозиторий `/tmp/agy-stage0-*` как cwd |
| Учётная запись | собственная владельца, Google AI Pro (`_meta.antigravity.tier.name` в каждом `session/new`) [Ж: p1-diagnostics.jsonl] |
| Расход квоты | ~15 ходов на flash-low; все бесквотные пробы — без ходов |

Данные владельца. Файлы учётных данных не открывались. В `~/.gemini` чтением ходил только сам `agy`. Пробы создали ~12 сессий в базе разговоров владельца (видны в `session/list`); других изменений настроек владельца нет. Заголовки чужих сессий из `session/list` перед коммитом вырезаны (в evidence только число и ключи записей) [Ж: p1-session-list-summary.json].

## 2. Используемая поверхность интерфейса

| Вход | Решение | Причина |
| --- | --- | --- |
| `agy acp` (ACP в stdio) | **Выбрано** | Всё нужное есть и доказано живыми кадрами: §3. B и C не понадобились |
| `agy -p --conversation --output-format stream-json` | Запасной путь, не проверялся (P23 открыт) | HAPI уже водит его иначе (`GEMINI_FORCE_FILE_STORAGE=true`, пер-ход процесс) [Д: agyHeadlessDriver.ts, agyNdjsonParser.ts] |
| Python SDK | Отвергнут | Только ключ API/Vertex, нет подписочного входа (совпадает с черновиком F17) |
| Remote Control | Не пробовался | Продукт Google, вне адаптера |

Готовность: ответ `initialize` за ~0.2 с; `session/new` 0.1–2.4 с (холодный и тёплый одинаковы). 27 с из черновика (F3) не воспроизвелись — вероятно, артефакт периода исчерпанной квоты. `ready_deadline` 60 с избыточен, достаточно ~15 с [Ж: p1-diagnostics.jsonl, t2_text.jsonl].

Закрытие: EOF stdin → выход код 0 за 0.06–0.42 с; процесс жив до EOF [Ж: все *.jsonl].

## 3. Матрица проб

| # | Сценарий | Вердикт | Evidence |
| --- | --- | --- | --- |
| P1 | Диагностика без хода | **PASS** | `initialize` 0.2 с, 4 `authMethods`, `session/list` (20 записей, ключи `sessionId/cwd/title/updatedAt`), `/usage` бесплатен, `session/new` 0.1–2.4 с, модели только 3× `gemini-3.8-flash-*`, `configOptions` model + thought_level, tier Pro [Ж: p1-diagnostics.jsonl, usage-before.json, models.txt] |
| P2 | Текстовый ход | **PASS** | Эхо `user_message_chunk` через +0.11 с, `session_info_update` с заголовком, `agent_message_chunk`, ответ `{stopReason:"end_turn", usage:{inputTokens,outputTokens,cachedRead/WriteTokens,thoughtTokens,totalTokens}}`. `transmit_deadline` 10 с хватает с запасом [Ж: t2_text.jsonl] |
| P3 | Второй ввод во время хода | **PASS с оговоркой** | Второй `session/prompt` во время `run_command` принят (эхо +3.6 с), инструмент ушёл в фон по `WaitMsBeforeAsync`, оба хода завершились `end_turn`, usage только в последнем ответе. Не очередь и не ошибка, а слияние с уходом в фон [Ж: t7-cancel-p3-steer.jsonl] |
| P4 | Инструмент | **PASS** | `tool_call` (`toolCallId`, `name:"run_command"`, `kind:"execute"`, `rawInput` = args), `tool_call_update completed` с `rawOutput:{output, exit_code}` и `content[]`. Ненулевой код не проверялся (модель прогнала `exit 3` только в зависшем ходе t4) [Ж: t5-allow.jsonl] |
| P5 | Разрешение | **PASS** | Запрос `session/request_permission {toolCall(status pending), options:[{optionId:"allow",kind:"allow_once"},{optionId:"deny",kind:"reject_once"}]}`, id запроса строковый `"1"`. Ответ `{id:"1", result:{outcome:{outcome:"selected", optionId}}}`. Сервер ответ не подтверждает. `deny` → ход молча `end_turn` без текста. Открытый запрос жил ≥180 с без срока (t4 висел, пока мы не убили процесс). Cancel при открытом запросе → `tool_call_update in_progress` + `stopReason:"cancelled"` + usage [Ж: t4_tool.jsonl, t5-deny.jsonl, t5-allow.jsonl, t7c-cancel-perm.jsonl] |
| P6 | Вопрос | **PARTIAL** | `ask_question` приходит как `tool_call kind:"other"` + текст с вариантами, **без** `request_permission` и без `elicitation/create`; ход зависает без `stopReason` (≥145 с). Ответ текстом в новом ходе («Blue») модель поняла, но старый инструмент не завершился — replay его не содержит, модель просто продолжила диалог. `elicitation/create` в бинарнике есть (строка `elicitation/create for session %s step %d ended without an answer`), но живьём не наблюдалась [Ж: t6-question.jsonl, t6b-answer.jsonl] |
| P7 | Прерывание | **PASS** | `session/cancel` во время висящего разрешения: `stopReason:"cancelled"`, процесс жив, следующий ход работает. Cancel после конца хода — тихий no-op. **Не проверен** cancel посреди чистой текстовой генерации (в t7 cancel не был отправлен: оба хода завершились раньше) [Ж: t7c-cancel-perm.jsonl; негатив: t7b-cancel.jsonl] |
| P8 | Фон | **PARTIAL** | `run_command` уходит в фон по `WaitMsBeforeAsync` (модель ставила 5000/10000), модель сказала «background command still running». После SIGKILL модель сама вызвала `manage_task {Action:"status", TaskId:"<sid>/task-2"}` (kind other, completed) и сказала «server restart halted all background tasks». Явного списка фоновых задач не запрашивалось [Ж: t7-cancel-p3-steer.jsonl, t13b-resume.jsonl] |
| P9 | Субагент | **PARTIAL** | `invoke_subagent {Model:"inherit", Role, Prompt}` (kind other, **без** разрешения) → completed за ~0.1 с, но ход закончился «waiting for its report»: результат приходит позже и сам не разбудил сессию. В completed-кадре **нет** `rawOutput` — вывод субагента виден только через текст модели. Повторный ход модель пере-делегировала заново [Ж: t9-subagent.jsonl, t9b-subagent-result.jsonl] |
| P10 | План | **OPEN** | Просьба «только спланируй» дала обычный текст, кадров `plan`/`TodoUpdated` нет. Есть ли выделенный инструмент плана — неизвестно [Ж: t10-plan-t11-compact.jsonl] |
| P11 | Сжатие | **PASS** | `/compact` как prompt: эхо + текст-саммари + `end_turn` + usage. Обычный ход, `holds_input_during_compaction`=true тривиально [Ж: t10-plan-t11-compact.jsonl] |
| P12 | Resume | **PASS** | Новый процесс + `session/load`/`session/resume` + prompt работает (несколько раз). Replay: `user/agent_message_chunk` + `agent_thought_chunk`, **без** состояний инструментов. `session/resume` принимает и голый `{sessionId}`. `session/load` неизвестного UUID успешен, но prompt по нему → `-32603 trajectory not found` — существование проверять через `session/list` или ловить эту ошибку [Ж: t5-deny.jsonl, t6b-answer.jsonl, t13b-resume.jsonl, resume-заметки в §7] |
| P13 | SIGKILL | **PASS** | SIGKILL группы посреди хода: `sleep 60` **выжил** (дети убегают из группы, как у Claude/Codex). Новый процесс + load + prompt: модель заметила рестарт через `manage_task status`, ход `end_turn`. Убитый инструмент терминального кадра не оставил [Ж: t13-kill.jsonl, t13b-resume.jsonl] |
| P14 | Модель и effort | **PASS** | `session/set_model {modelId:"gemini-3.8-flash-low"}` → `{}` + `config_option_update`; действует со следующего хода. `session/set_config_option` требует пару `{configId, value}` (`id`/`optionId` → `-32602 Unknown config option`). Неизвестная модель → `-32602 Unknown model ID: ...` [Ж: p14-model-effort.jsonl] |
| P15 | Инструкции | **PARTIAL** | `_meta.antigravity.systemPrompt.append` в `session/new` принят (ответ штатный). Доходит ли до модели — не проверено (нужен ход с маркером) [Ж: p15-instructions.jsonl] |
| P16 | MCP | **PARTIAL** | `session/new` с `mcpServers:[{command:"true",args:[],env:[]}]` принят. Гонка холодного старта, `alwaysLoad`, имена через `call_mcp_tool` — не проверены [Ж: p16-mcp-setup.jsonl] |
| P17 | Лимиты | **OPEN** | Реальная 429 не наблюдалась (weekly не достигал 0). Текстовая ошибка квоты из черновика (F4) стоит. `usage` в ответе prompt — основной источник расхода; `/usage` — запасной. Замер: показания `/usage` **плавают между опросами** (±0.003 weekly) — серверная задержка учёта [Ж: usage-before.json, usage-after.json] |
| P18 | Отказы | **OPEN** | Только строки: `GOOGLE_GEMINI_BASE_URL` (кастомный endpoint для L2-заглушки), `CLOUD_CODE_URL`, ретраи внутри. Живьём не рвалось |
| P19 | Режимы | **PARTIAL** | Флаги `--dangerously-skip-permissions`, `--disable-telemetry`, `--sandbox` принимаются (initialize OK). Правила `deny` для read_only и `artifactReviewPolicy` — не проверены (настройки `~/.gemini` трогать было нельзя) [Ж: p19-*.jsonl] |
| P20 | Конкуренция и вход | **PASS, решение ниже** | (a) Изолированный `--gemini-dir`: initialize OK, `session/new` → `-32000 Authentication required`; создаются `config/`, `antigravity-cli/` (имена в кадрах). (b) Два процесса на дефолтном dir: параллельные `session/list` оба OK (20 записей), блокировок нет. (c) `GEMINI_FORCE_FILE_STORAGE=true` на изолированном dir ничего не меняет — по-прежнему `-32000` [Ж: p20a-isolated.jsonl, p20b-concurrency.txt, p20c-force-file-storage.jsonl, p20d-concurrency.txt] |
| P21 | Обновление | **PARTIAL** | `agy update --help` пуст. Только строки: автоапдейтер `https://antigravity-cli-auto-updater-*.run.app`, пропуск при проверке <15 мин, `current_version.txt`. Дочернего апдейтера у `agy acp` не замечено (короткое наблюдение) [Ж: p21-update-help.txt] |
| P22 | Картинка | **PASS (негатив)** | Блок `{type:"image", data, mimeType}` принят без ошибки, но **молча выброшен**: эха нет, входных токенов +10. Картинки — только путём в тексте [Ж: t22_image.jsonl] |
| P23 | Запасной путь B | **OPEN** | Только чтение HAPI: пер-ход процесс, `GEMINI_FORCE_FILE_STORAGE=true`, убийство через сигналы, PreToolUse-хук с ожиданием ответа. Живьём не гонялось [Д: agyHeadlessDriver.ts, agyNdjsonParser.ts] |

Дополнительно (вне P1–P23):

- `initialize` отдаёт `_meta.antigravity {tier, systemPrompt:{append:true}}`; `session/new` — tier `{name:"Google AI Pro", id:"g1-pro-tier"}` [Ж: t2_text.jsonl].
- Методов `session/set_mode`, `session/capabilities`, `session/methods` нет (`-32601`) [Ж: p22-caps.jsonl].
- `session/request_permission` после нашего ответа никак не подтверждается; поздний ответ на мёртвый запрос не проверялся.
- `agent_thought_chunk` стримится и на flash-low (после рестарта), и реплеится при load [Ж: t13b-resume.jsonl].
- Первый кэш-хит: `cachedReadTokens:4041` в повторном ходе [Ж: t6b-answer.jsonl].
- Управляющие строки из бинарника: `AGY_ACCOUNT=<имя>` выбирает аккаунт среди вошедших; `GEMINI_API_KEY` + `modelProvider:"gemini"` в settings.json + `GOOGLE_GEMINI_BASE_URL`; `AGY_ADC_AUTH` для ADC; `SendAllQueuedMessages`, `queuedMessages:"send-immediately"` в settings.json (очередь настраивается); `elicitation/create ... ended without an answer` (есть таймаут ответа на elicitation) [Д: строки бинарника, changelog 1.4.12 в черновике].

## 4. Семантика доставки и события

| ACP | AdapterEvent | Основание |
| --- | --- | --- |
| `user_message_chunk` с текстом нашего prompt, пока ход в полёте | `Proof{kind:"agy_acp_user_echo"}` (приём) | +0.06–0.11 с после отправки [Ж: t2_text.jsonl] |
| JSON-RPC `error` на prompt до эха (`-32603 trajectory not found`) | `Rejected` | Тот же текст на load неизвестного id успешен — сверка через `session/list` [§3 P12] |
| `agent_message_chunk` / `agent_thought_chunk` | `BlockStarted Text/Reasoning`, дельты, `BlockFinal` | Чанки мелкие (до ~200 символов), склейка на адаптере [Ж: t10-*] |
| `tool_call` in_progress → удержание; `tool_call_update completed/failed` | `BlockStarted ToolCall`, `Phase RunningTool`, `ToolResult BlockFinal` | `rawOutput:{output, exit_code}`; `rawInput`=args [Ж: t5-allow.jsonl] |
| `tool_call_update` pending + `session/request_permission` | `RequestOpened kind:"permission"`, `Phase AwaitingPermission` | shape в §3 P5; срок не наблюдался (≥180 с жил) |
| Ответ prompt `stopReason` `end_turn` (+`usage`) / `cancelled` (+`usage`) / `max_tokens` / `refusal` (не встретились) | `RunFinished`; usage из ответа | usage есть всегда, даже у `cancelled` [Ж: t7c-cancel-perm.jsonl] |
| второй `session/prompt` в полёте принят | НЕ `Undetermined`: новый ввод вливается, инструмент уходит в фон | P3: адаптер держит один prompt в полёте; «steer» = второй prompt с семантикой «уйди в фон», не отмена |
| `ask_question` pending + тишина | **Вопрос без поверхности**: таймаут хода → показать текст модели как вопрос, ответ владельца — следующим prompt | P6: ни request_permission, ни elicitation не приходят |
| `invoke_subagent` completed без payload | `BlockFinal ToolResult` без содержимого; результат — только текстом модели позже | P9: детей и их разговоров в ACP не видно |
| `manage_task` (kind other) | Служебный; `Action:status` после рестарта — признак «фоновые задачи умерли» | P8/P13: маркер для фазы «проверь фон после load» |
| `available_commands_update`, `session_info_update`, `config_option_update` | Внутреннее состояние | title — только для своих сессий |
| выход процесса / SIGKILL | `ProcessExited`; дети инструментов **выживают** → чистить по cgroup исполнения, как у остальных | P13: `sleep 60` пережил SIGKILL группы |

## 5. Режимы, MCP, инструкции, лимиты

- **Режимы.** Проверены только флаги запуска (§3 P19). `bypass` = `--dangerously-skip-permissions` (плюс, по черновику, `artifactReviewPolicy` от ревью планов — не проверено). `ask` — дефолт владельца сейчас и так спрашивает `run_command`. `read_only` через `permissions.deny` в settings.json — не проверено: писать в `~/.gemini` было запрещено, изолированный dir без входа не заводится. Смена режима = перезапуск процесса между ходами (методов смены на лету нет, `set_mode` отсутствует).
- **MCP.** Передача через `session/new.mcpServers` принята с dummy-сервером. Формат записи, `alwaysLoad`, гонка холодного старта — открыты (нужен настоящий MCP-стенд после сброса).
- **Инструкции.** `_meta.antigravity.systemPrompt.append` принят; эффект на модель — открыт (маркерный ход после сброса). `AGENTS.md`/`GEMINI.md` репозитория читаются всегда (по аналогии с остальными; не проверялось).
- **Лимиты.** Два источника: `usage` из каждого ответа prompt (основной, точный) и `agy -p /usage --output-format json` без хода (запасной; два провайдера Gemini / Claude+GPT × окна weekly/5h). Разрешение спора двух групп окон — как в черновике, вопрос 6. Показания `/usage` плавают между опросами — не использовать для строгих сверок.
- **Модели.** ACP отдаёт только 3× `gemini-3.8-flash-*` (текущая `...-high`); `set_model` переключает на `-low` с немедленным `config_option_update`. `agy models` видит 7 (плюс Claude/GPT) — в ACP их нет. Effort = `thought_level` low/medium/high через `set_config_option {configId, value}`.

## 6. Что это значит для адаптера (поправки к разделу 6 черновика)

| Метод/поле | Вердикт пробы | Поправка к черновику |
| --- | --- | --- |
| `capabilities.streaming` | Подтверждено | Чанки мелкие; thought-чанки есть и в replay |
| `capabilities.resume` | Подтверждено | `session/resume` работает и с голым `{sessionId}`; replay — только текст+мысли, без инструментов; load чужого id «успешен», правда проверяется prompt (`-32603`) или `session/list` |
| `capabilities.interrupt` / `interrupt_keeps_process` | Подтверждено | `session/cancel` → `cancelled` + usage, процесс жив. Cancel опоздавший — no-op |
| `capabilities.approvals` | Подтверждено с точной формой | `optionId` = `allow`/`deny` (не `allow_once`), `kind` = `allow_once`/`reject_once`; ответ `{outcome:{outcome:"selected",optionId}}`; **никогда `allow_always`** — правило черновика в силе |
| `capabilities.questions` | **Не подтверждено, есть обход** | `ask_question` не даёт сигнала; детектор = `tool_call ask_question` + стоп без `stopReason` дольше N с → вопрос из текста модели, ответ — следующим prompt. `Unavailable("stage0")` остаётся, но с описанным обходом |
| `capabilities.background_work` | Частично | Виден уход в фон (молча), `manage_task status` после рестарта; списка задач и `WaitMsBeforeAsync`-управления нет. `Unavailable("stage0")` для списка, фон переживает процесс |
| `transmit` / `validate_transmission` | Подтверждено + поправка | Эхо +0.1 с; **image-блоки молча выбрасываются — резать на адаптере**, только путь в тексте |
| настройки в `transmit` | Подтверждено | `set_model {modelId}`, `set_config_option {configId, value}`; `set_mode` не существует |
| `deliver_decision` / `refuse_request` | Подтверждено | Формы выше; deny завершает ход без текста |
| `steer` | **Пересмотреть** | Второй prompt не ошибка и не очередь: вливание + уход инструмента в фон. `NotApplicable` по умолчанию остаётся, но механика для явного steer есть (дизайн на этапе 1) |
| `compact` | Подтверждено | `/compact` = обычный ход с саммари |
| `next_event` (§8 черновика) | Подтверждена + дополнения | Добавить: ` cancelled` с usage; `manage_task` служебный; replay содержит `agent_thought_chunk`; `invoke_subagent` без payload |
| `close` | Подтверждено | EOF → 0 за <0.5 с; дети — через cgroup |
| `usage` | **Поправка: есть** | `RunFinished.usage` заполняется из ответа prompt (input/output/cache/thought/total). Опасение черновика снято |
| `isolates_mcp()` | Условно | Только если вход — отдельный gemini-dir (см. §7); на `~/.gemini` владельца плагины/хуки владельца видны (чужие скиллы уже светились в `available_commands_update` первой пробы черновика) |

## 7. Auth / P20: решение

- (a) Изолированный gemini-dir своим входом не обладает: `session/new` → `-32000 Authentication required`. Неинтерактивных методов вселения входа без чтения секретов нет (oauth — браузер, ключ — значение ключа). Вход через `authenticate` в изолированном dir не выполнялся (интерактив) [Ж: p20a-isolated.jsonl].
- (b) Дефолтный `~/.gemini` работает, два процесса параллельно — без блокировок (concurrent `session/list` OK). Цена: сессии canterlot пишутся в базу владельца (общая с App/IDE/CLI), видны владельцу; MCP/плагины/хуки владельца наследуются; настройки владельца действуют [Ж: p20d-concurrency.txt].
- (c) `GEMINI_FORCE_FILE_STORAGE=true` на изолированном dir вход не чинит. Но для headless-устойчивости (нет keyring) его стоит ставить всегда, как делает HAPI [Д: agyHeadlessDriver.ts; Ж: p20c-force-file-storage.jsonl].

Вывод для canterlot: **сессии на `~/.gemini` владельца** (вариант «как у владельца» = честный дефолт; изоляция MCP — только через `session/new.mcpServers` + пустые правила, глобальный `mcp_config.json` владельца при этом всё равно виден — проверить после сброса, насколько `mcpServers` перекрывает глобальный). Отдельный gemini-dir возможен только если владелец сам выполнит вход внутри него (интерактивный `authenticate`/браузер) — решение за владельцем, проба не делалась. `GEMINI_API_KEY` из секрета воркера — альтернатива без подписки (только Gemini, окна лимитов пропадают, остаются 429): строки бинарника описывают точную схему (`modelProvider:"gemini"` + `GOOGLE_GEMINI_BASE_URL` для L2-заглушки).

## 8. Осталось после сброса квоты (точно)

Квота на конец работ: Gemini weekly ~0.008 (сброс 2026-11-04T15:55Z), 5h ~0.99, 3p-weekly 0.0026. Ходов не делать до сброса. По порядку приоритетности:

1. P17: исчерпать 5h-окно или дождаться 429 — снять структуру quota-ошибки на настоящем ходе (текст vs структура).
2. P6-доводка: таймаут висящего `ask_question` (сколько висит: 3/10 мин?), приходит ли `elicitation/create ... ended without an answer` и с каким кадром; поздний ответ на мёртвый permission-запрос.
3. P8: чем отвечает `manage_task {Action:"list"}` (параметры TaskId?); спонтанно ли приходит результат субагента/фоновой задачи без нового prompt (подержать сессию t9 открытой 5–10 мин).
4. P10: есть ли выделенный инструмент плана (спросить модель «какие инструменты доступны» нельзя — смотреть `tool_call` при просьбе спланировать через `/plan` как команду).
5. P9-доводка: `TriggerSubagentApprovalFast`/`RespondToSubagentToolConfirmation` из строк — когда всплывают.
6. P15-эффект: ход с `_meta append` маркером, проверить влияние на ответ.
7. P16: настоящий MCP-сервер canterlot-заглушка: имя инструмента, `alwaysLoad`, гонка (20 повторов), MCP-ответ с картинкой.
8. P19-режимы: `read_only` через отдельный gemini-dir невозможен без входа — значит проверять `permissions.deny` можно только на `~/.gemini` (нужно разрешение владельца на правку настроек) либо отложить; `bypass` + ревью `/plan`.
9. P18: сеть недоступна / неверный ключ (`GEMINI_API_KEY=invalid` в изолированном dir — бесплатно, без квоты!): повторы и их видимость; `GOOGLE_GEMINI_BASE_URL` на заглушку (L2-прототип).
10. P21: трогает ли автообновлятор закреплённую копию (нужен отдельный стенд с копией бинарника вне PATH).
11. P23: stream-json SIGINT посреди хода (жив ли процесс), `denied_actions`, PreToolUse-хук с `timeout` 600.
12. P7-щель: cancel посреди чистой текстовой генерации (длинный ответ).
13. P5-щель: срок жизни открытого запроса (>3 мин? `ended without an answer` для permission?).
14. P3/P4-щели: ненулевой `exit_code` в `rawOutput` (модель в t4 не добежала; повторить `exit 3` с allow заранее через `--dangerously-skip-permissions`? — меняет surfacing разрешений, только отдельным процессом).

## 9. Риски (дополнение к черновику)

- Пробы велись на потребительском входе владельца по его решению (записано в черновике). FAQ-риск (F18) остаётся; новых данных нет.
- Дети инструментов убегают из группы и переживают смерть ACP-процесса (два случая: `sleep 45` после EOF и после SIGKILL pgid) — адаптеру нужен cgroup исполнения, как у Claude/Codex. Свои probe-дети убиты, чужих процессов не трогалось (проверено `ps` до/после).
- `/usage` плавает между опросами — решения о лимитах только по `usage` из ответов prompt + окнам `/usage` с гистерезисом.
- `session/list` светит заголовки и cwd сессий владельца любому процессу пользователя — в evidence не коммитить (редактировано).
- Отмена владельцем: `logout` capability задекларирована в initialize — не вызывать.
