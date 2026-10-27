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

## Errors

Ошибки имеют форму `{"error":{"code","message","scope","retry","correlation_id"}}`; необязательный `details` имеет закрытый словарь без пользовательского содержимого. `code` записывается в snake_case, `message` является короткой русской фразой; `scope` и `retry` берутся из [словаря ошибок](README.md#ошибки-63). Клиент определяет повтор по `retry`. Хаб принимает UUID из `X-Correlation-Id` либо создаёт новый и всегда возвращает его.

| `code` | HTTP | `scope` | `retry` | `message` |
| --- | --- | --- | --- | --- |

Таблица заполняется задачей 4 и расширяется вместе с новыми обработчиками. Это единый реестр кодов для `ApiError` и `components.schemas.ErrorCode` в OpenAPI.

## Маршруты и покрытие

Маршруты, схемы payload и соответствие экранам пилота фиксируются последующими задачами 4–10c. Наличие имени события в этой таблице само по себе не означает, что его производитель уже реализован.
