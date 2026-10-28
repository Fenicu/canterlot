# canterlot

Self-hosted хаб для coding agents: Rust-хаб, Rust-воркеры на рабочих машинах, PWA на SvelteKit, PostgreSQL, gRPC между хабом и воркерами. Первая версия поддерживает Claude Code и Codex.

## Состояние

- Этап 0 завершён: оба CLI проходят обязательный набор возможностей, сводка в `docs/stage0/README.md`.
- Этап 1.1 (фундамент) завершён: транспорт gRPC выбран (ADR-0001), локальный журнал (ADR-0002), контракт хаб–воркер v1, ядро доставки хаба на PostgreSQL. Этап 1.2a добавляет gRPC-сеансы, доставку команд и квитанций. Этап 1.3a добавляет HTTP API и WSS хаба для пилота: вход по заголовку прокси, CSRF, ошибки, идемпотентные изменения, сессии и группы, очередь и отложенные сообщения, лимиты и «кранч», активность, машины и вложения. Контракт для PWA (план 1.3b) заморожен: `docs/design/api.md`, `docs/api/openapi.json`, `docs/api/events.schema.json`; сквозные сценарии пилота проверяют его на настоящих хабе и воркере.

## Структура

| Путь | Назначение |
| --- | --- |
| `crates/canterlot-journal/` | Локальный журнал команд и событий воркера |
| `crates/canterlot-proto/` | Типы protobuf, контракты gRPC и локального IPC управляющей службы с сопровождением |
| `crates/canterlot-worker/` | Воркер: TLS-сеанс, журналы, сопровождение исполнений, адаптеры и `machine/{metrics,limits}` |
| `crates/canterlot-hub/` | Хаб: PostgreSQL, gRPC-сервер, диспетчер, outbox, факты агента и данные машин |
| `crates/canterlot-hub/src/{runs,limits}.rs` | Ходы, расход по моделям и агентам, снимки лимитов и история |
| `crates/canterlot-hub/src/{crunch.rs,http/crunch.rs}` | Правила лимитов: удержания «кранча», недельный резерв, пробуждение после сброса, опрос лимитов, `spend-reserve` |
| `crates/canterlot-hub/src/{machine_info,singleton}.rs` | Каталог и метрики машин, проекция доступности и блокировки фоновых служб |
| `crates/canterlot-hub/src/http/` | HTTP хаба на axum: вход по заголовку прокси, CSRF и Origin, ошибки, `healthz`/`readyz`, `me`, `config`, OpenAPI |
| `crates/canterlot-hub/src/{background,dberr}.rs` | Единое место запуска фоновых служб; безопасное описание ошибок базы данных |
| `crates/canterlot-hub/src/{activity,bus,scheduler}.rs` | Проекция активности и непрочитанного, шина уведомлений и транзакционный планировщик |
| `crates/canterlot-hub/src/{conversations,groups}.rs` | Создание сессий, атрибуты, группы, список и прочитанность |
| `crates/canterlot-hub/src/http/{conversations,groups,machines,limits}.rs` | HTTP сессий и групп, чтение машин, лимитов и недавних каталогов |
| `crates/canterlot-hub/build.rs` | Пересборка встроенных миграций при добавлении SQL-файла |
| `crates/canterlot-hub/src/{snapshot,history}.rs` | Транзакционные снимки с границей журнала, история по scope_seq, блоки, ходы и делегирование |
| `crates/canterlot-hub/src/{subscriptions.rs,http/ws.rs}` | WSS canterlot.v1: подписки по курсору, pull-based pump, слияние дельт и отключение зависшего клиента |
| `crates/canterlot-hub/src/http/{snapshot,history}.rs` | GET снимков, истории и диапазона событий с заглушками внутренних видов |
| `docs/api/events.schema.json` | JSON Schema 2020-12 событий браузера и кадров canterlot.v1 |
| `crates/canterlot-hub/src/http/scheduled.rs` | Создание, список, изменение и отмена отложенных сообщений |
| `crates/canterlot-hub/src/{attachments/,http/attachments.rs}` | Вложения хаба: загрузка частями в staging, проверенная надёжная публикация, метаданные, привязка к вводу, сборщик и безопасная выдача |
| `crates/canterlot-hub/src/grpc/attachments.rs` | Выдача вложений машине через `FetchAttachment`, возобновление по смещению и ограничение потоков |
| `crates/canterlot-worker/src/attachments.rs` | Загрузка вложений с возобновлением, проверка SHA-256 и надёжная публикация в каталоге исполнения |
| `crates/canterlot-worker/tests/pilot_e2e.rs` | Сквозные сценарии пилота через HTTP и WSS: настоящий хаб, воркер, сопровождение и синтетический CLI |
| `crates/canterlot-hub/tests/contract_coverage.rs` | Заморозка контракта: таблицы `api.md` против OpenAPI и схемы событий, ключ идемпотентности у каждого изменяющего маршрута |
| `docs/api/openapi.json` | Машиночитаемый контракт API, генерируется `scripts/gen-openapi.sh` |
| `scripts/{gen-openapi,check-proxy}.sh` | Перегенерация OpenAPI; приёмка заголовков Caddy и Authelia |
| `docs/adr/0004-browser-auth-forward-auth.md` | Вход браузера через прокси с forward-auth |
| `spikes/` | Прототипы этапа 0, не продуктовый код |
| `docs/stage0/` | Отчёты этапа 0: матрица возможностей, доказательства, ограничения |
| `crates/canterlot-proto/src/harness.rs` | Единая таблица харнессов и провайдеров |
| `docs/design/api.md` | Контракт API пилота: представление данных, ошибки, маршруты, виды событий, покрытие экранов |
| `docs/design/` | Автоматы состояний, правила идемпотентности и протоколы восстановления |
| `docs/adr/` | Решения по архитектуре и их обоснования |

## Тулчейн

- Rust 1.99.1 (закреплён в `rust-toolchain.toml`)
- protoc 36.2
- Node.js 24 LTS (сборка PWA)

## Разработка

Полная проверка — `scripts/check.sh`; e2e-тесты запускаются только с `--all-features`. Сквозные сценарии пилота: `cargo test -p canterlot-worker --all-features --test pilot_e2e`; тяжёлые e2e одного бинарника выполняются не больше трёх одновременно (`CANTERLOT_E2E_PARALLEL`), процессы сопровождения и синтетического CLI тест убивает при завершении. Стенд с настройками как у `serve` (лимиты, активность, планировщик, хранилище вложений) собирает `testkit::spawn_test_hub_with`.

Для тестов хаба нужен Docker. Из корня репозитория запустите PostgreSQL 18.6 и тесты:

```bash
scripts/test-db.sh up
source ~/.cargo/env
cargo test -p canterlot-hub
```

База доступна на `127.0.0.1:55440`. Тестовые пользователь, пароль и база: `canterlot`.
`DATABASE_URL` задан в `.cargo/config.toml`; переменная окружения позволяет использовать другую тестовую базу.
Каждый тест схемы создаёт отдельную базу через `sqlx::test`, поэтому пользователю PostgreSQL нужно право `CREATEDB`.
Функции подготовки данных доступны с feature `testkit` и автоматически подключаются при тестировании хаба.

```bash
scripts/test-db.sh psql
scripts/test-db.sh down
```

Данные контейнера хранятся в tmpfs и теряются при его остановке.

Для запуска команд хаба подготовьте TOML-конфиг и отдельный файл с URL PostgreSQL.
URL не передаётся аргументом CLI и не читается из `DATABASE_URL` при запуске бинарника:

```bash
umask 077
mkdir -p ~/.config/canterlot ~/.local/state/canterlot-control
read -rsp 'URL PostgreSQL: ' canterlot_db_url
printf '\n'
printf '%s\n' "$canterlot_db_url" > ~/.config/canterlot/database-url
unset canterlot_db_url
```

Сохраните `~/.config/canterlot/hub.toml`:

```toml
database_url_file = "database-url"
grpc_listen = "127.0.0.1:7443"
tls_cert = "tls/cert.pem"
tls_key = "tls/key.pem"
manifest = "../../.local/state/canterlot-control/installation.toml"

[timers]
handshake = "10s"
degraded_after = "15s"
unavailable_after = "30s"
command_stored_receipt = "5s"
dispatch_sweep = "5s"
http2_keepalive_interval = "30s"
http2_keepalive_timeout = "10s"

```

Хаб принимает факты `pilot_v1`: модель и агент блока, инструмент и краткое описание, расход хода по моделям и агентам, заполнение контекста, лимиты и отчёт машины. `runs` хранит интервалы в машинном времени, приведённом к часам хаба; пересекающиеся и опоздавшие более чем на минуту ходы исключаются из оценки доли недельного лимита. История расхода группируется по разговору, модели, агенту и дню выбранного часового пояса; временные выборки используют полуинтервал `[from, to)`. Прирост недельного лимита хода засчитывается один раз, в строке с наибольшим расходом токенов; отрицательный прирост (сброс окна внутри хода) в оценку не входит.

В `[limits]` уже работают `snapshot_min_interval` (снимок при изменении значения/сброса или по истечении интервала между наблюдениями) и хранение времени наблюдения. Старые наблюдения остаются историей и не изменяют текущее значение; событие `limits_changed` пишется только при изменении значения или времени сброса. Без образца машинных часов наблюдение сохраняется только в историю с диагностикой. Снимки отправляются после commit подходящим сеансам `pilot_v1`, включая первое заполнение после отчёта машины.

Правила лимитов (`crunch.rs`, [контракт](docs/design/api.md#лимиты-кранч-и-недельный-резерв)) работают внутри цикла планировщика и останавливаются вместе с ним. Упор в лимит без «кранча» ставит паузу `limit_reached` со временем сброса; с «кранчем» очередь удерживается до сброса (`reset_grace` после него, `unknown_reset_retry` при неизвестном времени), а пробуждение проверяется по новому показанию лимитов. Без подтверждения задание повторяется по `verify_backoff` и после commit просит одну доступную машину провайдера с `pilot_v1` опросить лимиты: не чаще `poll_min_interval`, после исчерпания шагов не чаще `poll_floor`. Недельное показание не ниже `weekly_reserve_pct` включает резерв: новые ходы провайдера удерживаются, идущий ход завершается. Выход из резерва требует нового окна или двух показаний ниже порога на `reserve_exit_hysteresis_permille`. После сброса продолжаются только сессии в «кранче», которые остановил лимит; текст продолжения берётся из сессии или `default_resume_text`. Резерв тратится только действием владельца `spend-reserve` для одной сессии до сброса недели.

`machine_info` заменяет каталог и профили атомарно, проверяет лимиты вложений и сохраняет последнюю допустимую метрику. Проектор доступности получает отдельное PostgreSQL-соединение с singleton-блокировкой, после её получения сбрасывает сохранённую доступность и обновляет её каждые 5 секунд и по изменениям Registry. При потере соединения или shutdown служба останавливается и освобождает блокировку; второй процесс остаётся пассивным, повторяет попытку раз в 2 секунды и принимает работу, когда первый отпустит блокировку.

Обновление: сначала хаб, затем воркер. Откат хаба ниже уже использованной воркером capability не поддерживается.

HTTP сессий поддерживает создание по каталогу машины, старт в одной транзакции, закрепление, цвет, архив, группы, поиск по заголовку с ICU, список с курсором и отметку прочитанного. Чтение машин включает каталоги и профили, ОС, метрики и предупреждения; история лимитов учитывает часовой пояс и расход по агентам. Поля и маршруты описаны в [контракте API](docs/design/api.md#сессии-группы-и-чтение-данных-машин). Новых таблиц конфигурации не требуется: текст кранча берётся из `[limits]`, предупреждения из `[machines]`, HTTP использует существующий `[http]`.

Таблицы пилота в `hub.toml` необязательны; показаны значения по умолчанию. Таблица `[http]` отсутствует по умолчанию: без неё хаб обслуживает только gRPC. При её наличии хаб слушает `listen` и требует непустые `trusted_proxies`, `owner_login`, `allowed_origins`; ниже раздел «HTTP: вход, CSRF и Caddy».

```toml
[http]
listen = "127.0.0.1:8080"
trusted_proxies = ["127.0.0.1/32", "::1/128"]
owner_login = "owner"
user_header = "Remote-User"
allowed_origins = ["https://chat.example"]
max_json_body_bytes = 524288
header_read_timeout = "10s" # Также предел простоя HTTP keep-alive.
body_read_timeout = "30s"
max_connections = 256
multi_process = false
# Рекомендуется: читаемый файл не короче 32 байт.
# proxy_secret_file = "proxy-secret"
# logout_url = "https://auth.example/logout"

[http.ws]
max_connection_age = "30m"
max_subscriptions = 16
queue_messages = 256
queue_bytes = 1048576
delta_window = "25ms"
poll_interval = "2s"
stall_timeout = "15s" # без прогресса записи при заполненной очереди

[limits]
weekly_reserve_pct = 95
snapshot_min_interval = "10m"
history_days = 90
default_resume_text = "Лимиты восстановлены, продолжай с того места, где остановился."
reset_grace = "20s"
verify_backoff = ["30s", "1m", "2m", "5m"]
poll_min_interval = "5m"
unknown_reset_retry = "30m"
reserve_exit_hysteresis_permille = 20
warn_pcts = [80, 90, 95]
stale_after = "35m"

[limits.poll_floor]
claude = "30m"
codex = "5m"

[activity]
stalled_generating = "5m"
stalled_tool = "30m"
tick = "15s"
debounce = "250ms"

[scheduler]
max_pending_per_conversation = 50
retry_queue_full = "60s"
max_attempts = 20
idle_poll = "1s"

[attachments]
blob_dir = "data/attachments"
max_concurrent_uploads = 4
gc_grace = "24h"
gc_interval = "1h"

[machines]
disk_warn_pct = 85
metrics_stale_after = "90s"
```

Origin задаётся как `scheme://host[:port]`, без пути, query и fragment; `logout_url` является абсолютным HTTPS URL. `blob_dir` и `proxy_secret_file` разрешаются от каталога конфигурации, staging и blobs размещаются на одной файловой системе вне рабочих репозиториев. `multi_process = true` включает `NOTIFY` и слушатель PostgreSQL с восстановлением соединения; при `false` шина остаётся внутри процесса.

`WorkerData.FetchAttachment` читает опубликованные blobs из того же `[attachments].blob_dir` чанками до 128 КиБ, начиная с запрошенного смещения. На машину допускается четыре потока; пятый получает `RESOURCE_EXHAUSTED`. Потоки не удерживают соединения PostgreSQL и завершаются с `UNAVAILABLE` при остановке хаба. Готовность файлов и причины ожидания поступают в `CJ` как `attachment_ready` / `attachment_failed`; обрыв передачи сам по себе не отклоняет ввод. Blobs связанных вложений хранятся до удаления сессии; само удаление сессий появится после пилота.

`[activity]` задаёт пороги подозрения на зависание и частоту проекции. Генерация без событий дольше пяти минут или инструмент без событий дольше получаса получают `stalled` только при доступной машине. Это состояние не прерывает исполнение. Проектор считает непрочитанные блоки и сводку очереди, публикует `derived_changed` при изменении и восстанавливает пропущенные уведомления периодическим обходом.

`[scheduler]` ограничивает число ожидающих сообщений в разговоре, повторы при заполненной очереди и время ожидания без уведомлений. Маршруты `/api/v1/conversations/{id}/scheduled` (GET, POST) и `/api/v1/scheduled/{id}` (PATCH, DELETE) позволяют планировать ввод от секунды до 366 дней вперёд. Изменение и отмена доступны до отправки. В момент отправки одна транзакция создаёт обычный ввод с ключом `sched:<id>` и завершает задание; диспетчер учитывает все паузы и удержания. Повторный запуск хаба не дублирует ввод. Планировщик удаляет события старше семи дней, снимки лимитов старше `[limits].history_days` и отправленные либо отменённые задания старше 30 дней. Планировщик и проектор держат отдельные advisory lock, проверяют их в цикле и завершаются вместе с `serve`.

`[attachments]` задаёт каталог вложений хаба, число одновременно принимаемых тел загрузки, отсрочку и период сборщика. Файл до 100 МиБ (до 10 на ввод) создаётся `POST /api/v1/attachments`, загружается частями до 8 МиБ через `PATCH /api/v1/attachments/{id}/content` с `Upload-Offset` (текущее смещение даёт `HEAD`) и завершается `POST …/complete`: длина и SHA-256 проверяются вне транзакций, затем `fsync` файла, `rename` из `staging/` в `blobs/` и `fsync` каталогов, и только потом PostgreSQL помечает вложение готовым. Сбой между публикацией и commit оставляет orphan, повтор `complete` завершает публикацию без копирования. Ввод и отложенное сообщение ссылаются на готовые вложения через `attachment_ids`; машина должна сообщить `pilot_v1` и совпадающие лимиты. Выдача `GET …/content` всегда скачивает файл: изображения из короткого списка с проверенной сигнатурой, остальное как `application/octet-stream`, с `nosniff` и `sandbox`. Сборщик держит advisory lock `canterlot:attachment-gc`, завершается вместе с `serve` и через `gc_grace` удаляет брошенные загрузки, вложения без ссылок и файлы без записи.

Относительные пути хаба разрешаются от каталога конфига. Таймеры хаба и воркера задаются строками humantime (`"10s"`, `"250ms"`);
таблицу `timers` и отдельные её поля можно опустить, тогда действуют показанные значения.
Секреты задаются только путями к файлам; неизвестные поля конфига отклоняются.
Каталог манифеста должен существовать и находиться вне восстанавливаемого набора PostgreSQL/blobs.
Для рабочей инсталляции этот файл должен сохраняться независимо от восстановления данных.

Инициализируйте хаб и добавьте машину:

```bash
source ~/.cargo/env
cargo run -p canterlot-hub -- init --config ~/.config/canterlot/hub.toml
cargo run -p canterlot-hub -- machine add --config ~/.config/canterlot/hub.toml --name workstation
```

`init` применяет миграции и создаёт владельца, запись инсталляции и внешний манифест.
Повторный вызов запрещён, если уже есть запись инсталляции или файл манифеста.
`machine add` выводит ровно четыре строки: `machine_id=<uuid>`, `installation_id=<uuid>`, `restore_generation=<uuid>`, `token=<base64url>`.
Перенесите `machine_id`, `installation_id` и `restore_generation` в конфиг воркера, а значение `token` сохраните в файл, указанный в `token_file`.
Токен показывается один раз; сохраните его в файл воркера с правами `0600` или строже.
Хаб сохраняет только SHA-256 токена, повторно получить токен нельзя.

На машине воркера сохраните `worker.toml` (UUID возьмите из вывода `machine add`):

```toml
hub_url = "https://hub.example:7443"
ca_cert = "/home/user/.config/canterlot/ca.pem"
token_file = "/home/user/.config/canterlot/token"
machine_id = "019a1b10-0000-7000-8000-000000000001"
installation_id = "019a1b10-0000-7000-8000-000000000002"
restore_generation = "019a1b10-0000-7000-8000-000000000003"
data_dir = "/home/user/.local/state/canterlot-worker"

[timers]
handshake = "10s"
heartbeat = "5s"
heartbeat_ack = "5s"
range_ack = "10s"
backoff_min = "250ms"
backoff_max = "30s"
backoff_reset_after = "30s"
http2_keepalive_interval = "30s"
http2_keepalive_timeout = "10s"

[execution]
allowed_workdir_roots = ["/home/user/projects"]
supervisor_exe = "/usr/local/bin/canterlot-worker"
spawn_timeout = "10s"

[execution.supervisor]
attach_wait = "30s"
ready_deadline = "30s"
transmit_deadline = "10s"
stop_grace = "5s"
stop_retry = "2s"

[machine]
metrics_interval = "30s"

[attachments]
fetch_parallelism = 2
fetch_retry = "2s"
fetch_attempts = 5
fetch_backoff_cap = "30s"

[[instruction_profiles]]
name = "executor"
title = "Исполнитель"
description = "Профиль без персоны"
harnesses = ["claude_code", "codex"]
```

Замените `/home/user` своим домашним каталогом; `~` внутри TOML не раскрывается.
Относительные пути воркера считаются от рабочего каталога процесса. Неизвестные поля,
включая опечатки в `[timers]`, отклоняются; числовые таймеры без единицы измерения запрещены.
`data_dir` создаётся с правами `0700`, файлы журналов и `hub.toml` — `0600`.

Таблицы `[machine]`, `[attachments]`, `[[instruction_profiles]]` необязательны и отклоняют неизвестные поля. `fetch_parallelism` равен 1 или 2 и ограничивает загрузки всей машины; число попыток и интервалы положительны. `fetch_attempts` считает только ошибки содержимого (checksum и преждевременный `last`), между ними выдерживается `fetch_retry`. Обрывы, `RESOURCE_EXHAUSTED` и отсутствие соединения не расходуют попытки: повтор начинается через 250 мс с удвоением до `fetch_backoff_cap`, ожидание восстановления хаба не ограничено этим интервалом. Имена профилей уникальны; `owner` зарезервировано для встроенного «Как у владельца».

Вложения воркера находятся только в `data_dir/executions/<execution_id>/attachments/`, вне рабочих каталогов. Временный файл `.tmp/<attachment_id>.part` возобновляется с его длины, в том числе после перезапуска управляющей службы. Публикация: проверка длины и SHA-256, `fsync` файла, `rename` в `<uuid>-<безопасный суффикс>`, `fsync` каталогов. Каталоги имеют права `0700`, файлы `0600`; агент получает абсолютный путь и метаданные через `Deliver.files`. Уже опубликованный файл проверяется повторно без загрузки.

Для каждого ввода actor запускает отдельную отменяемую задачу и передаёт ввод сопровождению только после готовности всех файлов. Отмена, отзыв и остановка исполнения удаляют его временные файлы сразу; завершение управляющей службы оставляет их для продолжения. Опубликованные копии удаляются после терминального состояния исполнения и полного подтверждения его SJ. `attachment_ready` / `attachment_failed` пишутся только для команды, полученной с согласованным `pilot_v1`; этот признак сохраняется в receipt и защищён временной durable-меткой до записи inbox. На один непрерывный сбой транспорта пишется одна причина `hub_unreachable`; окончательная ошибка файла заканчивается `command_rejected{reason:"attachment_unavailable"}` без передачи агенту.

После согласования `pilot_v1` воркер отправляет `MachineReport` на каждом подключении: каталог харнессов и моделей, канонические рабочие корни, профили (включая встроенный `owner`), версию, ОС и ограничения вложений. Метрики `[machine].metrics_interval` собираются из `/proc` и `statvfs` в блокирующем пуле: CPU, память, нагрузка, диски без повторов одного устройства, процессы исполнения и потомки CLI с проверкой времени старта. Заполненная управляющая очередь отбрасывает sample; heartbeat сохраняет приоритет. Незавершённый сбор дожидается завершения при остановке воркера.

С feature `synthetic-adapter` доступны `synthetic`, `synthetic-exit-on-interrupt` (провайдер `claude`) и `synthetic-codex` (`codex`). Каталог содержит `synthetic-large` (усилие `low/medium/high`) и `synthetic-small` (`low/medium`), режимы `owner/ask/bypass/read_only`. Начальные параметры передаются через `StartCli`; изменения settings проверяются до durable intent и поступают в CLI с очередным вводом. Сценарии JSON описаны в `src/adapter/synthetic_cli.rs`: модели, субагенты, usage, limit-факты, исчерпание контекста и чтение локального вложения по индексу. Текст и пути не попадают в диагностику.

`PollLimits` читает `data_dir/synthetic-limits/<provider>.json` с окнами `five_hour`/`weekly` и полями `used_permille`, `resets_at_ms`; без синтетической feature источник пуст до этапа 2. Последний `LimitsSnapshot` хранится в памяти (`LimitsCache`). Без `pilot_v1` новые факты не передаются, остальные журналы продолжают отправляться; диагностика `journal_withheld` указывает на несовместимость. Обновляйте сначала хаб, затем воркер; откат хаба ниже уже использованной возможности воркера не поддерживается.

Таблица `[execution]` необязательна и так же строгая. `allowed_workdir_roots` — каталоги,
внутри которых допустим рабочий каталог исполнения (сравнение по каноническому пути);
пустой список отклоняет каждый `start_execution` с причиной `workdir_not_allowed`.
`supervisor_exe` — бинарник процесса сопровождения (по умолчанию текущий исполняемый файл),
`spawn_timeout` — срок, за который запущенное сопровождение должно принять `Attach`.
`[execution.supervisor]` задаёт таймеры сопровождения. Каталоги исполнений лежат в
`data_dir/executions/<execution_id>` с правами `0700`; секрет IPC и `spec.toml` — `0600`.
Второй экземпляр службы на том же `data_dir` завершается с ошибкой блокировки журнала.

Хабу нужны PEM-сертификат и закрытый ключ в `tls_cert`/`tls_key`.
SAN сертификата должен совпадать с хостом в `hub_url` (в примере `hub.example`).
В `ca_cert` воркера укажите CA, подписавший сертификат хаба; проверка TLS обязательна.
Если воркер подключается с другой машины, задайте хабу доступный `grpc_listen`, например `0.0.0.0:7443`.

Запустите хаб и воркер в отдельных терминалах:

```bash
canterlot-hub serve --config ~/.config/canterlot/hub.toml
canterlot-worker run --config ~/.config/canterlot/worker.toml
```

При запуске из исходников используйте `cargo run -p canterlot-hub -- serve --config ...`
и `cargo run -p canterlot-worker -- run --config ...` соответственно.
`serve` сначала проверяет файл манифеста и отсутствие незавершённого восстановления,
применяет миграции под advisory lock sqlx, пишет версию схемы в диагностику,
сверяет манифест с БД и запускает TLS gRPC-сервер до SIGINT/SIGTERM.
Отсутствующий или несовпадающий манифест и незавершённое восстановление запрещают запуск:
команда возвращает ненулевой код и пишет код ошибки, например `manifest_missing`.
Утерянный манифест не создаётся автоматически из восстановленной БД.

Отзыв и ротация:

```bash
canterlot-hub machine revoke --config ~/.config/canterlot/hub.toml --machine-id <uuid>
canterlot-hub machine rotate-token --config ~/.config/canterlot/hub.toml --machine-id <uuid>
```

`revoke` закрывает оба потока машины с `Unauthenticated`, запрещает новые команды и записи
журналов; неизвестный UUID даёт ненулевой код и `machine_not_found`.
`rotate-token` выводит только `token=<new>` и также закрывает старый сеанс.
Сохраните новый токен в прежний `token_file` с правами `0600` и перезапустите воркер,
сохранив `machine_id`, `data_dir` и остальные UUID. Курсоры и команды сохраняются.
Старый токен больше не действует. Для отозванной машины ротация возвращает
`machine_revoked`; явное включение требует `rotate-token --reactivate`.

Хаб закрепляет `cj_journal_id` при первом принятом control handshake.
Смена журнала без перепривязки даёт терминальный `local_state_reset` и диагностическое событие.
Если локальное состояние намеренно удалено, выполните `machine rotate-token --machine-id <uuid>
--reset-local-state --config ...` и настройте воркер с новым токеном.
Этот флаг очищает привязку CJ; без него ротация сохраняет её. Сверка оставшихся
нетерминальных исполнений относится к следующему этапу; сброс не подтверждает их завершение.
`machine add` создаёт новую идентичность без привязки CJ.

## HTTP: вход, CSRF и Caddy

Хаб слушает обычный HTTP во внутренней сети; TLS и вход пользователя обеспечивает Caddy с Authelia (`forward_auth`). Хаб сам никого не аутентифицирует, он доверяет заголовку прокси при строгих условиях (ADR-0004):

- заголовок `user_header` (по умолчанию `Remote-User`) принимается, только если TCP-адрес собеседника входит в `trusted_proxies`; `X-Forwarded-*` не читается никогда;
- при заданном `proxy_secret_file` запрос обязан нести `X-Canterlot-Proxy` с тем же значением. Секрет нужен, когда у веб-хоста общий адрес для всех контейнеров (NAT) и один адрес доверенного прокси ещё не доказывает, что запрос прошёл через Authelia. Создайте его без перевода строки: `umask 077; openssl rand -hex 32 | tr -d '\n' > proxy-secret` (в файле допустимы только видимые ASCII-символы, не менее 32);
- значение заголовка личности должно равняться `owner_login`; пустой, повторённый или отсутствующий заголовок даёт `401`, чужой логин от доверенного прокси даёт `403`;
- изменяющий запрос несёт `Origin` из `allowed_origins`, заголовок `X-Canterlot-Client: 1` и `Idempotency-Key`; заголовков CORS хаб не отправляет, `OPTIONS` получает `405`.

`GET /healthz` (живость) и `GET /readyz` (база отвечает, запись установки совпадает с манифестом) не требуют личности. Лимит JSON-тела задаёт `max_json_body_bytes`. Каждая строка журнала запросов содержит только метод, шаблон маршрута, статус, задержку и `correlation_id`; заголовки, query и тела в журнал не попадают.

Caddy должен убрать служебные заголовки, присланные клиентом, до `forward_auth` и добавить секрет к запросу к хабу. Директивы `route` сохраняют порядок записи: без неё `request_header` выполнился бы после `forward_auth` и стёр бы только что скопированные заголовки.

```caddyfile
chat.example {
    route {
        request_header -Remote-User
        request_header -Remote-Groups
        request_header -Remote-Name
        request_header -Remote-Email
        request_header -X-Canterlot-Proxy

        forward_auth authelia:9091 {
            uri /api/authz/forward-auth
            copy_headers Remote-User Remote-Groups Remote-Name Remote-Email
        }

        reverse_proxy hub:8080 {
            header_up X-Canterlot-Proxy {file./run/secrets/canterlot-proxy}
        }
    }
}
```

Authelia должна защищать весь хост, включая `/api/` и WSS. Адрес Caddy (или сеть контейнера) укажите в `trusted_proxies`. WSS-соединение живёт не дольше `[http.ws].max_connection_age`, после чего хаб закрывает его кодом 4401, а клиент переподключается через прокси и заново проходит проверку.

Приёмка развёртывания, один раз и после каждого изменения Caddy: `scripts/check-proxy.sh https://chat.example cookies.txt`. Скрипт отправляет `GET /api/v1/me` с действующей сессией Authelia и поддельными `Remote-User: attacker` и `X-Canterlot-Proxy: forged`, затем тот же запрос без сессии. Он завершается ненулевым кодом, если ответ не состоит из `owner_id` и `login` владельца или если запрос без сессии дошёл до хаба. Файл `cookies.txt` — jar в формате curl (`curl -c`) с сессией Authelia; необязательная переменная `EXPECTED_LOGIN` сверяет `login`.

WSS `GET /api/v1/ws` использует подпротокол `canterlot.v1`; кадры определены как `ClientOp` / `ServerOp` в `docs/api/events.schema.json`, правила восстановления описаны в `docs/design/api.md`. В `[http.ws]`: до 16 подписок, очередь 2–256 сообщений и 4096–1048576 байт, с резервом одного слота и 1024 байт для управления; `delta_window` не больше 25 мс. Merge учитывает полный JSON-кадр и бюджет данных; невместимое одиночное событие передаётся как `oversized` со ссылкой на HTTP-чтение. Catch-up ждёт ёмкости, `lagged` возникает только после `stall_timeout` без прогресса при заполненной очереди. Shutdown закрывает WSS кодом 1001 и дожидается pump.

Контракт API: `docs/api/openapi.json` (отдаётся как `GET /api/v1/openapi.json`) генерируется командой `canterlot-hub openapi`; `scripts/gen-openapi.sh` обновляет файл, а тест `openapi_is_served_and_committed_file_matches_generated` падает, пока файл устарел. Таблица кодов ошибок: `docs/design/api.md`.

Управление очередью доступно через `POST /api/v1/inputs/{id}/cancel`, `POST /api/v1/conversations/{id}/interrupt`, `POST /api/v1/conversations/{id}/stop` и `POST /api/v1/conversations/{id}/queue/continue`. Маршрут решения запроса зарезервирован и возвращает `not_implemented`.

`POST /api/v1/conversations/{id}/inputs` ставит текст и готовые `attachment_ids` в очередь; `retry_of` ссылается на неподтверждённый ввод той же сессии. `PATCH /api/v1/conversations/{id}/settings` сохраняет изменения для следующего ввода; они становятся эффективными после подтверждения воркера. `POST /api/v1/conversations/{id}/executions` запускает исполнение, а `POST /api/v1/conversations/{id}/resume` продолжает сессию с последним native ID и при запросе снимает паузу очереди в той же транзакции.

Чтения: `GET /api/v1/snapshot/conversations`, `GET /api/v1/snapshot/machines`, `GET /api/v1/conversations/{id}/snapshot`, `/history`, `/blocks/{block_id}`, `/runs/{run_id}`, `/delegation`. Снимок и его курсор читаются в одной `REPEATABLE READ` транзакции; `GET /api/v1/events?scope=&after=&limit=` восстанавливает хвост с сохранением номеров внутренних событий через `hidden`. При устаревшем или опережающем курсоре нужен новый снимок. Подробные поля и ограничения: `docs/design/api.md`; схема событий: `docs/api/events.schema.json`. Новых таблиц конфигурации в задаче 7a нет: действуют `[http]` и пороги `[machines]`.

## Обновление контракта пилота

Порядок обновления: хаб первым, затем воркер. Возможность `pilot_v1` согласуется через `Hello.optional_capabilities` и `Accepted.capabilities`; до её подтверждения записи и команды пилота не используются. Откат хаба ниже возможности, которую воркер уже использовал, не поддерживается. Обновление управляющей службы воркера сохраняет живые процессы сопровождения и их исполнения.
