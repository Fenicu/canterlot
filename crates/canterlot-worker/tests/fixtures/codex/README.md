# Сконструированные фикстуры Codex (задача 1)

Записанные свидетельства лежат в `docs/stage0/evidence/codex/` и перечисления не
требуют. Файлы ниже **сконструированы по записям, подтвердить в live smoke**.
Каждый повторяет форму записанного кадра; источники указаны под файлом.

- `handshake_owner.jsonl` — сконструировано, подтвердить в live smoke. Две строки,
  сырые JSON без обёртки `{"t_ms","dir","data"}` (помощник повтора появится
  в задаче 2). Источники формы: `config/read` — схема 0.164
  `ConfigReadResponse` (`{config, origins, layers}`), значения внутри `config`
  по записи `ev/start_stream.jsonl:12` (поля `approval_policy`, `sandbox_mode`,
  `sandbox_workspace_write` плюс поле `developer_instructions` из схемы
  `Config`); `account/read` —
  `ev/diagnostics.jsonl:23` (`account` с `type`/`planType`, `requiresOpenaiAuth`).
  Строка 1 — ответ `config/read` владельца (`never` + `danger-full-access`,
  собственный текст инструкций). Строка 2 — ответ `account/read` без входа
  (`account: null`,   `requiresOpenaiAuth: true`). Значения `<redacted>` оставлены
  как в записях.
- `subagent.jsonl` — сконструировано, подтвердить в live smoke. Источники формы:
  `collabAgentToolCall` и `thread/started` с `parentThreadId` из схемы `ThreadItem`
  и `thread/started`; `subAgentActivity` из схемы уведомлений; `tokenUsage`
  повторяет форму `ev/tools.jsonl:27`. Покрывает: главный `spawnAgent` с
  `receiverThreadIds`, регистрацию ребёнка по `parentThreadId`, детские ходы
  (не завершают главный), расход ребёнка под его агентом и моделью.
- `reasoning.jsonl` — сконструировано, подтвердить в live smoke. Источники формы:
  элемент `reasoning` и методы `item/reasoning/summaryPartAdded`,
  `summaryTextDelta` из схемы (`generate-ts`, типы `Reasoning*`). Покрывает:
  две части сводки через пустую строку и пустой элемент без событий.
- `image_view.jsonl` — сконструировано, подтвердить в live smoke. Источник формы:
  элемент `imageView` с путём из схемы `ThreadItem`. Покрывает: блок
  `view_image` и одну `Media` по пути.
- `mcp_tool.jsonl` — сконструировано, подтвердить в live smoke. Источники формы:
  элемент `mcpToolCall` и `item/mcpToolCall/progress` из схемы. Покрывает:
  блок `<server>.<tool>`, фазу из прогресса, результат текстовыми частями.
- `output_delta.jsonl` — сконструировано, подтвердить в live smoke. Источник
  формы: `item/commandExecution/outputDelta` из схемы (в записях отсутствует
  даже для `sleep 6`, `ev/multi_input.jsonl:76, 89`). Покрывает: блок результата
  из дельт и финал из `aggregatedOutput`.
- `compaction.jsonl` — сконструировано, подтвердить в live smoke. Источник формы:
  элемент `contextCompaction` из схемы `ThreadItem`. Покрывает фазу
  `ContextMaintenance`.
- `context_exceeded.jsonl` — сконструировано, подтвердить в live smoke. Источник
  формы: `turn/completed failed` с `codexErrorInfo` (форма `ev/service_failure.jsonl:70`).
  Покрывает `ContextExhausted`.

- `rate_limits_read.json` — сконструировано, подтвердить в live smoke. Ответ
  `account/rateLimits/read` (`schema:GetAccountRateLimitsResponse`) с окнами
  `primary`/`secondary` и дубликатом в `rateLimitsByLimitId.codex`.

- `rate_limits_read_live.json` — настоящий ответ `account/rateLimits/read` (пилот,
  31.10, codex 0.164). Удалены `accountId` и `planType`, остальное без правок.

- `usage_limit_turn.jsonl`, `usage_limit_need_read.jsonl`, `usage_limit_read.json`,
  `rate_limit_turn.jsonl` — сконструировано, подтвердить в live smoke. Сценарии
  «кранча» для `usageLimitExceeded` и `rateLimitExceeded` (Task 6).

- `mcp_status.jsonl` — сконструировано, подтвердить в live smoke. Две строки
  уведомлений `mcpServer/startupStatus/updated` для сервера `canterlot`: статусы
  `ready` и `failed` (форма `McpServerStatusUpdatedNotification`).
- `model_list.json` — сконструировано по схеме 0.164 `ModelListResponse`
  (`data` + `nextCursor`; у модели `id`, `model`, `displayName`, `description`,
  `hidden`, `isDefault`, `defaultReasoningEffort`,
  `supportedReasoningEfforts[].reasoningEffort`, `inputModalities`), подтвердить
  в live smoke. Сырой ответ в записях пропущен (`ev/diagnostics.jsonl:27`);
  значения моделей и усилий взяты из сводки `ev/diagnostics.jsonl:28`. Покрывает
  проверку настроек задачи 3 (`gpt-6-luna` без `ultra`).
- `settings_updated.jsonl` — сконструировано, подтвердить в live smoke. Источник
  формы: `thread/settings/updated` после смены (`ev/question.jsonl:92`; песочница —
  ключ `sandboxPolicy`, как в записи и схеме `ThreadSettings`; обязательный по
  схеме `collaborationMode` — режим `default`).
  Покрывает подтверждение смены модели и режима задачей 3.
- `question_free_text.jsonl` — сконструировано, подтвердить в live smoke. Источник
  формы: `item/tool/requestUserInput` (`ev/question_default_mode.jsonl:28`) с
  `isOther:true`, `options:null`. Покрывает свободный ответ задачей 4.
- `permissions_request.jsonl` — сконструировано, подтвердить в live smoke. Источник
  формы: имя метода из схемы (`item/permissions/requestApproval`); поля повторяют
  остальные approval-запросы. Покрывает карточку разрешений задачей 4.
- `elicitation.jsonl` — сконструировано, подтвердить в live smoke. Источник формы:
  `mcpServer/elicitation/request` из схемы 0.164 (`McpServerElicitationRequestParams`:
  `serverName`, `threadId`, `turnId`, без `itemId`; form — `requestedSchema`,
  url — `url` и `elicitationId`, `openai/userVerification` — `title`,
  `description`, `challenge`). Строки: form с обязательным полем, url, form без
  обязательных полей, `openai/userVerification`, `openai/form`. Покрывает
  elicitation задачей 4 и сразу отклоняемые режимы. Строки 6-8: подтверждение
  инструмента MCP самим Codex (`_meta.codex_approval_kind`, `persist`,
  `tool_params`, `tool_params_display`, пустой `requestedSchema.properties`), та
  же метка с полями формы и форма без метки. Имена ключей взяты из строк бинарника
  0.164, значения выдуманы: живого кадра нет, подтвердить в live smoke.
- `unknown_server_request.jsonl` — сконструировано, подтвердить в live smoke.
  Методы `attestation/generate` и `item/tool/call` из схемы серверных запросов.
  Покрывает ответ-ошибку `-32601` задачей 4.
- `config_read.json`, `account_read.json` — сконструированные сырые ответы (без
  обёртки) для повтора `codex-replay`: пропущенные (`<omitted>`) ответы
  `config/read` и `account/read` и методы без записи. Форма `config/read` — по
  схеме `ConfigReadResponse` (`config`, `origins`, `layers`), значения — по
  записи `ev/start_stream.jsonl:12`; `account/read` — по `ev/diagnostics.jsonl:23`.

## Живые кадры (codex-cli 0.164.0, 01.11, одноразовый CODEX_HOME)

Кадры настоящие. Пути заменены на `/work` и `/codex-home`, шумовые уведомления
(`turn/moderationMetadata`, лимиты, `remoteControl`, `readState`) убраны.

- `async_question_live.jsonl` - `agentMessage` с `delivery:"async"` и `questions`:
  вопрос через `request_user_input_async`, запроса к серверу нет.
- `subagent_live_v2.jsonl` - ход `gpt-6-luna` (`multi_agent_version` v2): порождение
  приходит элементом `subAgentActivity`, `thread/started` потомка нет, первый кадр
  потомка (`thread/status/changed`) обгоняет элемент, потомок выполняет команду.
- `subagent_live_v1.jsonl` - ход `gpt-5.6-luna` (v1): `collabAgentToolCall` с
  `tool:"spawnAgent"`; `model` пуст и `receiverThreadIds` пуст на старте, оба
  заполняются к завершению; кадры потомка идут до этого завершения.
- `subagent_live_outlives_turn.jsonl` - главный ход прерван, потомок работает
  дальше и заканчивает после `turn/completed` главного.
- `goal_cleared_live.jsonl` - `thread/goal/cleared` сразу после `thread/resume`.
- `thread_resume_no_rollout_live.json` - ошибка `thread/resume`, когда рулона нет в
  доме сервера. Та же ошибка приходит и с параметром `path` на рулон другого дома.
