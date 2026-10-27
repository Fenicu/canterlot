# Контракт API пилота

Документ фиксирует словарь этапа 1.3a. HTTP-маршруты, схемы OpenAPI, полезная нагрузка событий и таблица покрытия дополняются в задачах, которые реализуют соответствующее поведение. Требования пилота: [pilot.md](pilot.md); транспорт: [streams.md](streams.md).

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

В кадрах WSS, снимках и `GET /events` используются только внешние имена. Области списков разрешаются хабом по аутентифицированному владельцу.

## Виды событий

Таблица перечисляет новые имена этого плана. Существующие имена событий сохраняются. Полный список клиентских видов фиксируется в `events::CLIENT_KINDS` в задаче 7a.

| Область | `kind` | Назначение |
| --- | --- | --- |
| `conversation:{id}` | `derived_changed` | Вычисленное состояние разговора |
| `conversation:{id}` | `conversation_updated` | Атрибуты разговора |
| `conversation:{id}` | `conversation_settings_changed` | Настройки следующего хода и их применение |
| `conversation:{id}` | `crunch_changed` | Настройки «кранча» |
| `conversation:{id}` | `crunch_held` | Удержание по лимиту |
| `conversation:{id}` | `crunch_woken` | Подтверждённое пробуждение |
| `conversation:{id}` | `limit_reached` | Факт исчерпания окна |
| `conversation:{id}` | `scheduled_changed` | Отложенные сообщения |
| `conversation:{id}` | `attachment_state_changed` | Состояние вложения |
| `conversation:{id}` | `request_opened` | Зарезервировано для плана 1.3c |
| `conversation:{id}` | `request_closed` | Зарезервировано для плана 1.3c |
| `conversation:{id}` | `request_decided` | Зарезервировано для плана 1.3c |
| `owner:{id}:conversations` | `conversation_created` | Создание разговора |
| `owner:{id}:conversations` | `conversation_updated` | Атрибуты элемента списка |
| `owner:{id}:conversations` | `derived_changed` | Вычисленные поля элемента списка |
| `owner:{id}:conversations` | `group_changed` | Группы владельца |
| `owner:{id}:conversations` | `limits_changed` | Лимиты провайдера |
| `owner:{id}:machines` | `machine_availability_changed` | Доступность машины |
| `owner:{id}:machines` | `machine_report_updated` | Каталог и сведения машины |
| `owner:{id}:machines` | `machine_metrics` | Последний снимок метрик |
| Любая | `hidden` | Заполнитель без полезной нагрузки для внутреннего события |

Событие вне `CLIENT_KINDS` передаётся в WSS и `GET /events` как `hidden` с настоящим `scope_seq`, без внутреннего payload. Поэтому курсор каждой области остаётся непрерывным. Соседние дельты одного блока могут объединяться в течение 25 мс: `scope_seq_from` и `committed_at_from` обозначают начало диапазона, `scope_seq` и `committed_at` его конец. ID и correlation ID берутся из первого события. Объединение не перескакивает через чужие события. Соседние `hidden` объединяются по тому же правилу диапазона.

## Удержания очереди

`HoldReason` хранит причины в PG. `HoldView` содержит все эти варианты и вычисляемые `machine_not_reconciled`, `capability_missing { input_id }`. Вычисляемые варианты не десериализуются как `HoldReason`. У `attachments_pending` необязательное поле Rust `reason` передаётся как `machine_reason` из `input_attachments.machine_reason`, поскольку JSON-поле `reason` служит тегом варианта.

## Вход и служебные правила

Подробности и обоснование: [ADR-0004](../adr/0004-browser-auth-forward-auth.md).

- Личность задаёт прокси перед хабом. Заголовок (`[http].user_header`, по умолчанию `Remote-User`) принимается, только если TCP-адрес собеседника входит в `[http].trusted_proxies`, а при заданном `proxy_secret_file` запрос несёт `X-Canterlot-Proxy` с этим секретом. Значение заголовка должно быть равно `[http].owner_login`. `X-Forwarded-*` не читается.
- Недоверенный адрес, чужой секрет, отсутствующий или повторённый заголовок личности дают одинаковый `401 unauthenticated`; верный прокси с чужим логином получает `403 forbidden_user`.
- Изменяющий запрос (любой метод, кроме `GET` и `HEAD`) несёт `Origin` из `[http].allowed_origins` и `X-Canterlot-Client: 1`; запрос с `Upgrade` (WSS) несёт разрешённый `Origin`. Нарушение даёт `403 origin_rejected` до проверки личности и до обработчика. `OPTIONS` всегда `405 method_not_allowed`; заголовков `Access-Control-*` хаб не отправляет.
- Каждый изменяющий запрос несёт `Idempotency-Key` по шаблону `^[A-Za-z0-9_-]{1,256}$`. Ключи с `:` (пространства `sched:` и `wake:`) принадлежат хабу и отклоняются с `400 invalid_request`.
- Тело JSON не больше `[http].max_json_body_bytes` (512 КиБ); превышение даёт `413 resource_limit`.
- Каждый ответ `/api/` несёт `Cache-Control: no-store` и `X-Server-Time-Ms`; каждый ответ несёт `X-Correlation-Id`. `GET /healthz` и `GET /readyz` не требуют личности и сообщают только `ok` либо `not_ready` с причиной `database`.

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
| `resource_limit` | 413 | `browser` | `never` | Превышен допустимый размер данных |
| `not_implemented` | 501 | `hub_db` | `never` | Функция пока не реализована |
| `too_many_requests` | 429 | `hub_db` | `safe_same_key` | Слишком много запросов, повторите позже |
| `db_unavailable` | 503 | `hub_db` | `safe_same_key` | База данных временно недоступна |
| `internal` | 500 | `hub_db` | `safe_same_key` | Внутренняя ошибка хаба |
| `queue_full` | 409 | `delivery` | `after_condition` | Очередь разговора заполнена |
| `conversation_closed` | 409 | `delivery` | `never` | Разговор закрыт |
| `execution_already_live` | 409 | `delivery` | `after_condition` | Исполнение уже запущено |
| `invalid_state` | 409 | `delivery` | `never` | Действие недопустимо в текущем состоянии |

Строки таблицы совпадают с перечислением `ErrorCode` хаба и со схемой `components.schemas.ErrorCode` в `docs/api/openapi.json`; тест `error_code_table_in_api_md_matches_the_enum_and_openapi` сверяет код, статус, `scope`, `retry` и сообщение. Код, который повторяется тем же ключом (`safe_same_key`), бывает только у 429, 500 и 503: условие проходит само. Отказ с другим статусом оставляет ключ свободным и сообщает в `retry`, что нужно для повтора. Каждая новая задача добавляет свои коды в перечисление и в эту таблицу в одном коммите.

Разбивка по областям: `auth` (вход и источник запроса), `browser` (форма запроса, ключ идемпотентности, размеры), `hub_db` (хаб и PostgreSQL), `delivery` (очередь, разговор, исполнение). Чужой идентификатор неотличим от отсутствующего: оба дают `not_found`.

## Маршруты и покрытие

Маршруты ядра HTTP (задача 4); полная схема в [openapi.json](../api/openapi.json), перегенерация `scripts/gen-openapi.sh`:

| Метод и путь | Назначение |
| --- | --- |
| `GET /healthz` | Живость процесса |
| `GET /readyz` | База отвечает и установка совпадает с манифестом |
| `GET /api/v1/me` | `{owner_id, login}` владельца |
| `GET /api/v1/config` | Лимиты, пороги, путь и протокол WSS, `logout_url` |
| `GET /api/v1/openapi.json` | Документ OpenAPI |

Маршруты, схемы payload и соответствие экранам пилота фиксируются последующими задачами 4–10c. Наличие имени события в этой таблице само по себе не означает, что его производитель уже реализован.
