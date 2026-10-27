# canterlot

Self-hosted хаб для coding agents: Rust-хаб, Rust-воркеры на рабочих машинах, PWA на SvelteKit, PostgreSQL, gRPC между хабом и воркерами. Первая версия поддерживает Claude Code и Codex.

## Состояние

- Этап 0 завершён: оба CLI проходят обязательный набор возможностей, сводка в `docs/stage0/README.md`.
- Этап 1.1 (фундамент) завершён: транспорт gRPC выбран (ADR-0001), локальный журнал (ADR-0002), контракт хаб–воркер v1, ядро доставки хаба на PostgreSQL. Этап 1.2a добавляет gRPC-сеансы, доставку команд и квитанций. HTTP/WS, исполнение команд агентом и PWA ещё не реализованы.

## Структура

| Путь | Назначение |
| --- | --- |
| `crates/canterlot-journal/` | Локальный журнал команд и событий воркера |
| `crates/canterlot-proto/` | Типы protobuf, контракты gRPC и локального IPC управляющей службы с сопровождением |
| `crates/canterlot-worker/` | Воркер: TLS-сеанс с хабом, локальные журналы команд и квитанций |
| `crates/canterlot-hub/` | Хаб: PostgreSQL, gRPC-сервер, диспетчер и outbox команд |
| `spikes/` | Прототипы этапа 0, не продуктовый код |
| `docs/stage0/` | Отчёты этапа 0: матрица возможностей, доказательства, ограничения |
| `crates/canterlot-proto/src/harness.rs` | Единая таблица харнессов и провайдеров |
| `docs/design/api.md` | Словарь событий пилота, области подписок и каркас API |
| `docs/design/` | Автоматы состояний, правила идемпотентности и протоколы восстановления |
| `docs/adr/` | Решения по архитектуре и их обоснования |

## Тулчейн

- Rust 1.99.1 (закреплён в `rust-toolchain.toml`)
- protoc 36.2
- Node.js 24 LTS (сборка PWA)

## Разработка

Полная проверка — `scripts/check.sh`; e2e-тесты запускаются только с `--all-features`.

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

Таблицы пилота в `hub.toml` необязательны; показаны значения по умолчанию. В задаче 1 добавлен контракт конфигурации, HTTP-сервер и фоновые службы подключаются последующими задачами этапа 1.3a. Таблица `[http]` отсутствует по умолчанию; при её наличии обязательны непустые `trusted_proxies`, `owner_login`, `allowed_origins`.

```toml
[http]
listen = "127.0.0.1:8080"
trusted_proxies = ["127.0.0.1/32", "::1/128"]
owner_login = "owner"
user_header = "Remote-User"
allowed_origins = ["https://chat.example"]
max_json_body_bytes = 524288
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
stall_timeout = "15s"

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

Origin задаётся как `scheme://host[:port]`, без пути, query и fragment; `logout_url` является абсолютным HTTPS URL. `blob_dir` и `proxy_secret_file` разрешаются от каталога конфигурации, staging и blobs размещаются на одной файловой системе вне рабочих репозиториев. `multi_process = true` включает уведомления PostgreSQL в будущей реализации шины событий; при `false` шина остаётся внутри процесса.

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

Таблицы `[machine]`, `[attachments]`, `[[instruction_profiles]]` необязательны и отклоняют неизвестные поля. `fetch_attempts` считает только ошибки содержимого; ожидания транспорта ограничены `fetch_backoff_cap`. Имена профилей уникальны; `owner` зарезервировано для встроенного «Как у владельца».

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

## Обновление контракта пилота

Порядок обновления: хаб первым, затем воркер. Возможность `pilot_v1` согласуется через `Hello.optional_capabilities` и `Accepted.capabilities`; до её подтверждения записи и команды пилота не используются. Откат хаба ниже возможности, которую воркер уже использовал, не поддерживается. Обновление управляющей службы воркера сохраняет живые процессы сопровождения и их исполнения.
