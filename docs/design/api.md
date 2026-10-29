# Контракт API пилота

Документ фиксирует контракт этапа 1.3a, на который опирается PWA (план 1.3b): представление данных, ошибки, маршруты, виды событий и покрытие экранов пилота. Машиночитаемый контракт: [openapi.json](../api/openapi.json) и [events.schema.json](../api/events.schema.json). Требования пилота: [pilot.md](pilot.md); транспорт: [streams.md](streams.md).

## Представление данных

- Идентификаторы передаются строками UUID.
- Все номера последовательности передаются десятичными строками: `scope_seq`, `scope_seq_from`, курсоры снимков, поля запросов `seq`, `created_seq`, `feed_seq`. Остальные целые значения (размеры, счётчики) передаются числами JSON.
- Время передаётся в RFC 3339 UTC с миллисекундами, например `2026-10-27T12:00:00.000Z`.
- `used_permille` имеет диапазон 0..=1000. Отображаемое `used_pct` равно `used_permille / 10` с одним десятичным знаком; пороги конфигурации задаются в процентах.
- Смещения текста (`offset`, `next_offset`, `content_bytes`) измеряются байтами UTF-8. Срез начинается и заканчивается на границе символа; смещение внутри символа отклоняется с `400 invalid_offset`.
- Каждый ответ `/api/` содержит `Cache-Control: no-store` и `X-Server-Time-Ms` (целые миллисекунды часов хаба).

## Области подписки

| Внутренний ключ | Поле `scope` в API |
| --- | --- |
| `owner:{id}:conversations` | `conversations` |
| `owner:{id}:machines` | `machines` |
| `conversation:{id}` | `conversation:<uuid>` |

В кадрах WSS, снимках и `GET /api/v1/events` используются только внешние имена. Области списков разрешаются хабом по аутентифицированному владельцу.

## Виды событий

Таблица перечисляет все клиентские виды (`events::CLIENT_KINDS`, `$defs` и `oneOf` файла [events.schema.json](../api/events.schema.json)); тест `coverage_table_matches_openapi_and_event_schema` сверяет её со схемой. «Разговор» означает область `conversation:{id}`, «список» - `owner:{id}:conversations`, «машины» - `owner:{id}:machines`.

| Область | `kind` | Назначение |
| --- | --- | --- |
| Разговор | `input_queued` | Ввод сохранён хабом и стоит в очереди |
| Разговор | `input_state_changed` | Ввод выдан, на машине, принят, отклонён или отменён |
| Разговор | `queue_state_changed` | Паузы, удержания и слот хода очереди |
| Разговор | `queue_paused` | Очередь поставлена на паузу (прерывание, остановка, ошибка) |
| Разговор | `queue_resumed` | Очередь продолжена, оставшихся пауз нет |
| Разговор | `execution_state_changed` | Состояние исполнения |
| Разговор | `phase_changed` | Фаза хода, имя инструмента и краткий аргумент |
| Разговор | `run_finished` | Ход завершён |
| Разговор | `interrupt_state_changed` | Ход прерывания |
| Разговор | `block_started` | Начало блока: вид, агент, модель, родитель, инструмент, summary |
| Разговор | `block_delta` | Дельта текста блока; соседние дельты в WSS объединяются |
| Разговор | `block_final` | Окончательный текст блока |
| Разговор | `block_incomplete` | Блок оборван с причиной |
| Разговор | `adapter_diagnostic` | Диагностика адаптера |
| Разговор, список | `state_divergence` | Расхождение состояния хаба и машины |
| Разговор | `block_revision_gap` | Пропуск ревизии блока |
| Разговор | `block_checksum_mismatch` | Контрольная сумма блока не совпала |
| Разговор | `block_conflicting_final` | Повторный финал блока с другим содержимым |
| Разговор | `block_media_attached` | Картинка, на которую смотрел агент, опубликована и прикреплена к блоку |
| Разговор, список | `conversation_updated` | Атрибуты разговора; в списке также `counts` |
| Разговор | `conversation_settings_changed` | Настройки следующего хода и их применение |
| Разговор | `crunch_changed` | Настройки «кранча» и переопределение резерва |
| Разговор | `crunch_held` | Удержание по лимиту |
| Разговор | `crunch_woken` | Подтверждённое пробуждение |
| Разговор | `limit_reached` | Факт исчерпания окна |
| Разговор | `scheduled_changed` | Отложенное сообщение |
| Разговор | `attachment_state_changed` | Состояние вложения на хабе и на машине |
| Разговор | `request_opened` | Зарезервировано для плана 1.3c |
| Разговор | `request_closed` | Зарезервировано для плана 1.3c |
| Разговор | `request_decided` | Зарезервировано для плана 1.3c |
| Разговор | `request_transmitting` | Решение передаётся агенту: `{request_id}`, один раз при переходе `decision_saved` в `decision_transmitting` |
| Разговор, список | `derived_changed` | Вычисленное состояние: активность, непрочитанное, очередь, настройки, кранч |
| Список | `conversation_created` | Создание разговора (полный элемент списка) |
| Список | `group_changed` | Группы владельца и счётчики |
| Список | `limits_changed` | Лимиты провайдера |
| Машины | `machine_availability_changed` | Доступность машины |
| Машины | `machine_report_updated` | Каталог и сведения машины |
| Машины | `machine_metrics` | Последний снимок метрик |
| Любая | `hidden` | Заполнитель без полезной нагрузки для внутреннего события |
| Любая (WSS) | `oversized` | Исходный вид события и HTTP-ссылка вместо невместимого payload |

Внутренние виды (`journal_closed`, `execution_fact_rejected`, `journal_record_mismatch`, `local_state_reset`, `limit_observation_unmapped` и будущие) хаб не выпускает: клиент видит на их месте `hidden`.

Событие вне `CLIENT_KINDS` передаётся в WSS и `GET /api/v1/events` как `hidden` с настоящим `scope_seq`, без внутреннего payload. Поэтому курсор каждой области остаётся непрерывным. Соседние дельты одного блока могут объединяться в течение 25 мс: `scope_seq_from` и `committed_at_from` обозначают начало диапазона, `scope_seq` и `committed_at` его конец. ID и correlation ID берутся из первого события. Объединение не перескакивает через чужие события. Соседние `hidden` объединяются по тому же правилу диапазона.

## Удержания очереди

`HoldReason` хранит причины в PG. `HoldView` содержит все эти варианты и вычисляемые `machine_not_reconciled`, `capability_missing { input_id }`, `attachments_pending { input_id, machine_reason? }`. Вычисляемые варианты не десериализуются как `HoldReason`; `attachments_pending` строится из `input_attachments` и в очереди не хранится. Необязательное поле Rust `reason` передаётся как `machine_reason` из `input_attachments.machine_reason`, поскольку JSON-поле `reason` служит тегом варианта.

## Вход и служебные правила

Подробности и обоснование: [ADR-0004](../adr/0004-browser-auth-forward-auth.md).

- Личность задаёт прокси перед хабом. Заголовок (`[http].user_header`, по умолчанию `Remote-User`) принимается, только если TCP-адрес собеседника входит в `[http].trusted_proxies`, а при заданном `proxy_secret_file` запрос несёт `X-Canterlot-Proxy` с этим секретом. Значение заголовка должно быть равно `[http].owner_login`. `X-Forwarded-*` не читается.
- Недоверенный адрес, чужой секрет, отсутствующий или повторённый заголовок личности дают одинаковый `401 unauthenticated`; верный прокси с чужим логином получает `403 forbidden_user`.
- Изменяющий запрос (любой метод, кроме `GET` и `HEAD`) несёт `Origin` из `[http].allowed_origins` и `X-Canterlot-Client: 1`; запрос с `Upgrade` (WSS) несёт разрешённый `Origin`. Нарушение даёт `403 origin_rejected` до проверки личности и до обработчика. `OPTIONS` всегда `405 method_not_allowed`; заголовков `Access-Control-*` хаб не отправляет.
- Каждый изменяющий запрос несёт `Idempotency-Key` по шаблону `^[A-Za-z0-9_-]{1,256}$`. Ключи с `:` (пространства `sched:` и `wake:`) принадлежат хабу и отклоняются с `400 invalid_request`.
- Тело JSON не больше `[http].max_json_body_bytes` (512 КиБ); превышение даёт `413 resource_limit`.
- Каждый ответ `/api/` несёт `Cache-Control: no-store` и `X-Server-Time-Ms`; каждый ответ несёт `X-Correlation-Id`. `GET /healthz` и `GET /readyz` не требуют личности и сообщают `ok` либо `not_ready` с причиной `database` или `service` и именами недоступных фоновых служб.

## Errors

Ошибки имеют форму `{"error":{"code","message","scope","retry","correlation_id"}}`; необязательный `details` имеет закрытый словарь без пользовательского содержимого. `code` записывается в snake_case, `message` является короткой русской фразой; `scope` и `retry` берутся из [словаря ошибок](README.md#ошибки-63). Клиент определяет повтор по `retry`. Хаб принимает UUID из `X-Correlation-Id` либо создаёт новый и всегда возвращает его.

| `code` | HTTP | `scope` | `retry` | `message` |
| --- | --- | --- | --- | --- |
| `unauthenticated` | 401 | `auth` | `after_condition` | Требуется вход через сервис авторизации |
| `forbidden_user` | 403 | `auth` | `never` | Доступ для этого пользователя запрещён |
| `origin_rejected` | 403 | `auth` | `never` | Запрос отклонён: недопустимый источник |
| `invalid_request` | 400 | `browser` | `never` | Некорректный запрос |
| `invalid_offset` | 400 | `browser` | `never` | Недопустимое смещение |
| `not_found` | 404 | `browser` | `never` | Не найдено |
| `method_not_allowed` | 405 | `browser` | `never` | Метод не поддерживается |
| `idempotency_conflict` | 409 | `browser` | `never` | Ключ идемпотентности уже использован с другим запросом |
| `decision_conflict` | 409 | `browser` | `never` | Решение уже принято с другого устройства |
| `request_not_open` | 409 | `browser` | `never` | Запрос уже закрыт |
| `decision_invalid` | 422 | `browser` | `never` | Ответ не подходит к вопросу |
| `resource_limit` | 413 | `browser` | `never` | Превышен допустимый размер данных |
| `not_implemented` | 501 | `hub_db` | `never` | Функция пока не реализована |
| `too_many_requests` | 429 | `hub_db` | `safe_same_key` | Слишком много запросов, повторите позже |
| `db_unavailable` | 503 | `hub_db` | `safe_same_key` | База данных временно недоступна |
| `internal` | 500 | `hub_db` | `safe_same_key` | Внутренняя ошибка хаба |
| `queue_full` | 409 | `delivery` | `after_condition` | Очередь разговора заполнена |
| `conversation_closed` | 409 | `delivery` | `never` | Разговор закрыт |
| `execution_already_live` | 409 | `delivery` | `after_condition` | Исполнение уже запущено |
| `invalid_state` | 409 | `delivery` | `never` | Действие недопустимо в текущем состоянии |
| `not_pending` | 409 | `browser` | `never` | Отложенное сообщение уже обработано |
| `schedule_time_out_of_range` | 422 | `browser` | `never` | Время отправки вне допустимого диапазона |
| `too_many_scheduled` | 409 | `browser` | `never` | Слишком много отложенных сообщений |
| `machine_not_found` | 404 | `browser` | `never` | Машина не найдена |
| `machine_unavailable` | 409 | `delivery` | `after_condition` | Машина недоступна |
| `capability_missing` | 409 | `delivery` | `after_condition` | Требуется обновление воркера |
| `limits_mismatch` | 409 | `browser` | `never` | Лимиты вложений машины не совпадают |
| `harness_unknown` | 422 | `browser` | `never` | Харнесс отсутствует в каталоге машины |
| `model_unknown` | 422 | `browser` | `never` | Модель отсутствует в каталоге харнесса |
| `effort_not_allowed` | 422 | `browser` | `never` | Усилие недоступно для модели |
| `permission_mode_unknown` | 422 | `browser` | `never` | Режим разрешений отсутствует в каталоге |
| `settings_midsession_unsupported` | 422 | `browser` | `never` | Смена настроек во время исполнения не поддерживается |
| `profile_unknown` | 422 | `browser` | `never` | Профиль инструкций отсутствует в каталоге |
| `workdir_not_allowed` | 422 | `browser` | `never` | Рабочий каталог вне разрешённых корней |
| `group_not_found` | 404 | `browser` | `never` | Группа не найдена |
| `group_name_taken` | 409 | `browser` | `never` | Группа с таким именем уже существует |
| `archive_active` | 409 | `browser` | `never` | В сессии есть активная работа |
| `attachment_not_ready` | 409 | `browser` | `after_condition` | Вложение ещё не готово |
| `attachment_not_uploadable` | 409 | `browser` | `never` | Загрузка вложения уже завершена или прекращена |
| `attachment_unsupported` | 422 | `browser` | `never` | Харнесс не принимает такое вложение |
| `attachment_unavailable` | 409 | `browser` | `never` | Вложение больше недоступно |
| `offset_mismatch` | 409 | `browser` | `after_condition` | Смещение загрузки не совпадает с принятым |
| `upload_in_progress` | 409 | `browser` | `after_condition` | Вложение уже загружается |
| `checksum_mismatch` | 422 | `browser` | `never` | Содержимое не совпало с контрольной суммой |
| `attachment_conflict` | 409 | `browser` | `never` | Вложение уже завершено с другой контрольной суммой |

Строки таблицы совпадают с перечислением `ErrorCode` хаба и со схемой `components.schemas.ErrorCode` в `docs/api/openapi.json`; тест `error_code_table_in_api_md_matches_the_enum_and_openapi` сверяет код, статус, `scope`, `retry` и сообщение. Код, который повторяется тем же ключом (`safe_same_key`), бывает только у 429, 500 и 503: условие проходит само. Отказ с другим статусом оставляет ключ свободным и сообщает в `retry`, что нужно для повтора. Каждая новая задача добавляет свои коды в перечисление и в эту таблицу в одном коммите.

Разбивка по областям: `auth` (вход и источник запроса), `browser` (форма запроса, ключ идемпотентности, размеры), `hub_db` (хаб и PostgreSQL), `delivery` (очередь, разговор, исполнение). Чужой идентификатор неотличим от отсутствующего: оба дают `not_found`.

`decision_conflict` содержит `details.winner_decision` и `details.current_state`; `request_not_open` содержит `details.current_state` и `details.reason` (`closed_without_decision` или `generation_advanced`). `decision_invalid` содержит `details.question_id`, если ошибка относится к конкретному вопросу. Решения и ответы в details доступны только владельцу сессии и не записываются в журналы ошибок.

Для недопустимой ссылки `retry_of` код `invalid_request` возвращается со статусом 422.

Ответы `offset_mismatch`, `upload_in_progress` и `resource_limit` загрузки вложения несут заголовок `Upload-Offset` с принятым смещением; оборванное тело загрузки даёт `400 invalid_request` с тем же заголовком.

## Маршруты и покрытие

Маршруты ядра HTTP (задача 4); полная схема в [openapi.json](../api/openapi.json), перегенерация `scripts/gen-openapi.sh`:

| Метод и путь | Назначение |
| --- | --- |
| `GET /healthz` | Живость процесса |
| `GET /readyz` | База отвечает, установка совпадает с манифестом, фоновые службы готовы |
| `GET /api/v1/me` | `{owner_id, login}` владельца |
| `GET /api/v1/config` | Лимиты, пороги, путь и протокол WSS, `logout_url` |
| `GET /api/v1/openapi.json` | Документ OpenAPI |
| `GET /api/v1/conversations/{id}/scheduled` | Отложенные сообщения разговора |
| `POST /api/v1/conversations/{id}/scheduled` | Запланировать обычный ввод |
| `PATCH /api/v1/scheduled/{id}` | Изменить время или содержимое до отправки |
| `DELETE /api/v1/scheduled/{id}` | Отменить до отправки |

Отложенные сообщения принимают `run_at` от секунды до 366 дней вперёд, `content` как обычный ввод и `attachment_ids` готовых вложений (раздел «Вложения»). Создание возвращает 201, изменение и отмена 200; повтор ключа возвращает сохранённый ответ. Список содержит до 200 сообщений, сначала ожидающие, внутри каждой группы по `run_at` и `id`. `scheduled_changed` несёт полное представление сообщения в области разговора. Планировщик ставит ввод с ключом `sched:<id>` в той же транзакции, в которой помечает задание отправленным; паузы и удержания очереди действуют как обычно.

`derived_changed` публикуется в области разговора, затем в списке владельца и содержит `conversation_id`, `activity:{state,evidence,since}`, `unread_count`, `queue:{queued,pause_reasons,hold_reasons}`, `last_activity_at`, `settings:{model,effort,permission_mode}`, `crunch:{enabled}`. Настройки и кранч материализуются из эффективных значений разговора; изменение публикует только проектор. `last_activity_at` берётся по времени фиксации последнего ввода или создания блока и округляется вниз до минуты. Дельты блока не меняют это время. Прочитанность считается по `created_seq` текстовых блоков, предупреждений и ошибок в состояниях `streaming` и `final` относительно `last_read_seq` разговора.

Соответствие экранам пилота и полный список маршрутов и видов событий: раздел [«Покрытие экранов пилота»](#покрытие-экранов-пилота).


## Сессии, группы и чтение данных машин

| Метод и путь | Результат |
| --- | --- |
| `POST /api/v1/conversations` | 201, `conversation_id` и необязательное `execution` |
| `GET /api/v1/conversations` | `items`, `next_cursor`, `counts` |
| `GET /api/v1/conversations/{id}` | Атрибуты, проекция активности, настройки, кранч, живое исполнение и удержания |
| `PATCH /api/v1/conversations/{id}` | Изменение `title`, `color`, `pinned`, `archived`; `force` допускается при архивировании |
| `PUT /api/v1/conversations/{id}/groups` | Замена набора `group_ids` |
| `PUT /api/v1/conversations/{id}/crunch` | Сохранение `enabled` и `resume_text` |
| `POST /api/v1/conversations/{id}/spend-reserve` | 200, `continued`, `already_continued` или `not_held`; продолжение этой сессии за счёт недельного резерва |
| `POST /api/v1/conversations/{id}/read` | 204, монотонный максимум `seq` (десятичная строка), ключ не нужен |
| `GET /api/v1/groups` | Группы по `position`, затем `id` |
| `POST /api/v1/groups` | 201, новая группа с `name` |
| `PATCH /api/v1/groups/{id}` | Имя и/или позиция группы |
| `DELETE /api/v1/groups/{id}` | 204; разговоры сохраняются |
| `GET /api/v1/machines` | Каталоги, профили, ОС, доступность, метрики, сессии, предупреждения |
| `GET /api/v1/machines/{id}` | Та же структура одной машины |
| `GET /api/v1/machines/{id}/workdirs/recent` | `workdirs`, последние 20 различных каталогов этой машины |
| `GET /api/v1/limits` | Лимиты провайдеров: пятичасовое и недельное окна, `weekly_reserve_active`, `weekly_reserve_resets_at` |
| `GET /api/v1/limits/history` | `usage` по дню, сессии, модели и агенту; `series` с обоими окнами |

Создание проверяет сохранённый каталог машины, `pilot_v1`, модель, усилие, режим разрешений, профиль, разрешённый корень и группы. Пустой режим разрешений наследуется от владельца, профиль по умолчанию `owner`. Без `start` машина может быть недоступна. При `start=true` запись разговора, очереди, исполнения, команды, события и ключа фиксируется одной транзакцией. Текст по умолчанию для кранча копируется из `[limits].default_resume_text` при включении. Пока действует недельный резерв провайдера, новая сессия создаётся с удержанием `weekly_reserve` (см. ниже).

Список принимает `filter=all|pinned|archived|group:<uuid>`, `q`, `cursor`, `limit` от 1 до 200 (по умолчанию 50). `all`, `pinned` и группы скрывают архив. Счётчики не зависят от поиска: `all`, `pinned`, `archived`, `groups:[{id,count}]`, группы считают только неархивные разговоры. Поиск подстроки использует ICU, `%`, `_` и обратный слеш считаются буквальными символами. Сортировка: закрепление, время закрепления по убыванию, активность по убыванию (NULL в конце), UUID по возрастанию. Непрозрачный base64url-курсор содержит четыре ключа, фильтр, поисковую строку и уже показанные UUID; перемещение показанной строки ниже границы не создаёт повторов. Курсор нельзя переносить между фильтрами или владельцами. Изменения выше уже пройденной границы требуют обновления списка.

`ListItem.machine` содержит только `id` и `name`. Атрибуты публикуются в `conversation_created` (полный элемент списка) и `conversation_updated` (атрибуты без проекции); оба события несут актуальные `counts`. Изменение атрибутов сначала публикуется в области разговора, затем в списке. Удаление группы публикует изменения всех её бывших участников (сначала все области разговоров по UUID), затем `group_changed` с новым списком групп и `counts`. В `derived_changed` приходят только изменяемые проектором поля. Доступность машины принадлежит области `machines`.

Архивирование активной сессии возвращает `archive_active` с `details.reasons` из `crunch`, `scheduled`, `live_execution`. `force=true` выключает кранч, как `PUT …/crunch` с `enabled=false`, и отменяет ожидающие сообщения и пробуждения; если планировщик уже захватил задание, операция откатывается и возвращает `scheduled`. Само живое исполнение при принудительном архивировании продолжается.

У машины `availability_since` соответствует `machines.availability_at`, `heartbeat_age_ms` берётся из Registry (при отсутствии соединения `null`). Предупреждения: `disk_usage_high` при использовании диска не ниже `[machines].disk_warn_pct`, `metrics_stale` при отсутствии или возрасте метрик не меньше `[machines].metrics_stale_after`, `limits_mismatch` при расхождении лимитов вложений.

История принимает `provider`, `from`, `to` (RFC 3339), `tz` (имя зоны PostgreSQL, по умолчанию UTC). По умолчанию диапазон равен `[limits].history_days`, максимальный диапазон 366 дней, правая граница исключается. Неверная зона даёт `invalid_request`. `used_permille` хранится целым числом, `used_pct = used_permille / 10` вычисляется при чтении. Серии содержат `observed_at`, `used_permille`, `used_pct`.

## Лимиты, «кранч» и недельный резерв

Правила [pilot.md §8](pilot.md#8-кранч-и-паузы-по-лимитам) поверх хранения лимитов. Провайдер разговора выводится из его харнесса; лимиты Claude и Codex не влияют друг на друга.

- Пятичасовое окно не останавливается заранее. Факт адаптера `limit_reached` без кранча даёт паузу `limit_reached{provider,window,resets_at}`. С кранчем очередь получает удержание `quota_exhausted{provider,window,wake_at}` (одно на провайдера и окно), `wake_at` равно времени сброса или `now + [limits].unknown_reset_retry`, если сброс неизвестен. У разговора выставляется `resume_pending`, планируется задание пробуждения на `wake_at + reset_grace`, пишется `crunch_held`. Повторный факт с более поздним сбросом переносит и удержание, и задание. Идущий ход и процесс не трогаются.
- Пробуждение подтверждается лимитами, а не часами: нужно более новое показание того же окна со сбросом позже `wake_at` и `used_permille < 1000`; без времени сброса достаточно показания не раньше `wake_at`. Повтор доналимитного наблюдения подтверждением не служит. Без подтверждения задание повторяется через `verify_backoff` (последний шаг повторяется) и после commit просит одну доступную машину провайдера с `pilot_v1` опросить лимиты: не чаще `poll_min_interval`, после исчерпания шагов не чаще `poll_floor.<provider>`.
- Подтверждённое пробуждение снимает удержание. Текст продолжения (`crunch.resume_text`, иначе `[limits].default_resume_text`) ставится обычным вводом с ключом `wake:<id задания>` только если кранч включён, `resume_pending` выставлен, исполнение живо, в очереди нет ввода владельца и нет паузы, а недельный резерв провайдера не действует. Иначе пробуждение только снимает удержание; результат и `crunch_woken` содержат `skipped`: `no_live_execution`, `queue_not_empty`, `resume_already_queued`, `paused`, `weekly_reserve`, `not_pending`, `crunch_disabled` или `closed`. Пробуждение не запускает исполнение.
- Недельный резерв: новое недельное показание не ниже `weekly_reserve_pct` включает `weekly_reserve_active` в той же транзакции и ставит задание правила. До его выполнения шлюз выдачи уже считает флаг выводимым удержанием `weekly_reserve`. Правило ставит удержание `weekly_reserve{provider,resets_at}` всем незакрытым разговорам провайдера, кроме переопределённых, отзывает ещё не переданный выданный ввод и планирует проверку провайдера на сброс недели плюс `reset_grace`. Идущий ход доходит до конца. Выход из резерва: новое показание со сбросом позже окна входа больше чем на час и ниже порога, либо два новых показания не выше порога минус `reserve_exit_hysteresis_permille`, разнесённые на `snapshot_min_interval`. При выходе снимаются все удержания `weekly_reserve` провайдера и все переопределения, пишется `limits_changed`, а сессиям в кранче с `resume_pending`, которые были в резерве (с удержанием или с переопределением), ставится пробуждение с текстом продолжения. Остальные только теряют удержание.
- `spend-reserve` снимает удержание `weekly_reserve` одной сессии и выставляет `crunch.reserve_override_until` до сброса недели (иначе на 7 дней), повтор с другим ключом даёт `already_continued`, сессия без удержания `not_held`. Автоматика резерв не тратит: при активном резерве пробуждение не отправляет текст даже переопределённой сессии.
- `resume_pending` (поле `crunch.resume_pending` карточки разговора) снимается, когда поставлен текст продолжения, когда владелец отправил ввод, при остановке или прерывании владельцем, при выключении кранча и при пропусках `queue_not_empty`, `paused`, `closed`, `crunch_disabled`. При `no_live_execution` и `weekly_reserve` флаг сохраняется до срока: конец следующего недельного окна (ближайший известный сброс плюс 7 дней, иначе 14 дней от остановки лимитом). Новое или перенесённое удержание получает новый срок; повтор того же факта его не продлевает. Поздний `limit_reached` остановленного владельцем хода не выставляет флаг даже после снятия `user_stop`: у исполнения сохраняется номер последнего прерванного ввода, а остановка исполнения запрещает автопродолжение всех его ходов. По истечении планировщик снимает флаг с `crunch_changed`, даже если заданий больше нет. Выключение кранча превращает удержание `quota_exhausted` в паузу `limit_reached` и отменяет ожидающие пробуждения; включение существующие паузы не меняет.

Полезная нагрузка. `crunch_held`: `provider`, `window`, `reason` (`quota_exhausted` или `weekly_reserve`), `wake_at`, `resume_pending`; `crunch_woken`: `provider`, `window`, `hold_key`, `input_id`, `skipped`, `resume_pending`; `crunch_changed`: `enabled`, `resume_pending`, `reserve_override_until`. Удержания с данными видны в `queue.hold_reasons` элемента списка и в `holds` карточки разговора.

## Очередь, настройки, запуск и повтор

| Метод и путь | Тело | Результат |
| --- | --- | --- |
| `POST /api/v1/conversations/{id}/inputs` | `content[]` (`{"type":"text","text"}`), `attachment_ids?` (до 10 готовых вложений), `retry_of?` | 201, `SubmittedInput`; блоки `attachment` хаб добавляет после текста сам |
| `POST /api/v1/inputs/{id}/cancel` | - | `CancelOutcome` |
| `POST /api/v1/conversations/{id}/interrupt` | - | `StopOutcome`: пауза очереди, прерывание, отзыв |
| `POST /api/v1/conversations/{id}/stop` | `force?` | `StopExecutionOutcome` |
| `POST /api/v1/conversations/{id}/queue/continue` | - | `ContinueOutcome` |
| `PATCH /api/v1/conversations/{id}/settings` | `model?`, `effort?`, `permission_mode?` | `{effective, pending, rev}` |
| `POST /api/v1/conversations/{id}/executions` | - | 201, первый запуск |
| `POST /api/v1/conversations/{id}/resume` | `continue_queue?` | 201, новое исполнение с последним `native_conversation_id`; с `continue_queue` снятие паузы в той же транзакции |
| `POST /api/v1/conversations/{id}/requests/{request_id}/decision` | `DecisionBody` | 200 `DecisionOutcomeView`, первый ответ побеждает |

Доменные функции отмены, остановки, продолжения и ввода сами ведут ключ идемпотентности; `settings`, `executions` и `resume` оборачивает `idempotency::run_in_tx`. Чужой идентификатор даёт `404 not_found`. После фиксации транзакции маршрут будит диспетчер разговора.

Настройки проверяются как итоговая комбинация по сохранённому каталогу машины: модель, усилие этой модели, режим разрешений харнесса. Допустимое изменение попадает в `pending_settings`, увеличивает `settings_rev` и публикует `conversation_settings_changed {effective, pending, rev}` в области разговора. Диспетчер повторно проверяет комбинацию под блокировкой очереди, кладёт действующие значения в команду (`settings`, `settings_rev`) и снимает недопустимые ключи с событием, где добавлено `rejected:{keys, reason:"settings_unavailable"}`. `input_accepted` делает отправленные значения эффективными и убирает из ожидающих только ключи с тем же значением. Отказ до передачи с причиной `settings_unavailable` убирает ключи отклонённой команды и не ставит паузу. Ввод с настройками не уходит в сеанс без `pilot_v1`: он остаётся в очереди, а `detail` показывает вычисляемое удержание `capability_missing`.

`POST …/requests/{request_id}/decision` требует `Idempotency-Key` и `X-Canterlot-Client: 1`. `DecisionBody` различается по `kind`: `permission` принимает `behavior: "allow" | "deny"` и необязательный `message`, `question` принимает `answers`, словарь по id вопросов со строкой для одиночного выбора или массивом строк для множественного. Контракт из `requests.request_payload.questions` должен содержать от 1 до 4 вопросов. Каждый вопрос требует ответ, посторонние id, пустые и повторные значения отклоняются. Значения совпадают с `options[].label` точно, без перевода или обрезки; свободный текст допускается только при явном `allow_custom`, `isOther` или `is_other`, равном `true`. Множественный выбор задаётся `multiSelect` или `multi_select`, равным `true`.

Успех: 200 `{request_id, winner_decision_id, state: "decision_saved", decision}`. Решение и команда `deliver_decision` сохраняются атомарно. Одинаковый ключ и тело возвращают первый ответ, даже после продвижения состояния запроса; другое тело, request id или сессия с тем же ключом дают `409 idempotency_conflict`. Другой ключ после принятого решения даёт `409 decision_conflict`; закрытый запрос или смена поколения дают `409 request_not_open`. Отсутствующий запрос, запрос другой сессии и чужая сессия дают `404 not_found`. Неверная форма тела даёт `400 invalid_request`, несовпадение с контрактом запроса даёт `422 decision_invalid`.

`retry_of` обязан указывать на ввод той же сессии в состоянии `acceptance_unconfirmed`, иначе `422 invalid_request`; значение входит в хеш запроса. Первый ввод сессии с автоматическим названием задаёт название: первые 60 символов по границе слова, пробелы сжимаются, запасное значение `Новая сессия`.

## Вложения

| Метод и путь | Тело | Результат |
| --- | --- | --- |
| `POST /api/v1/attachments` | `conversation_id`, `name`, `mime?`, `size`, `sha256` | 201, `AttachmentView` и `upload {offset: 0, upload_chunk_bytes}` |
| `HEAD /api/v1/attachments/{id}/content` | - | 200, `Upload-Offset`, `Upload-Length` |
| `PATCH /api/v1/attachments/{id}/content` | заголовок `Upload-Offset`, тело до `upload_chunk_bytes` | 204, `Upload-Offset` |
| `POST /api/v1/attachments/{id}/complete` | `sha256?` | 200, `AttachmentView` в состоянии `ready` |
| `DELETE /api/v1/attachments/{id}` | - | 204 |
| `GET /api/v1/attachments/{id}` | - | `AttachmentView`; `uploaded_bytes` у незавершённой загрузки |
| `GET /api/v1/attachments/{id}/content` | - | содержимое готового вложения |

Размер от 1 до 104857600 байт, больше `413 resource_limit`; объявленный размер служит и потолком загрузки. Имя сводится к последнему компоненту пути, не длиннее 255 байт, без NUL и управляющих символов. `mime` хранится без параметров в нижнем регистре, по умолчанию `application/octet-stream`; `sha256` состоит из 64 шестнадцатеричных символов.

`PATCH` не требует `Idempotency-Key`: повтор определяется смещением. Маршрут имеет собственный предел тела 8 МиБ, общий предел JSON к нему не относится, а предел времени чтения тела отсчитывается от последнего принятого кадра. Тело пишется в `staging/<id>.part` кусками до 128 КиБ; принятое смещение равно длине файла, байты оборванного тела сохраняются. На одно вложение пишется одно тело: второй `PATCH` или `HEAD` во время записи получает `409 upload_in_progress` с текущим `Upload-Offset`, неверное смещение `409 offset_mismatch`. Одновременно принимается не больше `[attachments].max_concurrent_uploads` тел, сверх этого `429 too_many_requests`. Часть для `failed`, `cancelled` или `ready` даёт `409 attachment_not_uploadable`.

При запуске hub закрепляет всю цепочку `blob_dir` через fsync родителей. PATCH, публикация, отмена и GC используют общий для всех процессов `flock` на постоянном `blob_dir/locks/<id>`; захват живёт до конца фактического I/O и commit. После захвата PATCH перечитывает строку и физическое смещение; занятое вложение даёт `409 upload_in_progress`, GC его пропускает.

`complete` (§13.1) сначала проверяет ключ и отмечает активность, затем вне транзакций проверяет длину (`409 offset_mismatch` при неполной загрузке) и SHA-256 (`422 checksum_mismatch`, запись `failed`, staging удаляется), делает `fsync` файла, обеспечивает shard и синхронизирует его родителя `blobs/` при каждой публикации, затем `rename` в `blobs/<id[0..2]>/<id>` и `fsync` shard и staging. Только после этого короткая транзакция переводит запись из `created`/`uploading` в `ready` и добавляет `attachment_state_changed`. Сбой между публикацией и commit оставляет запись `uploading` и blob; повтор с тем же ID и checksum не копирует содержимое и фиксирует метаданные. Тело `{sha256}` необязательно; другая контрольная сумма для того же вложения даёт `409 attachment_conflict`.

`DELETE` переводит загрузку из `created`/`uploading` в `cancelled`, готовое вложение без ссылок в `released`; вложение со ссылкой из ввода или отложенного сообщения даёт `409 invalid_state`, конечные состояния повторно дают 204. Файлы удаляются после commit.

Ввод и отложенное сообщение принимают до 10 `attachment_ids`, больше `413 resource_limit`, повтор id `400 invalid_request`. Транзакция ввода блокирует записи `for share` по возрастанию id и требует владельца, разговор и `ready`, иначе `409 attachment_not_ready`, и ввод не создаётся. Сохранённый отчёт машины должен содержать `pilot_v1` (`409 capability_missing`) и совпадающие лимиты (`409 limits_mismatch`); живой сеанс не проверяется, поэтому ставить в очередь можно и при выключенной машине. Файл `image/*` для харнесса без изображений даёт `422 attachment_unsupported`. Ссылка агенту несёт `image = true` только для `image/png`, `image/jpeg`, `image/gif`, `image/webp`. Команда получает блоки `attachment` после текста и `attachment_ids`; диспетчер не отдаёт её сеансу без `pilot_v1` (вычисляемое удержание `capability_missing`). Хеш запроса покрывает `attachment_ids` по порядку.

Отложенное сообщение хранит ссылки в `scheduled_job_attachments`, и сборщик их учитывает. Отправка перепроверяет готовность: снятое вложение переводит задание в `failed` с `attachment_unavailable`. Отмена, отказ и отправка задания снимают его ссылки в той же транзакции.

Выдача доступна только владельцу. `Content-Type` берётся из списка `image/png`, `image/jpeg`, `image/gif`, `image/webp` при совпадающей сигнатуре файла, иначе `application/octet-stream`; ответ несёт `X-Content-Type-Options: nosniff`, `Content-Disposition: attachment; filename*=UTF-8''<имя в процентной кодировке>`, `Content-Security-Policy: sandbox`, `Cache-Control: private, no-store`. Незавершённое вложение даёт `409 attachment_not_ready`, снятое `409 attachment_unavailable`.

Сборщик (`canterlot:attachment-gc`, раз в `gc_interval`, отсрочка `gc_grace` = 24 ч) переводит загрузки без активности в `failed(upload_expired)` и удаляет staging после commit, переводит готовые вложения без ссылок в `released` и удаляет blob после commit, удаляет файлы staging без записи `created`/`uploading` и blob без записи `created`/`uploading`/`ready`, если они старше отсрочки. Каждое решение берёт строку `for update skip locked` и перепроверяет состояние, время и ссылки после блокировки. Тело загрузки, которое пишется в любом процессе с общим томом, сборщик не трогает.

`attachment_state_changed {attachment_id, state, reason}` публикуется в области разговора на каждый переход: `created`, `uploading`, `ready`, `failed` (с причиной), `cancelled`, `released`. Имени и содержимого файла в событии нет.

### Выдача вложений воркеру

Ссылки `AttachmentRef` входят в `deliver_input`; отдельной команды `deliver_attachment` нет ([streams.md](streams.md) §2, §6). `WorkerData.FetchAttachment {attachment_id, offset}` использует проверенный TLS канала данных и `authorization: Bearer …`. Доступно только опубликованное вложение, связанное с неотменённым и неотклонённым вводом разговора этой машины. Чужое, отсутствующее или непривязанное вложение одинаково возвращает `NOT_FOUND`.

Ответ: поток `AttachmentChunk {offset, data, last}`, размер `data` до 131072 байт; смещения непрерывны от запрошенного, последний чанк имеет `last=true`. Смещение больше размера возвращает `OUT_OF_RANGE`, равное размеру даёт пустой последний чанк. До четырёх потоков на машину; пятый получает `RESOURCE_EXHAUSTED`, что считается транспортным ожиданием без расходования попыток содержимого. Blob читается через `BlobStore::open` после освобождения соединения PostgreSQL. При штатной остановке открытый поток завершается `UNAVAILABLE`; повторный запрос продолжает чтение с указанного смещения. Фоновых задач передачи нет.

Worker при подготовке исполнения синхронизирует `data_dir` после обеспечения `executions/`, затем `executions/` после создания каталога исполнения, до записи ownership, readiness и transmission. Хранилище вложений опирается на эту долговечность каталога исполнения.

Новая попытка команды атомарно с dispatch сбрасывает `input_attachments.machine_state` в `pending`, очищает причину и retryability и при изменении публикует `attachment_state_changed`. Готовность предыдущей попытки не переносится; повторы фактов той же попытки остаются идемпотентными.

`CJ`-записи применяются к связке команды, текущего `dispatch_attempt` и вложения своей машины. Чужие записи, устаревшие попытки и отсутствующие связи подтверждаются без изменения данных. `attachment_ready` устанавливает `machine_state=ready` и очищает причину; `attachment_failed{retryable:true}` оставляет `pending` и записывает причину; `attachment_failed{retryable:false}` устанавливает `failed`. В той же транзакции только при изменении связки добавляется `attachment_state_changed {input_id, attachment_id, machine_state, machine_reason}` в область разговора. Повторы диапазона и повторные сообщения о том же состоянии не создают второе событие.

Удержание `attachments_pending` вычисляется из `input_attachments` ([delivery.md](delivery.md) §3). Проектор публикует его в `derived_changed.queue.hold_reasons`, детали разговора возвращают то же удержание без дублей. Поле `reason` содержит `attachments_pending`, поле `machine_reason` содержит, например, `hub_unreachable`. Когда все файлы готовы, удержание исчезает. Медленная передача, обрыв или перезапуск хаба не отклоняют ввод: окончательный отказ до передачи оформляется только отдельной записью воркера `command_rejected{reason:"attachment_unavailable"}`.

Воркер публикует файлы в `data_dir/executions/<execution_id>/attachments/`, с проверкой длины и SHA-256, `fsync` файла, `rename` и `fsync` каталогов. Рабочий каталог репозитория не используется. Загрузка каждого ввода выполняется отдельной отменяемой задачей; `Deliver.files` с абсолютными путями уходит сопровождению только после публикации всех вложений и записи квитанций в CJ. Повтор проверяет опубликованный файл и повторяет готовность, сохраняя запрет повторной передачи ввода. Отмена и отзыв до выдачи сопровождению отвечаются управляющей службой и удаляют временные файлы; stop и force stop также прерывают загрузку. Копии исполнения удаляются после его завершения и полного подтверждения SJ.

`fetch_attempts` расходуется только на ошибки содержимого. Транспортные ошибки, включая `RESOURCE_EXHAUSTED`, повторяются с backoff до `fetch_backoff_cap`; после reconnect загрузка продолжается с длины `.tmp/<attachment_id>.part`. Одна непрерывная недоступность хаба даёт одну `attachment_failed{retryable:true,reason:"hub_unreachable"}`. Исчерпание попыток содержимого, несоответствие размера и отсутствие доступа к файлу дают окончательную ошибку и отклонение команды. После подтверждённой отмены или отзыва поздние результаты fetch игнорируются. Квитанции вложений разрешены только для команды, принятой в сеансе с `pilot_v1`, в том числе после перезапуска воркера.

## Картинки, на которые смотрел агент

| Метод и путь | Результат |
| --- | --- |
| `GET /api/v1/media/{id}` | содержимое картинки, прикреплённой к блоку разговора владельца |

Картинка (pilot.md §5.7) проходит путь вложения в обратную сторону. Воркер берёт байты из результата инструмента или сразу в момент события читает файл по пути из вызова. Он определяет тип по сигнатуре (`image/png`, `image/jpeg`, `image/webp`, `image/gif`), считает SHA-256 и размеры и кладёт копию в `data_dir/executions/<execution_id>/media/<media_id>` (каталог `0700`, файл `0600`). Файл крупнее 20 МиБ (`MAX_MEDIA_BYTES` = 20971520) или другого типа картинки не даёт: блок сохраняет путь в summary и размер в тексте результата. Байты и пути не попадают в журналы, метрики и `Debug`.

Загрузка идёт клиентским потоком `WorkerData.PutMedia` по отдельному соединению HTTP/2 (ни поток SJ, ни управляющий сеанс его не ждут). Первый кадр `MediaChunk` несёт `MediaHeader {media_id (UUID v7 воркера), execution_id, block_id, mime, size, sha256}`, следующие несут данные до 131072 байт. Хаб принимает поток только для исполнения своей машины (иначе `NOT_FOUND`) и не больше четырёх потоков на машину (`RESOURCE_EXHAUSTED`). Тело пишется в `staging/<id>.part` того же хранилища, что вложения. Затем хаб проверяет длину (`INVALID_ARGUMENT`) и SHA-256 (`DATA_LOSS`), делает `fsync` файла, `rename` в `blobs/<id[0..2]>/<id>` и `fsync` каталогов. После этого он сверяет сигнатуру с `mime` (`INVALID_ARGUMENT`, blob удаляется) и только тогда фиксирует строку `block_media`. Ответ `MediaStored {media_id}` уходит после commit. Повтор с тем же ID и тем же заголовком возвращает тот же ответ без копирования. Сбой между публикацией и commit оставляет blob без строки: повтор проверяет опубликованный blob и фиксирует метаданные. Другой checksum, блок или исполнение под тем же ID, а также ID вложения дают `ALREADY_EXISTS`. Второе тело того же ID во время приёма получает `UNAVAILABLE`.

Сопровождение пишет запись SJ `BlockMedia {block_id, media_id, mime, size, sha256, width, height}` только после `MediaStored`, поэтому хаб не видит ссылок на неопубликованное. Приём записи требует строку того же исполнения с совпадающими блоком, типом, размером и checksum и существующий блок этого исполнения. Тогда в той же транзакции появляется `attached_at` и событие `block_media_attached {block_id, media_id, mime, size, width, height}` в области разговора; иначе запись пропускается, курсор журнала идёт дальше. Повтор записи второго события не даёт. Ширина и высота `0` означают неизвестный размер и отдаются как `null`.

Загрузкой управляет управляющая служба воркера по запросу сопровождения через IPC (`MediaUpload` и ответ `MediaResult`), по одной картинке на исполнение. Транспортные ошибки и `UNAVAILABLE`, `RESOURCE_EXHAUSTED` повторяются с backoff до `fetch_backoff_cap`, в том числе через переподключение к хабу и перезапуск управляющей службы: сопровождение повторяет незавершённый запрос после нового `Attach`. `DATA_LOSS` повторяется не больше трёх раз; `ALREADY_EXISTS`, `INVALID_ARGUMENT`, `NOT_FOUND` и пропавший файл окончательны, ссылка не пишется. Картинки, не опубликованные к закрытию журнала исполнения, отбрасываются. Запись `BlockMedia` относится к `pilot_v1`: сопровождение пишет её только при `pilot_v1` в `StartExecution`.

Модели чтения блока (история, снимок разговора) несут `media: [{media_id, mime, size, width, height}]` в порядке прикрепления. Выдача доступна только владельцу и только прикреплённой картинке; чужая, неприкреплённая, отсутствующая или с пропавшим blob дают одинаковый `404 not_found`. Ответ несёт `Content-Type` из проверенного типа, `Content-Length`, `X-Content-Type-Options: nosniff`, `Content-Disposition: inline`, `ETag` (SHA-256 в кавычках), `Content-Security-Policy: sandbox`, `Cache-Control: private, no-store`. Миниатюра строится в браузере.

Сборщик вложений не трогает blob, у которого есть строка `block_media`. Строку без ссылки из блока старше `gc_grace` он удаляет вместе с blob (blob после commit), staging приёма без строки старше отсрочки удаляет как обычный orphan. Строки `block_media` удаляются каскадом вместе с исполнением и разговором; оставшиеся blob затем убирает сборщик.

## Снимки, история и восстановление диапазона

| Метод и путь | Результат |
| --- | --- |
| `GET /api/v1/snapshot/conversations` | `{scope:"conversations",cursor,data:{items,truncated,counts,groups,limits}}` |
| `GET /api/v1/snapshot/machines` | `{scope:"machines",cursor,data:{items}}` с доступностью, каталогами, ОС, метриками и предупреждениями |
| `GET /api/v1/conversations/{id}/snapshot` | `{scope:"conversation:<uuid>",cursor,data:{detail,queue,history,scheduled,open_requests}}`; параметр `history_limit` (по умолчанию 100); `open_requests` - незавершённые запросы (`open`, `decision_saved`, `decision_transmitting`) по времени создания |
| `GET /api/v1/conversations/{id}/history` | `{items,next_before,next_after}`; параметры `before` и `after` (взаимоисключающие), `limit` (по умолчанию 100) |
| `GET /api/v1/conversations/{id}/blocks/{block_id}` | `{content,offset,next_offset,total_bytes,state,rev,checksum}`; параметры `offset` (по умолчанию 0), `limit` (до 262144) |
| `GET /api/v1/conversations/{id}/runs/{run_id}` | Контекст, длительность, токены по моделям и агентам, оценка лимита, части ответа |
| `GET /api/v1/conversations/{id}/delegation` | Вызовы субагентов: родитель, имя и модель первого дочернего блока, задача, состояние и время |
| `GET /api/v1/events` | `{events,head}` либо `{resync_required:{reason,oldest_available_seq}}`, оба с HTTP 200; параметры `scope`, `after`, `limit` (по умолчанию 256) |

Все чтения ограничены владельцем. Отсутствующий и чужой разговор, блок или ход возвращают одинаковый `404 not_found`. Параметры проверяются без повторения текста запроса в ошибке. Курсор событий `after` обязателен, неотрицателен и задаётся десятичной строкой. Область разрешается по владельцу из `conversations`, `machines` или `conversation:<uuid>`; внутренние ключи не принимаются.

Каждый снимок открывает одну транзакцию `REPEATABLE READ READ ONLY`. Первое чтение получает границу `B` из `scope_counters`, следующие читают состояние. `events::append` держит блокировку строки счётчика до фиксации: видимый `B` означает, что все получившие номера до `B` транзакции уже видимы. Более поздние изменения читаются из журнала строго после `B`. Откат возвращает номер. Глобальная последовательность PostgreSQL этого свойства не имеет: писатель с номером 1 может зафиксироваться после писателя с номером 2. Отрицательный контроль в `snapshot_boundary` воспроизводит потерю 1 и проверяет блокировку второго писателя при том же чередовании с `events::append`.

Снимок списка включает архивные разговоры и не более 500 элементов; при превышении `truncated=true`, полные счётчики сохраняются. `machine` элемента списка содержит только `{id,name}`. Волатильные поля читаются из `conversation_derived` через `LEFT JOIN` и изменяются только по `derived_changed`; строки без проекции остаются доступны. Список не содержит доступность машин. Лимиты с `used_pct` находятся только в снимке списка, а не в снимке разговора. Снимок машин использует тот же загрузчик, что `GET /api/v1/machines`, и пороги предупреждений из конфигурации; `heartbeat_age_ms=null` в снимке, поскольку живой возраст не является состоянием PostgreSQL.

`queue.queued` содержит вводы без `feed_seq` в состояниях `queued`, `dispatched`, `on_machine`, по порядку очереди. `pause_reasons`, хранимые удержания и `run_slot` читаются из `queue_state`; представление добавляет вычисляемые `machine_not_reconciled` и `capability_missing`. HTTP-вариант использует реестр текущих сеансов. При чтении без реестра живой сеанс не считается сверенным. Вычисляемое `attachments_pending {input_id, machine_reason?}` показывается для ввода в состояниях `dispatched` и `on_machine`, пока не все его вложения готовы на машине; причина берётся из `input_attachments.machine_reason`.

Лента всегда расположена по возрастанию номера области: ввод по `feed_seq`, блок по `created_seq`. Ввод без `feed_seq` находится в очереди и не разрывает текущий ответ. По умолчанию возвращаются последние 100 элементов, максимум 200. `before` выбирает предыдущее окно, `after` следующее, строго исключая сам ключ; указатель продолжения соответствующего направления выдаётся при наличии следующей страницы. Старые записи, перенесённые миграцией без исходного события, могут иметь отрицательный ключ истории. Миграция 0012 назначает таким вводам и блокам общие уникальные ключи внутри разговора: отдельные порядки миграции 0008 могли совпадать и терять элемент на границе страницы. Положительные номера событий сохраняются. Первичная выборка ключей использует `blocks_timeline` и `inputs_feed`, текст загружается только для выбранных элементов.

Тег элемента ленты `kind` равен `input` или `block`. Поле вида самого блока называется `block_kind` (`text`, `reasoning`, `tool_call`, `tool_result`, `warning`, `error`), чтобы не дублировать JSON-ключ `kind`. Ввод содержит `idempotency_key` владельца, `created_seq`, nullable `feed_seq`, `created_at`, `retry_of`, `content` и `attachments`. `content` сохраняет формат `{"type":"text","text":…}` и допускает следом `{"type":"attachment","attachment_id","name","mime","size","sha256","image"}`. Блок содержит `input_command_id`, `parent_block_id`, агента, модель, инструмент, summary, времена создания/начала/окончания, число строк, `rev`, `revision_gap`, `incomplete_reason` и `media` - картинки, на которые смотрел агент (раздел «Картинки, на которые смотрел агент»).

Потоковый текст собирается из `block_chunks` по номеру, остальные состояния используют `blocks.content`. В истории содержимое `tool_call`/`tool_result` обрезается до 4096 байт на границе символа, `content_truncated` указывает обрезку, `content_bytes` и `line_count` относятся к полному тексту. Отдельное чтение блока возвращает до 256 КиБ. `offset`, `next_offset` и `total_bytes` измеряются байтами UTF-8; отрицательное смещение, выход за конец или попадание внутрь символа дают `400 invalid_offset` (`retry=never`). Смещение ровно в конец даёт пустой текст и `next_offset=null`. Если запрошенный положительный `limit` меньше размера первого символа, возвращается этот один символ, чтобы пагинация продвигалась; верхняя граница 256 КиБ сохраняется.

`run_id` равен `input_command_id`, а `input_id` указывает сам ввод. `usage` содержит входные, выходные и оба вида кэш-токенов по модели и агенту; `parts` содержит ID, агента, модель и вид блоков. `limit=null`, если нет сопоставимой пары недельных показаний. `limit.exact=false` при перекрытии ходов, неточном ходе, нулевой разнице или двух показаниях, кратных целому проценту: имеющаяся гранулярность не доказывает точность. `compactions=null` зарезервирован до этапа 2. У дерева делегирования `task` берётся из summary вызова; `streaming` отображается как `running`, `final` как `done`, прочие состояния как `incomplete`. Обычные результаты инструментов того же агента не создают узлы делегирования.

Ответ диапазона показывает `head` из той же транзакции, что и события. Курсор ниже сохранённого журнала получает `cursor_too_old`, курсор выше головы `cursor_ahead`; `oldest_available_seq` является десятичной строкой (при пустом журнале это `head+1`). Клиент загружает новый снимок и начинает хвост после его `cursor`. До 256 исходных событий проходят через единую `events::project_for_client`; неизвестный вид становится `hidden` с `payload:{}`, `conversation_id:null`, `execution_id:null`. Соседние заглушки одной области с последовательными номерами объединяются: начальные `id`, `correlation_id`, `committed_at_from`, `scope_seq_from` сохраняются, конечные `scope_seq`, `committed_at` берутся из последнего события. Через разрыв, видимое событие или другую область объединение не проходит.

Клиентские виды и формы событий опубликованы в [events.schema.json](../api/events.schema.json), JSON Schema 2020-12. Полный набор: `input_queued`, `input_state_changed`, `queue_state_changed`, `queue_paused`, `queue_resumed`, `execution_state_changed`, `phase_changed`, `run_finished`, `interrupt_state_changed`, `block_started`, `block_delta`, `block_final`, `block_incomplete`, `adapter_diagnostic`, `state_divergence`, `block_revision_gap`, `block_checksum_mismatch`, `block_conflicting_final`, `block_media_attached`, `conversation_created`, `conversation_updated`, `conversation_settings_changed`, `crunch_changed`, `crunch_held`, `crunch_woken`, `derived_changed`, `group_changed`, `scheduled_changed`, `limits_changed`, `limit_reached`, `attachment_state_changed`, `request_opened`, `request_closed`, `request_decided`, `request_transmitting`, `machine_availability_changed`, `machine_report_updated`, `machine_metrics`, `hidden`, `oversized`. `journal_closed`, `execution_fact_rejected`, `journal_record_mismatch`, `local_state_reset` и неизвестные будущие виды скрыты. Схема содержит также `ClientOp` и `ServerOp`: subscribe/unsubscribe/ping, subscribed/event/resync_required/lagged/unsubscribed/pong/error/close. Транспорт WSS реализуется в задаче 7b; формы зарезервированных payload уточняются вместе с их производителями в задачах 8, 9 и плане 1.3c.

### Метаданные фактов агента

Ingest ограничивает свободный текст до записи проекций и клиентских событий; длины считаются в символах Unicode:

| Поля фактов | Максимум символов |
| --- | --- |
| `BlockStarted.summary`, `BlockFinal.summary`, `PhaseChanged.summary` | 1024 |
| `BlockStarted.tool_name`, `PhaseChanged.tool_name` | 128 |
| `BlockStarted.agent`, `BlockStarted.model`, `RunFinished.usage[].agent/model` | 128 |
| `AdapterDiagnostic.kind`, `AdapterDiagnostic.event_type` | 128 |
| `BlockIncomplete.reason`, `JournalClosed.reason`, `ExecutionState.outcome` | 1024 |

Значение сверх предела заменяется первыми `предел - 1` символами и маркером `…`; маркер входит в предел. Усечение проходит на границе символа UTF-8 и не отклоняет запись, поэтому курсор журнала продвигается. Повтор и digest проверяются по исходной записи, до усечения. UUID, checksum, перечисления вида блока, фазы, состояния и провайдера сохраняют собственные правила проверки; это не свободный текст. `BlockDelta.append` и `BlockFinal.text` сохраняют отдельные байтовые ограничения содержимого.

Причины квитанций команд/вводов сохраняют предел 1024 байта UTF-8, включая маркер `…`; префикс заканчивается на границе символа. NUL удаляется.

## WSS: возобновляемые подписки

`GET /api/v1/ws` требует подпротокол `canterlot.v1`, разрешённый `Origin` и ту же проверку личности и секрета прокси, что HTTP. Нет нужного подпротокола: `400 invalid_request`. Upgrade удерживает слот `[http].max_connections` до закрытия сокета и освобождён от дедлайна тела HTTP.

Кадры JSON текстовые; входящее сообщение и отдельный кадр ограничены 4096 байтами. `ClientOp` и `ServerOp` определены в `$defs` файла [events.schema.json](../api/events.schema.json). `id` подписки содержит 1–128 символов, уникален среди действующих подписок соединения. Ошибка синтаксиса даёт `bad_message` с доступным корректным `id` либо пустой строкой. Лишние поля, числовой/null/отрицательный `after` запрещены; курсор помещается в неотрицательный `i64`. Неизвестный и чужой conversation ID дают одинаковый `forbidden`. Внутренний ключ `owner:<uuid>:…` даёт `bad_scope`.

| ClientOp | ServerOp |
| --- | --- |
| `{"op":"subscribe","id":"s1","scope":"conversations","after":"123"}` | `{"op":"subscribed","id":"s1","scope":"conversations","head":"130"}`, затем события после 123 |
| `{"op":"subscribe","id":"s2","scope":"machines"}` | Подписка от текущей головы, без старых событий |
| `{"op":"unsubscribe","id":"s1"}` | `{"op":"unsubscribed","id":"s1"}`; после подтверждения событий этой подписки нет |
| `{"op":"ping","t":1}` | `{"op":"pong","t":1}` |

Событие: `{"op":"event","id":"s1","event":{…}}`. В `event` находятся `v:1`, UUID `id`, wire alias `scope`, строки `scope_seq`, `scope_seq_from`, времена `committed_at`, `committed_at_from`, `kind`, nullable UUID `conversation_id`, `execution_id`, `payload` и UUID `correlation_id`. Клиент принимает диапазон только при `scope_seq_from == cursor + 1`, после применения устанавливает `cursor = scope_seq`. Без `after` ответ `subscribed.head` и начальный курсор pump берутся из одного чтения головы; commit между проверками не создаёт повторов или пропусков.

Внутреннее событие превращается в `hidden` с `payload:{}`, `conversation_id:null`, `execution_id:null`; соседние заглушки объединяются в непрерывный диапазон. Заглушка сначала выпускает накопленную дельту. Только соседние `block_delta` того же блока, области и исполнения с `base_rev == предыдущий rev` объединяются на срок до `delta_window` (максимум 25 мс) и до 64 КиБ полного сериализованного кадра `op:event`, включая envelope и экранирование JSON. Размер также ограничен бюджетом данных соединения: `queue_bytes - 1024`. Перед добавлением исходного события, которое превысит любой из этих пределов, выпускается накопленный диапазон. Исходные события неделимы; текст внутри символа UTF-8 не разрезается. Первые `base_rev`, `scope_seq_from`, `committed_at_from`, `id`, `correlation_id` сохраняются; `rev`, `scope_seq`, `committed_at` берутся из последнего события, `append` склеивается. Другой блок, разрыв seq/rev, терминальное или любое другое событие сначала выпускают накопленное.

Курсор вне журнала получает единственный `{"op":"resync_required","id":"s1","reason":"cursor_too_old","oldest_available_seq":"90"}` либо `reason:"cursor_ahead"`. Подписка удаляется, сервер её не повторяет. Клиент загружает снимок с границей `B` и заново подписывается с `after:B`, в том числе после обрыва соединения во время resync.

Pump читает журнал только при доступной ёмкости writer. Уведомления шины будят подходящие области, `*` будит все. Раннее уведомление до commit запускает повторные чтения через 5, 10, 20, 40, 80, затем каждые 100 мс до видимости seq или истечения `poll_interval`. Потерянные уведомления покрывает периодический опрос. У каждого соединения собственные чтения и очередь; catch-up ждёт освобождения ёмкости.

Очередь ограничена одновременно `queue_messages` (2–256, включая отправляемое сообщение) и `queue_bytes` (4096–1048576 байт сериализованного JSON). Один слот и 1024 байта зарезервированы для управления. `max_subscriptions` допускает 1–16; превышение даёт `error{code:"too_many_subscriptions"}`, соединение остаётся открытым. Повтор действующего `id` даёт `bad_message`; после unsubscribe его можно использовать снова. Минимум 4096 байт оставляет место для `oversized` даже при максимальном экранированном `id` подписки.

Если одиночное committed-событие не помещается в бюджет данных, WSS передаёт `kind:"oversized"` с исходными `scope_seq`, `scope_seq_from` (одинаковыми), ID, временами, областью и ссылками на разговор/исполнение. Payload имеет вид `{"kind":"block_final","href":"/api/v1/events?scope=conversation:<uuid>&after=41&limit=1"}`: `kind` обозначает исходный клиентский вид, `after` равен `scope_seq - 1`. Такой placeholder не объединяется с соседями и не закрывает соединение. Клиент получает полное исходное событие по `href` через существующий аутентифицированный `GET /api/v1/events`, применяет его и сохраняет курсор placeholder; последующие события остаются непрерывными. HTTP не заменяет большие события заглушками. Если retention уже удалил событие, ответ `resync_required` требует нового снимка и возобновления после его границы.

`lagged` возникает только при заполненной очереди без успешной записи в течение `stall_timeout`. Неотправленный хвост удаляется, незавершённая запись заканчивается, затем writer отправляет по одному `{"op":"lagged","id":"s1","cursor":"1500"}` для каждой действующей подписки и закрывает соединение кодом 1008. Курсор учитывает только успешно записанные кадры. Заключительные кадры отправляются последовательно через резерв управления; на их отправку даётся до 10 с, затем сокет освобождается. Медленное чтение с продолжающейся записью не считается зависанием.

По истечении `max_connection_age` сервер отправляет `{"op":"close","reason":"reauth"}`, затем close 4401. Клиент переподключается через прокси для повторной проверки личности. Shutdown хаба закрывает сокеты кодом 1001 и дожидается завершения pump/writer; они не создают отделённых задач.

## Покрытие экранов пилота

Таблица связывает экраны и поведение [pilot.md](pilot.md) §3–§11 с маршрутами и событиями. PWA (план 1.3b) отображает только то, что перечислено здесь; строки с пометкой «резерв» задают контракт без реализации в 1.3a. Одно соединение WSS несёт все области; каждая область может нести `hidden`. Пути записаны полностью, параметры запроса указаны в скобках.

| № | Экран или поведение | Маршруты | События | Задачи |
| --- | --- | --- | --- | --- |
| 1 | Список сессий: цвет, название, агент и модель, машина, активность с доказательством, непрочитанное (§3) | `GET /api/v1/snapshot/conversations`, `GET /api/v1/conversations` (`filter`, `q`, `cursor`, `limit`), `GET /api/v1/conversations/{id}`, `POST /api/v1/conversations/{id}/read` | `conversation_created`, `conversation_updated`, `derived_changed` | 5, 6a, 7a |
| 2 | Вкладки «Все», «Закреплённые» и группы со счётчиками; управление группами; сессия в нескольких группах (§3) | `GET /api/v1/groups`, `POST /api/v1/groups`, `PATCH /api/v1/groups/{id}`, `DELETE /api/v1/groups/{id}`, `PUT /api/v1/conversations/{id}/groups`; счётчики в снимке списка | `group_changed`, `conversation_updated` | 6a, 7a |
| 3 | Закрепление, цвет, название, архив, синхронно между устройствами (§3) | `PATCH /api/v1/conversations/{id}` | `conversation_updated` | 6a |
| 4 | Поиск по названиям (§3, ТЗ §12.1) | `GET /api/v1/conversations` (`q`) | – | 6a |
| 5 | Строка лимитов и история расхода по дням, сессиям, моделям и агентам (§3, §7) | `GET /api/v1/limits`, `GET /api/v1/limits/history` (`provider`, `from`, `to`, `tz`); лимиты в снимке списка | `limits_changed` | 3, 6a, 7a, 8 |
| 6 | Создание сессии: машина, каталог под разрешёнными корнями и недавние папки, агент, модель, усилие, профиль инструкций, режим разрешений, «кранч» и текст продолжения (§4) | `GET /api/v1/machines`, `GET /api/v1/machines/{id}/workdirs/recent`, `POST /api/v1/conversations`, `POST /api/v1/conversations/{id}/executions` | `conversation_created`, `execution_state_changed` | 2, 3, 6a, 6b |
| 7 | Лента: пузыри с отметками «сохранено» и «принято», карточки ответа, блоки инструментов с картинками, на которые смотрел агент, потоковый текст, полный вывод по запросу (§5.1, §5.7) | `GET /api/v1/conversations/{id}/snapshot`, `GET /api/v1/conversations/{id}/history` (`before`, `after`, `limit`), `GET /api/v1/conversations/{id}/blocks/{block_id}` (`offset`, `limit`), `GET /api/v1/media/{id}`, `GET /api/v1/events` (диапазон разрыва) | `input_queued`, `input_state_changed`, `block_started`, `block_delta`, `block_final`, `block_incomplete`, `block_media_attached`, `run_finished`, `phase_changed`, `hidden` | 3, 7a, 7b, 10d |
| 8 | Строки и ленты субагентов, дерево делегирования на десктопе (§6) | поля блока `agent`, `model`, `parent_block_id`; `GET /api/v1/conversations/{id}/delegation` | `block_started` | 2, 3, 7a |
| 9 | Поле ввода: отправка, вложение, прерывание, «отправить позже» (§5.2) | `POST /api/v1/conversations/{id}/inputs`, `POST /api/v1/conversations/{id}/interrupt`, вложения (строка 16), `POST /api/v1/conversations/{id}/scheduled` | `input_queued`, `queue_state_changed`, `queue_paused`, `queue_resumed`, `interrupt_state_changed` | 5, 6b, 9 |
| 10 | Очередь и отложенные сообщения под лентой; отмена ввода в очереди (§5.1, §5.3) | очередь и отложенные в снимке разговора; `GET /api/v1/conversations/{id}/scheduled`, `PATCH /api/v1/scheduled/{id}`, `DELETE /api/v1/scheduled/{id}`, `POST /api/v1/inputs/{id}/cancel` | `scheduled_changed`, `input_state_changed` | 5, 6b, 7a |
| 11 | Смена модели, режима разрешений и усилия посреди сессии; ожидающее значение в шапке (§5.4) | `PATCH /api/v1/conversations/{id}/settings`; `settings{effective,pending}` в карточке | `conversation_settings_changed` | 2, 6b |
| 12 | Карточки вопросов и разрешений (§5.5) | `POST /api/v1/conversations/{id}/requests/{request_id}/decision`: 200, проверка контракта, единственный победитель | `request_opened`, `request_closed`, `request_decided`, `request_transmitting` | HTTP реализован; события и UI в остальных задачах плана 1.3c |
| 13 | «Инфо» об ответе и экспорт в markdown или картинку (§5.6) | `GET /api/v1/conversations/{id}/runs/{run_id}` (расход по моделям и агентам, контекст, длительность, оценка лимита), содержимое блоков | – | 3, 7a |
| 14 | «Кранч»: переключатель, текст продолжения, удержания `quota_exhausted` и `weekly_reserve`, «Продолжить, тратя резерв» (§8) | `PUT /api/v1/conversations/{id}/crunch`, `POST /api/v1/conversations/{id}/spend-reserve`; удержания в карточке, элементе списка и снимке | `crunch_changed`, `crunch_held`, `crunch_woken`, `limit_reached`, `queue_state_changed` | 6a, 8 |
| 15 | Состояние активности с доказательствами в списке и шапке (§9) | в элементе списка, карточке и снимках | `derived_changed` | 5 |
| 16 | Вложения: загрузка с возобновлением частями по 8 МиБ, прогресс, отметки готовности, скачивание; «Поделиться» из Android через тот же API (§10) | `POST /api/v1/attachments`, `HEAD /api/v1/attachments/{id}/content`, `PATCH /api/v1/attachments/{id}/content`, `POST /api/v1/attachments/{id}/complete`, `DELETE /api/v1/attachments/{id}`, `GET /api/v1/attachments/{id}`, `GET /api/v1/attachments/{id}/content` | `attachment_state_changed` | 9, 10a, 10b |
| 17 | Экран машины: связь, возраст heartbeat, ОС, CPU, память, диск с предупреждением 85 %, нагрузка, сессии (§11) | `GET /api/v1/machines`, `GET /api/v1/machines/{id}`, `GET /api/v1/snapshot/machines` | `machine_availability_changed`, `machine_report_updated`, `machine_metrics` | 2, 3, 6a, 7a |
| 18 | Остановка, принудительная остановка, продолжение разговора, продолжение очереди (§1 п. 4) | `POST /api/v1/conversations/{id}/stop`, `POST /api/v1/conversations/{id}/resume`, `POST /api/v1/conversations/{id}/queue/continue`, `POST /api/v1/conversations/{id}/executions` | `queue_state_changed`, `queue_paused`, `queue_resumed`, `execution_state_changed` | 6b |
| 19 | Диагностика активного разговора (ТЗ §12.1) | снимок и полный поток событий разговора, включая диагностические виды | `adapter_diagnostic`, `state_divergence`, `block_revision_gap`, `block_checksum_mismatch`, `block_conflicting_final` | 7a, 7b |
| 20 | Синхронизация и служебная инфраструктура | `GET /api/v1/me`, `GET /api/v1/config`, `GET /api/v1/openapi.json`, `GET /api/v1/events`, `GET /api/v1/ws`, `GET /healthz`, `GET /readyz` | `hidden`, `oversized`, `resync_required`, `lagged`, `close` | 4, 7a, 7b |
| 21 | Основания межсессионных инструментов (§12): вычисляемое состояние с доказательствами и контракт подписок | активность (строка 15) и области WSS (строка 20) | `derived_changed` | 5, 7b |
| 22 | Лимиты, видимые модели (§7): только путь данных; инструмент MCP и пометка 80/90/95 % в планах 2a/2b | – (`ControlDown.limits_snapshot` для сеансов `pilot_v1`) | `limits_changed` | 1, 2, 3 |

`resync_required`, `lagged` и `close` - служебные кадры WSS (`ServerOp`), а не виды событий. `queue_resumed` публикуется при продолжении очереди без оставшихся пауз, вместе с `queue_state_changed`; полезная нагрузка совпадает с `queue_paused` и содержит пустой `pause_reasons`.

### Заморозка контракта

Контракт для плана 1.3b заморожен тестами хаба `contract_coverage`:

- `coverage_table_matches_openapi_and_event_schema`: каждый маршрут этой таблицы есть в `openapi.json` и наоборот; каждый вид события таблицы покрытия и таблицы видов есть в `events.schema.json` (и в `CLIENT_KINDS`) и наоборот; каждый `ErrorCode` перечисления и `components.schemas.ErrorCode` есть в таблице ошибок; закоммиченный `openapi.json` совпадает с генерацией.
- `every_mutating_endpoint_records_its_key_once_across_the_whole_api`: каждая изменяющая операция `openapi.json`, кроме `PATCH /api/v1/attachments/{id}/content` и `POST /api/v1/conversations/{id}/read`, после первого вызова и повтора оставляет одну запись ключа, повтор возвращает тот же ответ, другой запрос с тем же ключом даёт `409 idempotency_conflict`. Маршрут без фикстуры проваливает тест. Маршрут `…/decision` входит в общую проверку идемпотентности.

Сквозные сценарии пилота через HTTP, WSS, настоящий хаб, управляющую службу воркера, процессы сопровождения и синтетический CLI: `crates/canterlot-worker/tests/pilot_e2e.rs`. Каждый сценарий сверяет поток WSS с журналом: только клиентские виды и `hidden` без payload, непрерывные номера каждой области (в том числе после переподключения с курсора), каждое событие базы покрыто ровно одним кадром.

## Восстановление фоновых служб и обслуживание

Планировщик, проекторы активности и доступности, сборщик вложений и media держат отдельные соединения с advisory lock. При потере соединения или проверке дольше 2 с работа приостанавливается (`singleton_lost`), соединение закрывается; повторные подключения идут с задержкой 250 мс до 30 с. После повторного захвата работа возобновляется; задержка сбрасывается только после 60 с непрерывной работы с блокировкой. Ранний выход работы отмечается `singleton_work_ended` и сохраняет нарастающую задержку. Второй хаб остаётся пассивным, пока блокировка занята. Сброс всей доступности в `unavailable` выполняется только при первом захвате в процессе; при повторных захватах стартовая проекция использует живой Registry, в том числе после ошибки первого сброса. Все циклы завершаются по остановке `serve`.

`GET /readyz` проверяет базу, установку и фоновые службы этого процесса. Служба без доступного соединения, а также неожиданно завершившаяся задача даёт `503 {"status":"not_ready","reason":"service","services":["scheduler"]}` (имена: `scheduler`, `activity`, `machine_status`, `attachment-gc`). Подтверждённо пассивная служба готова.

Ошибки заданий изолированы. Невалидная полезная нагрузка, превышение границ и постоянные отказы завершают задание как `failed` с кодом в `result.error`. Ошибки считаются в отдельном `scheduled_jobs.failures`; опросы `wake` и `limit_check` увеличивают только `attempts`. Для `message` и `limit_rule` ошибки расходуют `max_attempts`, откладывают задание с нарастающей задержкой; `queue_full` использует `retry_queue_full`. Временные ошибки `wake` и `limit_check` повторяются неограниченно с задержкой от 1 до 60 с; завершить их может только постоянная причина, например удалённый разговор или невалидная полезная нагрузка. Позднейшие независимые задания обрабатываются в том же тике. Проверки границ при создании и изменении включают ссылки вложений и повторяются при отправке.

Обслуживание выполняется ежечасно: события хранятся 7 дней, снимки лимитов по `history_days`, задания `fired`, `cancelled`, `failed` 30 дней, ключи идемпотентности 7 дней. Ошибка одного удаления не отменяет остальные. Для отправки ввода ключ сохраняется дополнительно в `inputs.client_idempotency_key` с уникальностью по владельцу; разговор входит в хеш запроса. Вместе с `submission_hash` он переживает очистку общей таблицы и позволяет вернуть исходный ввод с исходным ответом независимо от его текущего состояния. Изменение содержимого, порядка вложений, `retry_of` или разговора даёт конфликт. Миграция заполняет ключи старых вводов (при исторических дублях сохраняет первый), а удалённые хеши восстанавливаются из сохранённых вводов и ссылок вложений. Для прочих операций после удаления ключа повтор считается новой операцией. Полезная нагрузка `limits_changed` всегда содержит nullable `weekly_reserve_resets_at`, включая вход, выход и переход окна продолжающегося резерва.

Новые blob распределяются по последним двум hex-цифрам UUID. Старое размещение по первым двум цифрам читается и очищается без переноса. Orphan-обход проверяет каталог пакетами по 128 файлов под файловыми блокировками; lock-файлы сохраняются ради единого inode между процессами.
