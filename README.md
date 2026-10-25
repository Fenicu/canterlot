# canterlot

Self-hosted хаб для coding agents: Rust-хаб, Rust-воркеры на рабочих машинах, PWA на SvelteKit, PostgreSQL, gRPC между хабом и воркерами. Первая версия поддерживает Claude Code и Codex.

## Состояние

- Этап 0 завершён: оба CLI проходят обязательный набор возможностей, сводка в `docs/stage0/README.md`.
- Этап 1.1 (фундамент) завершён: транспорт gRPC выбран (ADR-0001), локальный журнал (ADR-0002), контракт хаб–воркер v1, ядро доставки хаба на PostgreSQL. HTTP/WS, gRPC-сервер, воркер и PWA ещё не реализованы.

## Структура

| Путь | Назначение |
| --- | --- |
| `crates/canterlot-journal/` | Локальный журнал команд и событий воркера |
| `crates/canterlot-proto/` | Типы protobuf и контракты gRPC |
| `crates/canterlot-worker/` | Воркер: конфигурация и локальные хранилища команд и квитанций |
| `crates/canterlot-hub/` | Хаб: схема PostgreSQL, модели состояний и тестовые данные |
| `spikes/` | Прототипы этапа 0, не продуктовый код |
| `docs/stage0/` | Отчёты этапа 0: матрица возможностей, доказательства, ограничения |
| `docs/design/` | Автоматы состояний, правила идемпотентности и протоколы восстановления |
| `docs/adr/` | Решения по архитектуре и их обоснования |

## Тулчейн

- Rust 1.99.1 (закреплён в `rust-toolchain.toml`)
- protoc 36.2
- Node.js 24 LTS (сборка PWA)

## Разработка

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
handshake = 10
degraded_after = 15
unavailable_after = 30
command_stored_receipt = 5
dispatch_sweep = 5
```

Относительные пути разрешаются от каталога конфига. Таймеры задаются целым числом секунд;
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

```bash
cargo run -p canterlot-hub -- machine revoke --config ~/.config/canterlot/hub.toml --machine-id <uuid>
cargo run -p canterlot-hub -- serve --config ~/.config/canterlot/hub.toml
```

На этом этапе `serve` проверяет манифест и завершается, сетевой сервер ещё не запускается.
Отсутствующий или несовпадающий манифест и незавершённое восстановление запрещают запуск:
команда возвращает ненулевой код и пишет в stderr только имя ошибки, например `manifest_missing`.
Утерянный манифест не создаётся автоматически из восстановленной БД.
