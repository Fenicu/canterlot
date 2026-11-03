# canterlot

<p align="center">

[![Forgejo](https://shields.fenicu.com/badge/Forgejo-Fenicu%2Fcanterlot-brightgreen?logo=forgejo)](https://git.fenicu.com/Fenicu/canterlot)
![этап](https://shields.fenicu.com/badge/%D1%8D%D1%82%D0%B0%D0%BF-1.3b%20PWA-00f4ee)
![агенты](https://shields.fenicu.com/badge/%D0%B0%D0%B3%D0%B5%D0%BD%D1%82%D1%8B-Claude%20Code%20%C2%B7%20Codex-1f5457)
![Rust](https://shields.fenicu.com/badge/rust-1.99-orange?logo=rust)
![SvelteKit](https://shields.fenicu.com/badge/SvelteKit-3-FF3E00?logo=svelte&logoColor=white)
![PostgreSQL](https://shields.fenicu.com/badge/PostgreSQL-18-4169E1?logo=postgresql&logoColor=white)
![Node](https://shields.fenicu.com/badge/node-24-339933?logo=node.js&logoColor=white)

</p>

Self-hosted хаб для coding agents: Rust-хаб, Rust-воркеры на рабочих машинах, PWA на SvelteKit, PostgreSQL, gRPC между хабом и воркерами. Первая версия поддерживает Claude Code и Codex.

Версия и история изменений — [CHANGES.rst](CHANGES.rst), правила бампа — [docs/versioning.md](docs/versioning.md).

## Состояние

- Этап 0 завершён: оба CLI проходят обязательный набор возможностей, сводка в `docs/stage0/README.md`.
- Этап 1.1 (фундамент) завершён: транспорт gRPC выбран (ADR-0001), локальный журнал (ADR-0002), контракт хаб–воркер v1, ядро доставки хаба на PostgreSQL. Этап 1.2a добавляет gRPC-сеансы, доставку команд и квитанций. Этап 1.3a добавляет HTTP API и WSS хаба для пилота: вход по заголовку прокси, CSRF, ошибки, идемпотентные изменения, сессии и группы, очередь и отложенные сообщения, лимиты и «кранч», активность, машины, вложения и картинки, на которые смотрел агент. Контракт для PWA (план 1.3b) заморожен: `docs/design/api.md`, `docs/api/openapi.json`, `docs/api/events.schema.json`; сквозные сценарии пилота проверяют его на настоящих хабе и воркере.

## Структура

| Путь | Назначение |
| --- | --- |
| `web/` | Статический клиент SvelteKit: токены, тема, примитивы, StageTrack, типизированный HTTP, журнал операций, черновики, загрузка вложений, синхронизация WSS в `src/lib/sync`, FakeHub в `testing/fake-hub`, лента чата, поле ввода, оболочка и список сессий, экраны машин, истории лимитов, настроек и выбора сессии для «Поделиться», платформа `src/lib/pwa` (установка, обновление по version.json, очистка), manifest и иконки из токенов, unit и браузерные проверки |
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
| `crates/canterlot-hub/src/{presets,http/presets,grpc/agent}.rs`, `migrations/0021_instruction_presets.sql`, `migrations/0022_conversation_presets.sql` | Пресеты инструкций: версии текста, CRUD API, события, снимок, стартовый пресет исполнителя и вызовы инструментов агента (`presets.list`, `presets.save`, `presets.apply`) |
| `crates/canterlot-worker/src/adapter/{canterlot_mcp/,codex/home.rs}` | MCP-сервер canterlot (разрешения, вопросы, лимиты, инструменты пресетов) и собственный `CODEX_HOME` для пресетов, кроме «Как у владельца», плюс изолированные дома делегированных сессий |
| `crates/canterlot-hub/src/http/{conversations,groups,machines,limits}.rs`, `migrations/0023_machine_workdirs.sql`, `migrations/0026_machine_home_dir.sql` | HTTP сессий и групп, чтение машин, лимитов и недавних каталогов: хаб запоминает до 20 проверенных каталогов на машину и отдаёт последние 3, домашний каталог воркера приходит в отчёте машины; лист «Как у владельца» читает глобальные файлы и память с живой машины (`GET /api/v1/machines/{id}/owner-instructions` и `.../file`, возможность `owner_instructions_v1`, в базе не кэшируется) |
| `crates/canterlot-hub/src/requests.rs`, `migrations/0018_requests.sql`, `migrations/0024_decision_unconfirmed.sql` | Запросы агента и разрешения: таблица requests (с состоянием `decision_unconfirmed`), первый ответ побеждает, команда `deliver_decision` через outbox, `POST /api/v1/conversations/{id}/requests/{id}/decision`, события `request_opened`, `request_decided`, `request_transmitting`, `request_closed`, удержание очереди и активность `WaitingForUser` |
| `crates/canterlot-hub/src/todos.rs`, `migrations/0028_agent_todos.sql` | Список задач агента: последний полный список каждой дорожки, отсечение устаревших ревизий, снимок и событие `todo_updated` (pilot.md §12.2) |
| `crates/canterlot-hub/src/background_tasks.rs`, `migrations/0033_background_tasks.sql` | Фоновые задачи агента: последнее состояние каждой задачи по `(conversation_id, task_id)`, связь с блоком вызова, отсечение устаревшего, признак хода, начатого самим CLI, в `executions.wake_turn` |
| `crates/canterlot-hub/migrations/{0019_blocks_approved_by,0025_blocks_approved_by_check}.sql` | Отметка блока `approved_by`: единственное допустимое значение `cli_policy` (действие разрешено политикой самого CLI без запроса), прочие при накате обнуляются, остальное отсекает CHECK |
| `crates/canterlot-hub/src/deletion.rs`, `migrations/0027_conversation_tombstones.sql` | Удаление остановленной сессии одной транзакцией: надгробие без содержимого, скрытые заглушки в журналах владельца, освобождение файлов после commit, подтверждение поздних записей журнала без применения; на машине остаются журналы, копии вложений и история CLI |
| `crates/canterlot-worker/src/exec/{group,lost}.rs`, `crates/canterlot-hub/migrations/0037_resolve_owner_lost.sql` | Выход из `owner_lost`: сверка группы после смерти сопровождения (сессия, метка в окружении, снимок `group.json`), доотправка его журнала, `failed(supervisor_died)` и `stopped(machine_rebooted)`, решение владельца `POST /api/v1/conversations/{id}/resolve-owner-lost` (возможность `owner_exit_v1`) |
| `crates/canterlot-hub/src/session_tools/`, `crates/canterlot-hub/src/http/session_tools.rs`, `crates/canterlot-hub/migrations/0035_session_tools.sql` | Межсессионные инструменты модели (`pilot.md` §12): `settle` (устоявшееся состояние и отпечаток), `subs` (подписки), `jobs` и `summary` (событие и итог), `access`, `origin`, `read`, `send`, `spawn`, `whitelist`, `wait`, `accept`; белый список и приём результата по HTTP |
| `crates/canterlot-worker/src/worktree.rs`, `crates/canterlot-worker/src/supervisor/git_facts.rs` | Отдельная рабочая копия делегированной сессии (подготовка и уборка) и факты git на конец хода |
| `crates/canterlot-worker/tests/session_tools_e2e.rs` | Сквозные сценарии межсессионных инструментов: запуск в копии, подписки, одобрение, приём, цикл и перезапуски |
| `crates/canterlot-hub/migrations/0020_midsession_effort.sql` | Признак `settings_midsession_effort` харнесса в каталоге машины: можно ли менять усилие у идущего исполнения (по умолчанию да; Claude Code меняет его только перезапуском, поэтому нет) |
| `crates/canterlot-hub/build.rs` | Пересборка встроенных миграций при добавлении SQL-файла |
| `crates/canterlot-hub/src/{snapshot,history}.rs` | Транзакционные снимки с границей журнала, история по scope_seq, блоки, ходы и делегирование |
| `crates/canterlot-hub/src/{subscriptions.rs,http/ws.rs}` | WSS canterlot.v1: подписки по курсору, pull-based pump, слияние дельт и отключение зависшего клиента |
| `crates/canterlot-hub/src/http/{snapshot,history}.rs` | GET снимков, истории и диапазона событий с заглушками внутренних видов |
| `docs/api/events.schema.json` | JSON Schema 2020-12 событий браузера и кадров canterlot.v1 |
| `crates/canterlot-hub/src/http/scheduled.rs` | Создание, список, изменение и отмена отложенных сообщений |
| `crates/canterlot-hub/src/{attachments/,http/attachments.rs}` | Вложения хаба: загрузка частями в staging, проверенная надёжная публикация, метаданные, привязка к вводу, сборщик и безопасная выдача |
| `crates/canterlot-hub/src/grpc/attachments.rs` | Выдача вложений машине через `FetchAttachment`, возобновление по смещению и ограничение потоков |
| `crates/canterlot-worker/src/attachments.rs` | Загрузка вложений с возобновлением, проверка SHA-256 и надёжная публикация в каталоге исполнения |
| `crates/canterlot-hub/src/{media.rs,grpc/media.rs,http/media.rs}`, `migrations/0015_block_media.sql` | Картинки, на которые смотрел агент: приём `PutMedia`, надёжная публикация, ссылка из блока по записи `BlockMedia`, выдача `GET /api/v1/media/{id}` |
| `crates/canterlot-worker/src/media.rs` | Копия картинки в каталоге исполнения: тип по сигнатуре, размеры, SHA-256; загрузка на хаб с повторами |
| `crates/canterlot-worker/tests/exec_media.rs` | Сквозные проверки картинок: ссылка только после публикации, перезапуски хаба и воркера, пропуск крупных и чужих файлов, независимость SJ и управления |
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

Проверки идут через `cargo-nextest` (`scripts/install-tools.sh` ставит готовый бинарник; `check.sh` делает это сам). Три уровня:

- `scripts/check.sh --fast` — для небольшой правки и для помощников по ходу задачи: fmt, clippy со всеми фичами, тесты изменённых крейтов и тех, кто от них зависит (для изменённого файла `tests/x.rs` только этот бинарник), первая же ошибка останавливает прогон; в вебе svelte-check, eslint, токены, vitest по diff и изменённые спеки Playwright на phone.
- `scripts/check.sh` — полная, перед слиянием в main: сравнивает с main и пропускает Rust, если менялись только web или документы, и web, если менялся только Rust; файлы `docs/api/*` (сгенерированные контракты), `scripts/*` и `.config/*` включают обе половины. `--all` отключает выбор по путям, `--base REF` меняет основу сравнения. Один clippy `--all-targets --all-features` идёт в отдельном каталоге `target/lint` одновременно с тестами; сборка без фич только проверяется `cargo check`, и из её тестов запускается единственный собственный: синтетический адаптер отключён.
- `scripts/check-web.sh --stack` — сквозной набор на настоящем хабе, только перед выкаткой.

Все тесты всех бинарников идут одним пулом, процесс на тест (`.config/nextest.toml`): потоков по числу ядер (`CANTERLOT_TEST_THREADS`); вдвое больше ускоряет пул, но рядом с линтом ломает тайминги тестов с сопровождением. Лимиты внутри процесса в таком режиме не работают, поэтому тяжёлые сценарии с сопровождением ограничены группой `heavy-e2e`, а три теста на голодание управляющего канала идут по одному и занимают часть пула (группа `starvation`). Долгие тесты видны в итоге как SLOW, `scripts/slowest-tests.py` печатает самые долгие. Повтор упавшего теста разрешён только для карантина: записи помечены `QUARANTINE` с причиной в `.config/nextest.toml` и в спеках Playwright, `scripts/list-quarantine.sh` их перечисляет, а `.superpowers/carry/flaky-tests.md` хранит историю; остальные тесты молча не перезапускаются. `cargo test` по-прежнему работает, но без групп. Сквозные сценарии пилота: `cargo nextest run --workspace --all-features -E 'binary(pilot_e2e)'`; процессы сопровождения и синтетического CLI тест убивает при завершении. Стенд с настройками как у `serve` (лимиты, активность, планировщик, хранилище вложений) собирает `testkit::spawn_test_hub_with`. Перед запуском `scripts/reap-stray-tests.sh` снимает тестовые супервизоры этой копии старше 30 минут: они намеренно переживают воркера, поэтому убитый прогон оставляет их работать. Каждая пересборка оставляет старые копии тестовых бинарников; `scripts/prune-target.sh` (его зовёт полная проверка) убирает все, кроме свежих.

В профиле `dev` зависимости собираются с `opt-level = 1`: SCRAM-аутентификация sqlx в неоптимизированной сборке стоила 0,3 с процессора на каждое подключение к тестовой базе. Инкрементальная сборка включена (кеш выходит на плато около 6 ГБ).

Спека на диске и IPC переживают релиз, который их записал: сопровождение живёт дольше воркера. `tests/spec_compat.rs` держит замороженную спеку старого релиза в `tests/fixtures/spec/`: при удалении или переименовании поля настройки адаптера добавляйте сюда спеку уходящего релиза. Совместимость со старым сопровождением проверяется вручную, без переменных тесты идут на текущем бинарнике: `CANTERLOT_OLD_RELEASE_BIN=<бинарник релиза> cargo test -p canterlot-worker --all-features --test spec_compat` (рукопожатие и журнал настоящего выпущенного бинарника; тест берёт из `tests/fixtures/spec/` спеку, которую бинарник принимает, поэтому для каждого выпуска с удалённым полем настройки нужна своя) и `CANTERLOT_E2E_SUPERVISOR_BIN=<бинарник старого коммита, собранный с --all-features> cargo test -p canterlot-worker --all-features --test exec_e2e` (весь сценарий на синтетическом CLI).

Перегенерация перечня лицензий зависимостей — `scripts/gen-licenses.sh` (обновляет `docs/licenses/THIRD_PARTY.md`).

Для тестов хаба нужны Docker и `strace` (регрессии порядка fsync); `strace` также используется сквозной проверкой долговечности каталогов worker. Из корня репозитория запустите PostgreSQL 18.6 и тесты:

```bash
scripts/test-db.sh up
source ~/.cargo/env
cargo nextest run --workspace --all-features
```

База доступна на `127.0.0.1:55440`. Тестовые пользователь, пароль и база: `canterlot`.
`DATABASE_URL` задан в `.cargo/config.toml`; переменная окружения позволяет использовать другую тестовую базу.
Каждый тест схемы создаёт отдельную базу через `sqlx::test`, поэтому пользователю PostgreSQL нужно право `CREATEDB`.
Функции подготовки данных доступны с feature `testkit` и автоматически подключаются при тестировании хаба.
Параллельные рабочие копии на одной машине поднимают отдельный сервер: `CANTERLOT_TEST_PG_PORT=55441 scripts/test-db.sh up`.
Затем `DATABASE_URL=postgres://canterlot:canterlot@127.0.0.1:55441/canterlot cargo nextest run --workspace --all-features`. Одной переменной порта для `cargo test` мало: `.cargo/config.toml` указывает на общую базу 55440. `scripts/check.sh` сам выставляет `DATABASE_URL` по `CANTERLOT_TEST_PG_PORT`, если `DATABASE_URL` не задан.
sqlx называет тестовые базы по пути теста, поэтому две копии на одном сервере удаляли бы базы друг друга.
`down` и `psql` принимают ту же переменную.

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
steer_outcome = "2m"
http2_keepalive_interval = "30s"
http2_keepalive_timeout = "10s"

```

Хаб принимает факты `pilot_v1`: модель и агент блока, инструмент и краткое описание, расход хода по моделям и агентам, заполнение контекста, лимиты и отчёт машины. `runs` хранит интервалы в машинном времени, приведённом к часам хаба; пересекающиеся и опоздавшие более чем на минуту ходы исключаются из оценки доли недельного лимита. История расхода группируется по разговору, модели, агенту и дню выбранного часового пояса; временные выборки используют полуинтервал `[from, to)`. Прирост недельного лимита хода засчитывается один раз, в строке с наибольшим расходом токенов; отрицательный прирост (сброс окна внутри хода) в оценку не входит.

В `[limits]` уже работают `snapshot_min_interval` (снимок при изменении значения/сброса или по истечении интервала между наблюдениями) и хранение времени наблюдения. Старые наблюдения остаются историей и не изменяют текущее значение; событие `limits_changed` пишется только при изменении значения или времени сброса. Без образца машинных часов наблюдение сохраняется только в историю с диагностикой. Снимки отправляются после commit подходящим сеансам `pilot_v1`, включая первое заполнение после отчёта машины.

Правила лимитов (`crunch.rs`, [контракт](docs/design/api.md#лимиты-кранч-и-недельный-резерв)) работают внутри цикла планировщика и останавливаются вместе с ним. Упор в лимит без «кранча» ставит паузу `limit_reached` со временем сброса; с «кранчем» очередь удерживается до сброса (`reset_grace` после него, `unknown_reset_retry` при неизвестном времени), а пробуждение проверяется по новому показанию лимитов. Без подтверждения задание повторяется по `verify_backoff` и после commit просит одну доступную машину провайдера с `pilot_v1` опросить лимиты: не чаще `poll_min_interval`, после исчерпания шагов не чаще `poll_floor`. Недельное показание не ниже `weekly_reserve_pct` включает резерв: новые ходы провайдера удерживаются, идущий ход завершается. Выход из резерва требует нового окна или двух показаний ниже порога на `reserve_exit_hysteresis_permille`. После сброса продолжаются только сессии в «кранче», которые остановил лимит; текст продолжения берётся из сессии или `default_resume_text`. Резерв тратится только действием владельца `spend-reserve` для одной сессии до сброса недели.

`machine_info` заменяет каталог харнессов атомарно, проверяет лимиты вложений и сохраняет последнюю допустимую метрику. Проектор доступности получает отдельное PostgreSQL-соединение с singleton-блокировкой, при первом захвате в процессе сбрасывает сохранённую доступность и обновляет её каждые 5 секунд и по изменениям Registry. При повторных захватах проекция сразу использует живой Registry. При потере соединения служба освобождает блокировку и переподключается; второй процесс остаётся пассивным и принимает работу после освобождения блокировки. Shutdown завершает службу.

Обновление: сначала хаб, затем воркер. Откат хаба ниже уже использованной воркером capability не поддерживается.

HTTP сессий поддерживает создание по каталогу машины (режим разрешений по умолчанию `bypass`), старт в одной транзакции, закрепление, цвет, архив, удаление остановленной сессии (`DELETE /api/v1/conversations/{id}`), группы, поиск по заголовку с ICU, список с курсором и отметку прочитанного. Проверенный каталог запоминается (до 20 на машину, экран новой сессии показывает последние 3); чтение машин включает каталоги, ОС, метрики и предупреждения; история лимитов учитывает часовой пояс и расход по агентам. Поля и маршруты описаны в [контракте API](docs/design/api.md#сессии-группы-и-чтение-данных-машин). Пресеты инструкций хранит хаб: `GET` и `POST /api/v1/presets`, `GET`, `PATCH` и `DELETE /api/v1/presets/{id}` (версии текста, правка с `base_revision`, мягкое удаление), событие `instruction_preset_changed` и поле `presets` в снимке списка; текст пресета отдаёт только детальное чтение и чтение агента. Ссылка на пресет (`clean` по умолчанию, `owner` или UUID) — поле `instruction_profile` создания сессии и настроек; сессия закрепляет применённую версию, а пресет, отличный от `owner`, получает только сеанс воркера с `instructions_v1`. Новых таблиц конфигурации не требуется: текст кранча берётся из `[limits]`, предупреждения из `[machines]`, HTTP использует существующий `[http]`.

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
unverified_wake_after = "20m"

[limits.poll_floor]
claude = "30m"
codex = "5m"

[activity]
stalled_generating = "5m"
stalled_tool = "30m"
tick = "15s"
debounce = "250ms"
idle_quiet = "5s"

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

[session_tools]
max_spawn_depth = 3
max_chain_depth = 8
pair_sends = 10
pair_window = "10m"
sender_sends = 30
sender_window = "10m"
parent_spawns = 10
spawn_window = "1h"
```

Origin задаётся как `scheme://host[:port]`, без пути, query и fragment; `logout_url` является абсолютным HTTPS URL. `blob_dir` и `proxy_secret_file` разрешаются от каталога конфигурации, staging и blobs размещаются на одной файловой системе вне рабочих репозиториев. Все процессы хаба с общим томом координируют загрузку, публикацию и удаление через `flock` на постоянных `blob_dir/locks/<id>`; эти lock-файлы нельзя удалять во время работы хабов. `multi_process = true` включает `NOTIFY` и слушатель PostgreSQL с восстановлением соединения; при `false` шина остаётся внутри процесса.

`WorkerData.FetchAttachment` читает опубликованные blobs из того же `[attachments].blob_dir` чанками до 128 КиБ, начиная с запрошенного смещения. На машину допускается четыре потока; пятый получает `RESOURCE_EXHAUSTED`. Потоки не удерживают соединения PostgreSQL и завершаются с `UNAVAILABLE` при остановке хаба. Готовность файлов и причины ожидания поступают в `CJ` как `attachment_ready` / `attachment_failed`; обрыв передачи сам по себе не отклоняет ввод. Blobs связанных вложений хранятся до удаления сессии; само удаление сессий появится после пилота.

Картинки, на которые смотрел агент (pilot.md §5.7), принимает `WorkerData.PutMedia`: первый кадр несёт заголовок (`media_id`, исполнение, блок, тип, размер, SHA-256), следующие - данные до 128 КиБ. Тело пишется в staging того же `[attachments].blob_dir`, проверяется по длине и SHA-256, публикуется (`fsync`, `rename`, `fsync` каталогов), сверяется сигнатура типа (png, jpeg, webp, gif), и только затем фиксируется строка `block_media`; ответ `MediaStored` уходит после commit. Повтор с тем же ID и checksum возвращает тот же ответ без копирования, другой checksum даёт `ALREADY_EXISTS`. Ссылка из блока и событие `block_media_attached` появляются только по записи `BlockMedia` журнала сопровождения, которую воркер пишет после `MediaStored`. История и снимок несут `media` на блоке, `GET /api/v1/media/{id}` отдаёт картинку владельцу (`inline`, `nosniff`, ETag = SHA-256). Сборщик вложений не трогает blobs с записью `block_media` и удаляет картинки без ссылки из блока через `gc_grace`.

`[session_tools]` задаёт пределы межсессионных инструментов модели: глубину вложенных запусков (`max_spawn_depth`, 3) и цепочки сообщений (`max_chain_depth`, 8), число сообщений на пару (`pair_sends`, 10 за `pair_window`, 10 минут) и на отправителя (`sender_sends`, 30 за `sender_window`) и число запусков на родителя (`parent_spawns`, 10 за `spawn_window`, час); превышение даёт модели ошибку инструмента. Таблица необязательна: добавляйте её только при смене умолчаний. `idle_quiet` в `[activity]` (5 секунд) - сколько сессия должна простоять в `idle` без фоновой работы и без новых событий, чтобы считаться устоявшейся. Работают все инструменты `sessions.*`; `sessions.accept` и `POST /api/v1/conversations/{id}/accept` останавливают принятую сессию, убирают её рабочую копию и архивируют (ветка с коммитами остаётся). `session_interrupt` останавливает текущий ход прямым родителем, `session_stop` останавливает процесс, сохраняя рабочую копию и ветку; обе идемпотентны и не трогают корневые сессии владельца. `session_send` и `session_spawn` принимают `attachment_ids` — готовые вложения из разговора вызывающего, которые цель получает как вложения владельца без повторной загрузки. `canterlot_limits` показывает всех известных хабу провайдеров (Claude, Codex, OpenCode) с пятичасовым, недельным и месячным окнами, процентом, временем сброса и свежестью; первая строка остаётся прежней строкой текущего провайдера. `share_file` отдаёт владельцу файл из процесса агента: путь читается с `O_NOFOLLOW`, проверяется по дескриптору, секретные имена и `~/.ssh` запрещены, потолок 50 МиБ; файл появляется в ленте блока `share_file` с картинкой, видео или именем, размером и скачиванием. Какие пары харнесса и модели агент запускает сам, владелец задаёт в настройках (`/api/v1/spawn-whitelist`); запуск вне списка ждёт его решения в карточке. Изолированный запуск и приём результата требуют воркера с возможностью `session_tools_v1`. Поведение целиком описано в `docs/design/pilot.md` §12.

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
# codexbar_path = "/home/user/.local/bin/codexbar" # Без значения: PATH, затем ~/.local/bin.
limits_poll_interval = "15m" # Не меньше 15 минут: точка usage ограничена по частоте.

[attachments]
fetch_parallelism = 2
fetch_retry = "2s"
fetch_attempts = 5
fetch_backoff_cap = "30s"

[claude_code]
executable = "claude" # Либо абсолютный путь к CLI.
allow_paid_model_poll = false

[codex]
executable = "codex" # Либо абсолютный путь к CLI.
rate_limits_poll_interval_s = 300
# home_dir = "/home/user/.local/state/canterlot-worker/codex-home" # CODEX_HOME canterlot для пресетов, кроме «Как у владельца»; по умолчанию <data_dir>/codex-home

# Необязательная таблица: без неё харнесс OpenCode не показывается и не запускается.
# [opencode]
# executable = "opencode" # Либо абсолютный путь к CLI.
# data_root = "/home/user/.local/state/canterlot-worker/opencode" # По умолчанию <data_dir>/opencode.
# providers = ["anthropic"] # Провайдеры в каталоге моделей сверх opencode-go.
# default_model = "opencode-go/muse-spark-1.3-contributor" # Первая в каталоге, форма выбирает её.
# owner_config = "/home/user/.config/opencode" # Каталог конфигурации владельца, по умолчанию $XDG_CONFIG_HOME/opencode.
# refuse_paths = [] # Каталоги, где базу держать нельзя, сверх данных владельца.
```

Замените `/home/user` своим домашним каталогом; `~` внутри TOML не раскрывается.
Относительные пути воркера считаются от рабочего каталога процесса. Неизвестные поля,
включая опечатки в `[timers]`, отклоняются; числовые таймеры без единицы измерения запрещены.
`data_dir` создаётся с правами `0700`, файлы журналов и `hub.toml` — `0600`.
Журнал воркера идёт в stderr, фильтр задаёт `RUST_LOG` (по умолчанию `info`).

Таблицы `[machine]` и `[attachments]` необязательны и отклоняют неизвестные поля. `fetch_parallelism` равен 1 или 2 и ограничивает загрузки всей машины; число попыток и интервалы положительны. `fetch_attempts` считает только ошибки содержимого (checksum и преждевременный `last`), между ними выдерживается `fetch_retry`. Обрывы, `RESOURCE_EXHAUSTED` и отсутствие соединения не расходуют попытки: повтор начинается через 250 мс с удвоением до `fetch_backoff_cap`, ожидание восстановления хаба не ограничено этим интервалом. Таблица `[[instruction_profiles]]` удалена планом 2c: такой раздел теперь ошибка разбора, пресеты инструкций хранит хаб.

Картинки, на которые смотрел агент, сопровождение читает сразу в момент события (байты из результата инструмента или файл по пути, только обычный файл до 20 МиБ с сигнатурой png, jpeg, webp или gif) и кладёт копию в `data_dir/executions/<execution_id>/media/<media_id>` (каталог `0700`, файл `0600`). Крупный или другой файл картинки не даёт: блок сохраняет путь и размер. Управляющая служба загружает копии по одной на исполнение через отдельное соединение HTTP/2, так что поток SJ и управляющий сеанс её не ждут; транспортные ошибки повторяются с backoff до `fetch_backoff_cap`, после перезапуска управляющей службы сопровождение повторяет запрос. Запись `BlockMedia` попадает в SJ только после ответа хаба о публикации, копия затем удаляется. Байты и пути не попадают в журналы.

Рабочие копии делегированных сессий лежат в `data_dir/worktrees/<id сессии>`: корень создаётся при старте воркера с правами `0700` и сообщается хабу в `MachineReport.worktree_root`; воркер сам готовит копию (`git worktree add -b canterlot/<последние 8 hex id> … <основа>`, хуки отключены) и убирает её по команде `RemoveWorktree`. Это полноценные чекауты, следите за местом на диске (предупреждение `disk_usage_high` по умолчанию с 85 %). В `.git` исходного репозитория появляются запись о копии и ветка `canterlot/…`; рабочее дерево, индекс и настройки репозитория не меняются, ничего не пушится. Ветка с коммитами остаётся после уборки. Без возможности `session_tools_v1` у воркера изолированный запуск не стартует.

Вложения воркера находятся только в `data_dir/executions/<execution_id>/attachments/`, вне рабочих каталогов. Временный файл `.tmp/<attachment_id>.part` возобновляется с его длины, в том числе после перезапуска управляющей службы. Публикация: проверка длины и SHA-256, `fsync` файла, `rename` в `<uuid>-<безопасный суффикс>`, `fsync` каталогов. Каталоги имеют права `0700`, файлы `0600`; агент получает абсолютный путь и метаданные через `Deliver.files`. Уже опубликованный файл проверяется повторно без загрузки.

Для каждого ввода actor запускает отдельную отменяемую задачу и передаёт ввод сопровождению только после готовности всех файлов. Отмена, отзыв и остановка исполнения удаляют его временные файлы сразу; завершение управляющей службы оставляет их для продолжения. Опубликованные копии удаляются после терминального состояния исполнения и полного подтверждения его SJ. `attachment_ready` / `attachment_failed` пишутся только для команды, полученной с согласованным `pilot_v1`; этот признак сохраняется в receipt и защищён временной durable-меткой до записи inbox. На один непрерывный сбой транспорта пишется одна причина `hub_unreachable`; окончательная ошибка файла заканчивается `command_rejected{reason:"attachment_unavailable"}` без передачи агенту.

После согласования `pilot_v1` воркер отправляет `MachineReport` на каждом подключении: каталог харнессов и моделей, канонические рабочие корни, домашний каталог пользователя воркера, версию, ОС и ограничения вложений. Метрики `[machine].metrics_interval` собираются из `/proc` и `statvfs` в блокирующем пуле: CPU, память, нагрузка, диски без повторов одного устройства, процессы исполнения и потомки CLI с проверкой времени старта. Заполненная управляющая очередь отбрасывает sample; heartbeat сохраняет приоритет. Незавершённый сбор дожидается завершения при остановке воркера.

С feature `synthetic-adapter` доступны `synthetic`, `synthetic-exit-on-interrupt` (провайдер `claude`) и `synthetic-codex` (`codex`). Каталог содержит `synthetic-large` (усилие `low/medium/high`) и `synthetic-small` (`low/medium`), режимы `owner/ask/bypass/read_only`. Начальные параметры передаются через `StartCli`; изменения settings проверяются до durable intent и поступают в CLI с очередным вводом. Сценарии JSON описаны в `src/adapter/synthetic_cli.rs`: модели, субагенты, usage, limit-факты, исчерпание контекста, чтение локального вложения по индексу и просмотр картинки (`view_image`: блок `Read` с путём и картинка по пути или байтами). Текст и пути не попадают в диагностику.

`PollLimits` читает `data_dir/synthetic-limits/<provider>.json` с окнами `five_hour`/`weekly` и полями `used_permille`, `resets_at_ms`; без синтетической feature и без `[claude_code]` источник пуст. Последний `LimitsSnapshot` хранится в памяти (`LimitsCache`). Без `pilot_v1` новые факты не передаются, остальные журналы продолжают отправляться; диагностика `journal_withheld` указывает на несовместимость. Обновляйте сначала хаб, затем воркер; откат хаба ниже уже использованной возможности воркера не поддерживается.

Адаптер Claude Code собирается без дополнительных feature. Воркер показывает `claude_code` в каталоге только при наличии `[claude_code]` и успешной проверке `--version` при запуске. Сейчас минимум 2.1.311, проверена версия 2.1.311; более новая версия допускается с диагностикой `untested_cli_version`. При отсутствии CLI, ошибке проверки или старой версии харнесс отсутствует в каталоге, причина пишется один раз. Сопровождение повторяет проверку перед запуском исполнения. Субагент не шлёт потоковых кадров: его вызовы инструментов, текст и рассуждение приходят целыми кадрами `assistant` с `parent_tool_use_id` вызова `Agent` и становятся блоками его дорожки. Каталог содержит Opus 1M, Sonnet 1M и Fable 1M (`opus[1m]`, `sonnet[1m]`, `fable[1m]`) и Haiku, режимы `owner`, `ask`, `bypass`, `read_only`. Сессии, созданные с `opus`, `sonnet` или `fable` до сокращения каталога, продолжают работать: каталог ограничивает только новый выбор, а лист настроек показывает их текущую модель. Первая модель каталога считается выбором формы по умолчанию; почему у Haiku нет варианта 1M, описано в [docs/stage0/claude-code.md](docs/stage0/claude-code.md). Модель и режим разрешений меняются между ходами; усилие пока задаётся только при запуске (`settings_midsession_effort: false`). Изменение усилия возвращает `effort_change_unsupported`.

Старт Claude Code. CLI присылает `system/init` только после первого сообщения в stdin и после подключения серверов из `--mcp-config` (на загруженной машине до 10 с и больше), поэтому пока процесс не показался, передача ввода ждёт `ready_deadline` (30 с по умолчанию), а не `transmit_deadline`. То же действует для процесса после перезапуска со сменой инструкций. С первым `system/init` ожидание доказательства возвращается к `transmit_deadline`. Если init не пришёл за `ready_deadline`, пишется обязательная диагностика `cli_init_missing`, исполнение завершается как `failed(start_failed)`, а не `protocol_incompatible`: протокол не нарушен, процесс не поднялся. Ответ CLI раньше init не отвергается: адаптер дожидается init в пределах того же срока.

Инструкции агента приходят от хаба в `StartExecution` и смене настроек (`Instructions`: режим, текст, пресет и версия; текст не длиннее 64 КиБ и не входит в лимит меты). Неверные инструкции отклоняют запуск (`invalid_instructions`) или ввод. Смена инструкций применяется перезапуском CLI между ходами с тем же нативным ID (`CliRestarted` обновляет владельца); текст инструкций хранится только в хабе и каталоге исполнения и не попадает в журналы и события.

Пресет «Как у владельца» (`Owner`) сохраняет настройки, плагины, MCP-серверы и инструкции владельца. Делегированная сессия (создана через `session_spawn`) MCP-серверов владельца не получает ни в одном пресете и адаптере: хаб ставит `isolate_mcp`, Claude Code запускается с `--strict-mcp-config`, Codex в отдельном доме с конфигом без `mcp_servers` и с `--disable apps --disable plugins`, OpenCode с копией конфигурации без `mcp` (подробно в `docs/design/pilot.md` §4). «Без инструкций» (`Clean`, по умолчанию) добавляет к `--settings` вычисленные при каждом запуске абсолютные пути файлов инструкций владельца строго выше корня репозитория рабочего каталога (`CLAUDE.md`, `CLAUDE.local.md`, `.claude/CLAUDE.md`, `AGENTS.md`, `.claude/AGENTS.md`, `.claude/rules/**`) — по логическому пути и по пути с раскрытыми символическими ссылками (CLI идёт от физического cwd), файлы `CLAUDE.md` и `rules/**` пользовательского каталога (`~/.claude` и `CLAUDE_CONFIG_DIR`, если задан; оттуда же берётся `.credentials.json` для опроса лимитов) и `"autoMemoryEnabled": false`; файлы самого репозитория остаются. Пользовательский пресет (`Preset`) — то же плюс `--append-system-prompt-file <exec_dir>/instructions.md` (текст хаба, `0600`, каталог исполнения вне репозитория). Каждый пресет передаёт `--system-prompt-snapshot off`, чтобы перезапуск с `--resume` применял новый текст. Смена инструкций применяется перезапуском CLI между ходами с флагами действующих настроек: stdin закрывается (до 5 с), затем SIGTERM группе и до 2 с, затем SIGKILL; после него дочитывание stdout и ожидание выхода ограничены секундой каждое, остатки дерева гасит контейнер; ввод пишется только в новый процесс; неудача — запасной запуск с `--resume` со старыми настройками и отказ входа, а провал запасного запуска (или невозвращённый файл прежних инструкций) останавливает исполнение с исходом `start_failed`. Текст пресета не попадает в журналы, события и диагностику.

Адаптер Codex говорит с `codex app-server` по stdio JSON-RPC и собирается без дополнительных feature. Таблица `[codex]` задаёт путь к CLI (по умолчанию `codex` из `PATH`), интервал фонового опроса лимитов `rate_limits_poll_interval_s` (по умолчанию 300, меньше 60 поднимается до 60) и необязательный `home_dir` — корень собственного `CODEX_HOME` canterlot (по умолчанию `<data_dir>/codex-home`, временные каталоги запрещены); без живой сессии воркер раз в 60 с ставит Codex в очередь опроса и при необходимости вызывает `account/rateLimits/read` через короткий probe app-server. Минимум 0.164.0, проверена 0.164.0, более новая версия допускается с диагностикой `untested_cli_version`. Каталог показывает `codex` только при настроенной таблице и успешном зонде `model/list` на старте воркера; модели и усилия берутся из ответа CLI, умолчательная первой. В пресете «Как у владельца» (`Owner`) запуск добавляет лишь флаги `--enable default_mode_request_user_input` и `--disable incremental_tools` и свой MCP-сервер через `-c mcp_servers.canterlot.*`, конфигурация владельца не меняется. Второй флаг нужен всем пресетам: с инкрементальными инструментами смена модели в 0.164 переобъявляет пространство `functions` без неизменного `request_user_input`, и модель спрашивает через `request_user_input_async` без карточки. Тред помнит режим при создании, поэтому старый тред так и остаётся; вопрос без карточки в нём виден текстом и дополняется диагностикой `codex_async_question`. Остальные пресеты запускаются со своим `CODEX_HOME`: символические ссылки на записи дома владельца (вход, конфиг, сессии, навыки) без `AGENTS.md`, файлов SQLite, блокировок и каталогов демонов, плюс общий `AGENTS.md` canterlot; ссылка `sessions` есть всегда (каталог владельца создаётся до первого запуска, настоящий каталог в доме canterlot переносится к владельцу), иначе тред не возобновится при смене режима; `AGENTS.md` репозитория действует, память Codex отключена (`--disable memories`). Окружение наследуется за вычетом `OPENAI_API_KEY`, `CODEX_API_KEY`, `OPENAI_BASE_URL`, `CLAUDECODE`, `CLAUDE_CODE_*`, `CANTERLOT_*` и `RUST_LOG`; у не-владельца `CODEX_HOME` подменяется, а `CODEX_SQLITE_HOME` убирается. Рукопожатие: `initialize` с `experimentalApi`, затем `account/read` без обновления токена (без входа — `auth_required`), `config/read` и `model/list`; текст пресета едет в `developerInstructions` (`«Без инструкций»` — пусто, перекрывает конфиг владельца; при смене — однострочное примечание canterlot первым). Смена инструкций применяется перезапуском app-server между ходами и `thread/resume` с новыми инструкциями; неудача — запасной запуск со старыми домом и инструкциями, отказ входа `instructions_change_failed`, хаб снимает несостоявшуюся смену с ожидания. Возобновление — новым исполнением с `thread/resume` по нативному ID. Ввод идёт строго по одному ходу с доказательством эхом `clientId`, повтор сверяется с историей, настройки меняются оверрайдами `turn/start`, прерывание держит процесс и гасит терминалы прерванного хода. Субагенты: `thread/started` потомка в 0.164 нет, потомок принимается из элемента `subAgentActivity` (мультиагентность v2, имя из `agentPath`) или из завершения `collabAgentToolCall` с `spawnAgent` (v1, имя `subagent`, задача из `prompt`). В ленте это вызов `spawn_agent`, шаги потомка лежат под ним, а конец хода потомка становится результатом вызова (`error`, `interrupted` или без сводки), от него хаб ведёт состояние в панели делегирования. Потомок может пережить прерванный главный ход: его блоки принимаются, фазу сессии он не трогает. Кадры неизвестного треда ждут объявления потомка в ограниченном буфере, до конца главного хода; не нашедшие хозяина и пришедшие вне хода дают диагностику `unattributed_thread_frame`. Разрешения и вопросы идут через запросы сервера с доказательством `serverRequest/resolved`; вызов без запроса при чужой политике помечается `cli_policy`. Elicitation MCP открывает карточку разрешения только в режиме `url` (согласие — `content: null`); `form`, `openai/form`, `openaiForm` и `openai/userVerification` ждут содержимого или доказательства, которых PWA пока не собирает, и сразу отклоняются (`decline`) с диагностикой `unsupported_elicitation`. Ошибки хода разбираются по таблице: вход, модель, повтор, классы сервера, лимиты, контекст, прерывание. Обычные тесты используют поддельный app-server и записанные кадры, без настоящих запросов. Владелец сверяет схему после обновления CLI и запускает live smoke отдельно: Запросы сервера Codex решает владелец: команды, правки, вопросы, `url`-запросы MCP и подтверждения инструментов MCP (форма без полей с `_meta.codex_approval_kind`) приходят карточкой, разрешение действует на один вызов. Настоящие формы и `openai/userVerification` отклоняются сразу: PWA не собирает их поля. Запрос вне хода и открытый запрос субагента к концу хода получают отказ.

```bash
scripts/codex-schema-check.sh
CANTERLOT_CODEX_SMOKE=1 cargo test -p canterlot-worker --all-features --test codex_e2e -- --nocapture
```

Адаптер OpenCode (`src/adapter/opencode`) говорит с `opencode serve --stdio --port 0` по HTTP и SSE на loopback, один сервер на исполнение. Харнесс спрятан за таблицей `[opencode]`: без неё каталог, реестр и оба других адаптера работают как раньше. Ключи таблицы: `executable` (по умолчанию `opencode` из `PATH`), `data_root` — корень данных canterlot (по умолчанию `<data_dir>/opencode`, внутри `data/`, `state/`, `cache/`, `tmp/`), `refuse_paths` — каталоги, где базу держать нельзя, `providers` — провайдеры в каталоге моделей сверх `opencode-go`, и `owner_config` — каталог конфигурации владельца; каталог `opencode` в данных владельца запрещён всегда: управляемая служба на общей базе подхватывает чужие живые исполнения. Минимум и проверенная версия 2.1.4, более новая допускается с диагностикой `untested_cli_version`, ветка 1.x не поддерживается. Даже `opencode --version` заводит каталоги и журнал, поэтому проверка идёт с теми же каталогами XDG, что и сервер. Окружение собирается заново: от воркера берутся только `PATH`, `HOME`, `USER`, `LANG`, `TERM` и `XDG_CONFIG_HOME`; `TMPDIR` — каталог исполнения, пароль `OPENCODE_PASSWORD` случаен на запуск, `OPENCODE_CONFIG_CONTENT` несёт слой canterlot (`update`, `share`, `snapshots`, `experimental.todo` и MCP-сервер с `codemode: false`).

Запуск: версия, каталоги, сервер со своим stdin на всё время жизни (EOF завершает его вместе с shell, в том числе при смерти супервизора), строка готовности только с loopback, поток событий раньше всех запросов, `GET /api/info` с проверкой версии и PID, сессия `POST /api/session` со своим `ses_<hex>`, заголовком и правилами режима (возобновление: `GET`, `PATCH`, `POST .../model`), ожидание MCP (`failed` или срок — диагностика `mcp_unavailable`) и агентов. У непроверенной версии `GET /openapi.json` сверяется с манифестом, расхождение — диагностика `openapi_changed`. Мягкое закрытие: EOF stdin, `stop_grace`, SIGTERM, SIGKILL; принудительное сразу убивает группу. Разрыв потока, потерянный или неразобранный кадр и запуск после падения идут одним путём: подписка, затем чтение проекций. Сообщение истории, которое нельзя прочитать, обрывает хвост с диагностикой `history_unreadable`, а не сессию. Снимается сервер только если подписка и сверка не удались четыре раза подряд: исполнение получает `ProcessExited` вместо вечного ожидания. При возобновлении правила режима ставятся и старым потомкам, иначе субагент продолжил бы потомка по прежним правилам. Половина эмодзи от сервера (`\ud83d`) становится U+FFFD.

Ввод: `POST .../prompt` с `msg_<command hex>` и всегда `delivery: "queue"`, значение по умолчанию `steer` влило бы его в идущий ход. Ответ 200 с тем же `id` — доказательство `opencode_inbox_admitted`, 400, 404 и 409 — отказ с именем ошибки, остальное — `Undetermined`. Повторной отправки нет: после `Undetermined` адаптер читает `.../inbox` и `.../message/{id}`, а поздний факт из потока или проекции становится `InputAccepted`; повторная попытка команды сперва сверяется с историей. Вложения идут путём в тексте; картинка вдобавок идёт в `files` с `file://`, если модель её принимает (`capabilities.input` в `GET /api/model`) и файл не больше 20 МиБ: двоичный файл там молча исчезает, а картинка у модели без такого ввода становится текстом ошибки. Ответ на ввод возвращает вложения в base64 и читается с тем же потолком 64 МиБ, что другие ответы: сверх него адаптер считает исход неизвестным и сверяется с проекциями. Поэтому `images` харнесса всегда `true`, а точное значение и окно контекста идут в каталоге на модель: `ModelInfo.images`, `ModelInfo.context_window`. Картинка, на которую посмотрел агент, лежит `data:` URI в `session.tool.success.content`: такой файл `image/*` становится `MediaSource::Base64` блока вызова, размер виден до декодирования, а декодирует подготовка картинки, не цикл супервизора. Текст крупнее порога финала блока (64 КиБ) идёт ещё и дельтой: супервизор режет её по границам букв, иначе хаб собрал бы пустой блок. Каталог моделей воркер читает при старте коротким запуском сервера в тех же каталогах данных: в него попадают `opencode-go` и явно включённые провайдеры, выключенные и устаревшие модели отброшены, идентификатор `провайдер/модель`, выбор по умолчанию из `GET /api/model/default` первым, у модели без вариантов единственное усилие `default`, которое серверу не передаётся. Пустой каталог не скрывает харнесс, а отказ сервера скрывает. Смена модели и усилия идёт `POST .../model` перед вводом; вариант сверяется с `/api/model` заранее, потому что сервер принимает неизвестный с 204 и падает только на ходе. Настройки вступают в силу вместе с принятым вводом: при отказе правила режима возвращаются на сервер сразу, а модель и усилие следующим вводом. Поток `/api/event` разбирается отдельной задачей с потолком кадра 64 МиБ: куски могут резать кириллическую букву. Блоки текста и рассуждений получают стабильный UUIDv5 от сессии, сообщения, вида и номера, чужие сессии отбрасываются, неизвестный тип даёт одну диагностику, неизвестный `session.execution.*` останавливает исполнение. Инструменты: начало блока ждёт `session.tool.called`, имя берётся из `session.tool.input.started`, ключ вызова — пара сообщения и `id`, поэтому параллельные вызовы могут завершаться в любом порядке. Вне `bypass` блок удерживается до запроса разрешения (без пометки), начала работы или конца вызова без запроса (`approved_by: "cli_policy"`), удержание общее с Codex (`adapter/held_tool.rs`). Статус выхода `shell` виден в сводке (`exit 3`), отказ — `declined`, прерывание — `interrupted`. Режимы задаются правилами сессии: `bypass` разрешает всё, `ask` спрашивает обо всём, кроме вопросов, списка задач, `canterlot_limits` и `canterlot_presets_list`, `read_only` запрещает всё, кроме чтения, поиска, вопросов, субагентов, списка задач, `canterlot_limits` и `canterlot_presets_list`, `owner` оставляет правила конфигурации владельца. Смена режима идёт `PATCH` перед вводом, на корневую сессию и на каждого живого потомка: копию правил он получает при создании и смены родителя не видит; `owner` шлёт пустой список, потому что `null` сервер игнорирует. Выход из `read_only` (в том числе при возобновлении, по правилам, оставшимся в сессии) добавляет к первому вводу напоминание в `<system-reminder>`: сервер возвращает Code Mode пользовательским `<system-update>`, и модель перестаёт звать `shell` и `question` напрямую. Напоминание уходит только на сервер, лента, журнал и хаб хранят ввод как набран; при отказе или неизвестном исходе ввода оно ждёт следующего. Не применившиеся правила останавливают ввод и ставятся заново перед следующим. Запрос разрешения доходит карточкой `RequestOpened`, и ответ идёт на путь его сессии: `once` при согласии и `reject` с текстом при отказе, а `always` не отправляется, он записал бы правило на весь проект. Отказ без текста оборвал бы ход, поэтому текст есть всегда, в том числе у отказа без участия владельца (карточка больше журнала, запрос вне хода). Отказ снимает всю пачку запросов сессии, соседние карточки закрываются по `permission.replied` с причиной `cli_cancelled`, запрос прерванного инструмента — по концу инструмента. Доказательство решения — ответ сервера `opencode_reply_accepted`; 404 и 409 дают `request_not_pending`, обрыв — `Undetermined`, повторной отправки нет: запрос живёт только в памяти сервера, отметка об ответе ставится до запроса. Вопрос — форма инструмента `question`: `key` становится `id`, `custom` — `allow_custom`, `multiselect` — `multi_select`, ответ уходит по ключам формы значениями вариантов. Полноту проверяет адаптер: сервер принял бы неполный ответ и подставил модели `Unanswered`. Форма больше чем из четырёх вопросов, с полями, которых карточка не соберёт, или не от `question` (`sessionID: "global"` у форм MCP) отменяется `DELETE` с сообщением и диагностикой `opencode_form_refused`, иначе вызов ждал бы вечно. Прерывание — `POST .../interrupt`: `interrupted: true` даёт `Acknowledged`, `false` — `NotApplicable`, обрыв — `Undetermined`; всё, что сервер не снял, получает отказ с текстом, иначе вызов ждал бы вечно. Фоновые команды прерывание не снимает, а `DELETE /api/shell/{id}` разбудил бы модель, поэтому они продолжают работать, а после конца хода `GET /api/shell` показывает работающие команды сессии блоком-предупреждением. Завершение фоновой команды или субагента запускает период без ввода: адаптер шлёт `RunAdopted`, для супервизора это ход последней команды, поэтому карточки доходят до владельца, прерывание работает, а новый ввод ждёт конца; `RunFinished` с `adopted` добавляет расход и контекст к той же команде. Если ввод всё же обогнал конец такого периода, сервер сливает их, расход идёт новому вводу, а граница берётся из `session.inbox.delivered`. Конец хода — `session.execution.{succeeded,failed,interrupted}`, расход суммируется по шагам (`output` плюс `reasoning`, `input` уже без кэша), контекст — последний шаг, окно — `limit.context` из `GET /api/model`, из ответа которого берётся только оно: там же лежит `settings.apiKey`.

MCP canterlot подключается на исполнение через `OPENCODE_CONFIG_CONTENT`: `codemode: false` обязателен, иначе инструменты доступны только изнутри `execute`. Имена у модели идут с именем сервера: `canterlot_canterlot_limits`, `canterlot_canterlot_presets_list`, `canterlot_canterlot_preset_save`, `canterlot_canterlot_preset_apply`. Инструкции сервера и текст бюджета свои, `OPENCODE_SERVER_INSTRUCTIONS`, без упоминания других CLI. `canterlot_limits` отвечает адаптер, пресеты идут через хаб как `AgentCall` и возвращаются результатом инструмента. Источника лимитов внутри OpenCode нет, поэтому ответ пока «no limit data yet». Старт ждёт `connected` в `GET /api/mcp` и ещё 250 мс: инструмент появляется после дебаунса 100 мс, а публичного сигнала нет, событие `mcp.tools.changed` в поток не идёт; L2 проверяет все четыре инструмента в первом запросе к провайдеру на 20 холодных стартах. `failed` даёт необязательную диагностику `mcp_unavailable`.

Сверка по проекциям (`reconcile.rs`). Replay у сервера нет: `/api/event` не умеет `Last-Event-ID`, `/log` на CLI пуст, поэтому один путь обслуживает разрыв потока, потерянный слишком большой кадр и запуск после падения. Подписка идёт раньше чтения: событие либо придёт живым, либо уже видно в проекции. Читаются `/api/session/active`, `.../inbox`, список потомков `GET /api/session?parentID=`, потому что потомок мог родиться в разрыве, `.../permission` и `.../form` своей сессии и каждого потомка, затем хвост `.../message` от новых к старым до второго `idle`; страница больше потолка ответа читается мельче. Сообщения проходят теми же обработчиками, что живые события, с теми же идентификаторами блоков (`ordinal` части — её номер среди частей того же вида), а закрытые блоки и посчитанные шаги пропускаются, поэтому поздний живой кадр ничего не задваивает. Пока идёт сверка, новый поток ждёт, чтобы конец хода не обогнал потерянное. Дельты открытого в разрыве блока отбрасываются: остаток текста приходит одной дельтой перед финалом, и хаб сходится с контрольной суммой. Запросы из проекции открывают карточку тем же путём, открытая карточка, которой на сервере уже нет, закрывается с `cli_cancelled`. Сервер не работает, а ввод уже стал сообщением: ход закрывается итогом последнего `idle`, а терминал того же периода, если он ещё шёл по новому потоку, отбрасывается до следующего `session.execution.started`. Ввод в очереди или ещё не дошедший до снимка не закрывается. Подписка заново идёт с паузами 0, 200 мс, 1 с и 3 с; ввод в это время не ждёт, потому что сервер жив. Если подписаться не вышло, остаётся прежний путь: `event_stream_lost`, SIGTERM, SIGKILL и `ProcessExited`.

Перезапуск воркера адаптера не касается: сопровождение живёт дальше, журнал докатывается. Смерть супервизора закрывает stdin, сервер уходит сам и успевает дописать проекцию: текст остаётся, инструмент становится `error` с `aborted`. SIGKILL сервера оставляет текст пустым, инструмент в `running`, а его `shell` живыми. Новое исполнение с той же сессией читает те же проекции: блоки приходят с прежними идентификаторами, и хаб отбрасывает уже известные, а новые принимает без привязки к вводу, потому что команда принадлежала старому исполнению; результат садится под вызов прежнего исполнения, поэтому родитель блока проверяется в пределах разговора. Инструмент в `running` без работы сервера закрывается как `interrupted`, настоящий `session.tool.failed` следующего ввода уже ничего не меняет. Ввод, оставшийся в `.../inbox`, снимается `DELETE` с блоком-предупреждением: хаб считает его неопределённым, а следующий ввод исполнил бы его вместе с собой. Расход прерванного хода никому не приписывается. Сироты: пока жив супервизор, `shell` убитого сервера остаётся в его дереве через subreaper и снимается обычным путём; на случай смерти обоих после каждого `shell.created` снимок `(pid, время старта)` из `GET /api/shell` пишется в `<data_root>/shells/<сессия>.json`, а новый запуск снимает эти группы раньше старта сервера, сверяя время старта.

Инструкции. «Как у владельца» запускает сервер с настоящим `XDG_CONFIG_HOME` без `OPENCODE_CONFIG_DIR`. «Без инструкций» и пресет получают `OPENCODE_CONFIG_DIR=<каталог исполнения>/opencode-config` и `OPENCODE_DISABLE_CLAUDE_CODE=1`: в каталоге ссылки на всё содержимое каталога владельца, кроме его `AGENTS.md`, поэтому читаются и `opencode.json`, и `opencode.jsonc`, и плагины; `OPENCODE_CONFIG` не подходит, он принимает один файл. Текст пресета лежит в `AGENTS.md` этого каталога с правами `0600`, подменяется целиком. Смена между «Без инструкций» и пресетами не перезапускает сервер: он следит за файлом, а модель получает `<system-update>` на следующем шаге; после записи адаптер ждёт 400 мс, потому что читает файл наблюдатель с дебаунсом 100 мс и публичного сигнала нет. Переход на «Как у владельца» и обратно меняет окружение запуска, поэтому перезапускает сервер с той же сессией (`CliRestarted`), фоновые команды сессии при этом останавливаются; неудачный запуск возвращает прежнюю конфигурацию и отклоняет ввод с `instructions_change_failed`. Отклонённый ввод возвращает прежний текст перед следующим вводом. Разрыв относительно `pilot.md` §4 принят: `~/AGENTS.md` и `AGENTS.md` выше корня репозитория читаются, если рабочий каталог лежит под домом.

Субагент это дочерняя сессия. Её блоки идут в дорожку вызова `subagent` с именем агента из `session.created` и `session.step.started`, расход считается под этим именем, контекст разговора и фазу задаёт только основной агент. Связь с вызовом приходит в `session.tool.progress` раньше первого ввода потомка, после разрыва её даёт `metadata` результата вызова; блоки потомка без связи всё равно показываются. Фоновый потомок работает и между ходами родителя: его блоки приписаны последней команде. Блоки потомка, целиком выпавшие в разрыв потока, не восстанавливаются, проекция читается только у корневой сессии. Список задач включён всегда (`experimental.todo`) и приходит из `metadata.todos` результата `todowrite`, со статусом `cancelled`.

Отказы. `provider.quota` и `provider.free-limit` дают `LimitReached` внутри хода со сбросом из `retryAfterMs`: окна в отказе нет, ожидание длиннее пяти часов считается недельным, месячного окна и опроса нет. `provider.auth` останавливает исполнение с причиной `auth_required`. Переполнение OpenCode чинит сам, поэтому дошедшее до конца хода значит, что чинить нечем: `ContextExhausted`; тип ошибки его не отличает, признак берётся из шаблонов версии 2.1.4. Запрос к модели при `session.retry.scheduled` повторяет сервер, адаптер ввод заново не шлёт, в строке активности видна попытка. Один и тот же `provider.invalid-request` дважды подряд значит застрявшую сессию: причина такого отказа не установлена, поэтому это не `ContextExhausted`. Запись `SessionStuck` (возможность `compact_v1`) ставит очередь на паузу `session_stuck`, карточка предлагает `POST /api/v1/conversations/{id}/compact`: команда `compact_session` доходит до `POST .../compact` с идентификатором из команды. Сжатие тратит токены, поэтому оно только по действию владельца, во время хода не начинается, удача снимает паузу. Та же кнопка «Сжать контекст» живёт в индикаторе контекста у поля ввода (в два нажатия) и доступна между ходами живого исполнения без паузы: Claude Code сжимает вводом `/compact`, OpenCode — `POST .../compact` своего сервера, Codex — методом `thread/compact/start` своего app-server. Каждое сжатие — тихая строка ленты (`compaction_started`/`compaction_finished`, переживает перезагрузку), живой расход в ходу идёт записью `ContextProgress`. Вид каждого отказа и шаг отказа запуска видны в листе диагностики, без текста разговора.

Вход. База canterlot своя и учётных данных владельца не видит, поэтому вход делается один раз в её каталог, до включения таблицы и без живых исполнений OpenCode:

```bash
data=/home/user/.local/state/canterlot-worker/opencode # data_root, по умолчанию <data_dir>/opencode.
XDG_DATA_HOME="$data/data" XDG_STATE_HOME="$data/state" XDG_CACHE_HOME="$data/cache" \
  opencode auth login --standalone
```

`--standalone` обязателен: без него команда идёт через фоновую службу, а служба на этой базе подхватила бы живые исполнения как осиротевшие. Проверка: те же переменные и `opencode auth list --standalone`.

С feature `synthetic-adapter` собирается `fake-opencode`: настоящий HTTP и SSE на loopback по сценарию `--script`, собранному из записей `docs/stage0/evidence/opencode` (`adapter::opencode::replay`). Запуск и тесты идут через тот же путь, что у настоящего CLI: `--version`, `serve --stdio --port 0`, пароль, строка готовности, выход по EOF stdin и код 130 по SIGTERM.

Тесты с настоящим `opencode` и локальной заглушкой провайдера собираются с feature `opencode-live`, помечены `ignore` и в `check.sh` не входят, хотя тот собирает `--all-features`. Каталоги, `HOME` и конфигурация изолированы, запросов к внешним моделям нет:

```bash
cargo test -p canterlot-worker --features opencode-live --test opencode_live -- --ignored --test-threads=1
```

Изоляцию MCP делегированных сессий (D24) на настоящих `claude` и `codex` проверяет `delegated_mcp_live.rs`: поддельный дом владельца с сервером `nanokvm`, входа в аккаунт и платных запросов нет, настоящие `~/.claude` и `~/.codex` не открываются. Для OpenCode то же делает тест `delegated_session_never_starts_owner_or_project_servers` в `opencode_live.rs`.

```bash
cargo test -p canterlot-worker --test delegated_mcp_live -- --ignored --test-threads=1
```

При обновлении CLI схема работающего сервера сверяется с манифестом этапа 0; каталоги изолированы, запросов к модели нет:

```bash
scripts/opencode-schema-check.sh
```

OpenCode входит в работу после пилота, когда пройдёт смоук на настоящей модели: без таблицы `[opencode]` ничего не меняется.

Сопровождение добавляет свой MCP-сервер для разрешений, вопросов, чтения лимитов и работы с пресетами: `canterlot_presets_list`, `canterlot_preset_save` и `canterlot_preset_apply` вызывают хаб унарным `WorkerData.AgentCall` и действуют только на свою сессию (у Codex набор состоит из лимитов и пресетов, `approve` нет). SIGINT завершает ход и процесс CLI с исходом `user_stop`; продолжение запускает новое исполнение с `--resume` и тем же нативным ID. Отсутствие авторизации даёт `auth_required`, несовпадение ID или несовместимая версия дают `cli_incompatible`, недоступность canterlot MCP даёт `start_failed`.

Фоновая работа Claude Code. Адаптер читает `system/background_tasks_changed`, `task_started` и `task_notification` и шлёт полный список работающих задач, ключ — `task_id`: завершившийся субагент может запуститься снова. Хаб хранит задачи таблицей, число работающих идёт в доказательство `idle`. Ход, который CLI начал сам по завершении задачи, адаптер приписывает последней команде (`RunAdopted`, execution.md §2). Смена инструкций, для которой нужен перезапуск CLI, ждёт конца фоновой работы: сообщение уходит сразу со старыми инструкциями, в ленте появляется предупреждение, а перезапуск происходит между ходами, когда список задач пуст две секунды, иначе он оборвал бы начинающийся пробуждающий ход. Модель и режим разрешений из той же смены этого не ждут: они применяются перед сообщением, а режим, который CLI без перезапуска принять не может, отклоняет сообщение и показывается в настройках как не применённый. Явная остановка владельца работает сразу.

Воркер запускает Claude Code с `CLAUDE_CODE_ENABLE_TODO_TOOLS=true` и `CLAUDE_CODE_ENABLE_TASKS=false`: без первого CLI 2.1.311 прячет список задач от моделей новее `claude-*-4-*`, второй оставляет `TodoWrite` с полным списком (pilot.md §12.2).

Обычные тесты используют поддельный CLI и записанные кадры, без настоящих запросов. Владелец запускает live smoke отдельно: около дюжины коротких ходов Haiku, усилие `low`, временный git-репозиторий, пресет «Как у владельца» (`owner`), режим `ask`. Проверяются текст, окна лимитов из `rate_limit_event` и ответ `canterlot_limits`, разрешение Bash на команду записи (команды чтения вроде `printf` или `sleep` CLI разрешает сам, не спрашивая), вопрос, чтение PNG, `set_model`, SIGINT во время долгой команды на переднем плане и resume. Затем идёт проверка пресетов в репозитории под `$HOME`: `/context` в «Без инструкций» без автопамяти и с `CLAUDE.md` репозитория, две канарейки текста пресета при смене между ходами, возврат к «Без инструкций» и отчёт `cache_read_input_tokens` на неизменных ходах. При успехе тест печатает подтверждённые факты; открытые вопросы плана закрываются только после этого запуска.

```bash
CANTERLOT_CLAUDE_CODE_SMOKE=1 cargo test -p canterlot-worker --all-features --test claude_code_e2e -- --nocapture
```

Для нестандартного пути CLI добавьте `CANTERLOT_CLAUDE_CODE_EXECUTABLE=/absolute/path/to/claude`; без этой переменной используется `claude` из `PATH`. Без флага smoke пропускается. Не запускайте его в обычном CI.

Лимиты Claude Code. Адаптер переводит `rate_limit_event` в показания окон (`utilization` × 1000 в промилле, `resetsAt` из секунд в миллисекунды). Настоящий поток присылает только одно окно: то, что CLI считает определяющим (чаще недельное), поэтому пятичасовое окно из потока приходит редко. Окно со `status: "rejected"` или ход, упавший с 429 либо `usage_limit_reached`, дают упор в лимит: окно из `rateLimitType` (иначе самое загруженное), время сброса из `rate_limit_info` кадра ошибки, иначе из последнего события; повтор того же упора в одном ходе второго факта не даёт. 429, который CLI повторяет сам (`system/api_retry`), упором не считается.

Лимиты без живой сессии воркер берёт у CodexBar: `codexbar usage --provider claude|codex|opencodego --format json --no-color` (для OpenCode запуск идёт с `opencodego`, показания хранятся как провайдер `opencode`). Запрос к модели не делается, учётные данные читает сам CodexBar, воркер файлов с ключами не открывает. Исполняемый файл ищется в `[machine].codexbar_path`, затем в `PATH` и в `~/.local/bin`; интервал опроса `[machine].limits_poll_interval` (по умолчанию 15 минут, это же нижняя граница: CodexBar ходит в ограниченную по частоте точку usage), первый опрос через минуту после запуска. После сбоя или пустого чтения пауза удваивается от интервала и доходит до часа; первый удачный опрос её снимает. Запуск ограничен 20 секундами и 1 МиБ вывода, повторный запуск раньше 10 секунд не идёт. Окна определяются по `usage.*.windowMinutes` (300 это пятичасовое, 10080 недельное), а не по именам `primary` и `secondary`: у Claude без пятичасового окна `primary` повторяет недельное. Окно без `windowMinutes` из этих двух, месячное окно 43200 и дополнительные окна (`extraRateWindows`, например по модели) пропускаются, `usedPercent` × 10 даёт промилле, `resetsAt` читается как ISO 8601, время показания равно `usage.updatedAt`, но не позже часов воркера. Полное успешное чтение, где есть другие окна провайдера, но нет строки на 300 минут, воркер отправляет хабу как отсутствие пятичасового окна (`LimitsReport.absent`): так у плана без пятичасового лимита (например, Claude Ultra с 30 октября 2026) счётчик 5ч не показывается нигде. Хаб хранит отметку `provider_limits.five_hour_absent_at`, не трогает историю снимков, не ставит из-за этого окна паузы и подтверждает сброс пятичасового удержания «кранча» отчётом об отсутствии; более позднее показание окна снимает отметку само. Неполное или неудавшееся чтение окно не скрывает: строка на 300 минут без значения, пустой вывод и сбой запуска ничего не отмечают. Оставшиеся значения окна без отметки, у которых сброс прошёл или показание старше порога, остаются «устарело». Если CodexBar нет, он завершился с ошибкой или вывод непонятен, для Codex используется чтение `account/rateLimits/read` через `app-server`, в том числе на время паузы после сбоев, для Claude остаются показания живых сессий; причина пишется в журнал предупреждением `codexbar_poll_failed` один раз, восстановление отмечает `codexbar_poll_recovered`. Проверка пробуждения «кранча» (`PollLimits`) соблюдает тот же интервал и ту же паузу. Интервал и паузу пропускает только кнопка обновления в PWA (`POST /api/v1/limits/refresh`, `PollLimits.owner_refresh`), у неё свой предел: хаб 15 секунд на провайдера, воркер 10 секунд между запусками. `allow_paid_model_poll = true` (платный запрос к модели ради лимитов) в пилоте не поддерживается: воркер не запускается с этой настройкой.

Таблица `[execution]` необязательна и так же строгая. `allowed_workdir_roots` — каталоги,
внутри которых допустим рабочий каталог исполнения (сравнение по каноническому пути);
пустой список отклоняет каждый `start_execution` с причиной `workdir_not_allowed`. Значение `["/"]`
разрешает любой абсолютный путь; пути с `..` и относительные отклоняются в любом случае.
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
`canterlot-worker supervise` — служебная команда: запускается самим воркером для каждого исполнения и вручную не вызывается.
`canterlot-worker synthetic-cli` — скрытый тестовый помощник, доступен только с feature `synthetic-adapter`.

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

Чтения: `GET /api/v1/snapshot/conversations`, `GET /api/v1/snapshot/machines`, `GET /api/v1/conversations/{id}/snapshot`, `/history`, `/blocks/{block_id}`, `/runs/{run_id}`, `/delegation`. Снимок и его курсор читаются в одной `REPEATABLE READ` транзакции; `GET /api/v1/events?scope=&after=&limit=` восстанавливает хвост с сохранением номеров внутренних событий через `hidden`. При устаревшем или опережающем курсоре нужен новый снимок. Снимок разговора отдаёт в `open_requests` все незавершённые запросы (`open`, `decision_saved`, `decision_transmitting`); при переходе решения к передаче агенту хаб выпускает событие `request_transmitting`. Подробные поля и ограничения: `docs/design/api.md`; схема событий: `docs/api/events.schema.json`. Новых таблиц конфигурации в задаче 7a нет: действуют `[http]` и пороги `[machines]`.

## Обновление контракта пилота

Порядок обновления: хаб первым, затем воркер. Возможность `todo_v1` разрешает запись `TodoUpdated`: со старым хабом воркер отбрасывает список задач, ход не страдает. Возможность `background_v1` так же разрешает `BackgroundWorkChanged`. Возможность `pilot_v1` согласуется через `Hello.optional_capabilities` и `Accepted.capabilities`; до её подтверждения записи и команды пилота не используются. Возможность `session_tools_v1` разрешает рабочие копии (`StartExecution.worktree`, `RemoveWorktree`, `WorktreeRemoved`), факты git в `RunFinished` и признак `isolate_mcp` делегированных сессий; миграция `0035_session_tools` выполняется внутри `serve`, поэтому хаб обновляется первым (неизвестная запись `RecordBody` останавливает журнал, `streams.md` §3), затем PWA и воркер. Со старым воркером команды с этой возможностью остаются ожидающими, изолированный запуск отвечает `capability_missing`, а делегированная сессия ждёт обновления воркера и ни при каких условиях не стартует с серверами владельца; обычные сессии не меняются. Работающие CLI сохраняют прежний список инструментов и инструкции до перезапуска. Возможность `owner_exit_v1` так же требует хаб первым: без неё команда `resolve_owner_lost` остаётся ожидающей. Откат хаба ниже возможности, которую воркер уже использовал, не поддерживается; после подключения воркера откат через миграцию 0035 невозможен. Если обновление всё же разошлось и хаб не может применить запись журнала, запись не теряется: она остаётся на машине неподтверждённой, а на экране машины появляется предупреждение `cj_journal_stopped` или `sj_journal_stopped`. Журнал команд держит всю машину, журнал сессии только свою сессию. Обновление хаба или воркера снимает стоп само, ручных действий не требуется; в логах искать `journal_stopped` у хаба и `journal_refused` у воркера. Обновление управляющей службы воркера сохраняет живые процессы сопровождения и их исполнения.

Фоновые службы (`singleton.rs`) восстанавливают отдельное соединение и advisory lock после сбоя с задержкой 250 мс → 30 с, которая сбрасывается после 60 с непрерывной работы; второй хаб остаётся пассивным. `readyz` сообщает имена недоступных служб (`reason: service`, `services`). Планировщик считает ошибки отдельно от опросов и обрабатывает задания независимо. Временные ошибки пробуждений и проверок лимита повторяются неограниченно с задержкой не более 60 с; для остальных заданий действует `max_attempts`. Ежечасное обслуживание удаляет завершённые и ошибочные задания через 30 дней, общие ключи идемпотентности и события через 7 дней. Ключ отправки ввода и хеш запроса сохраняются в самом вводе: поздний повтор после очистки возвращает исходный ответ. Новый лимит получает собственный срок автопродолжения; поздний факт прерванного владельцем хода его не включает. Подробности восстановления, срок автоматического продолжения и форматы событий: [контракт API](docs/design/api.md#восстановление-фоновых-служб-и-обслуживание).


## PWA

Нужны Node.js 24 LTS и npm. Версии зависимостей закреплены точно в `web/package.json`, дерево в `web/package-lock.json`. В `web/src/lib` находятся типизированный API, синхронизация, журнал операций, черновики, докачка вложений, список сессий и чат. Экраны `screens/machines`, `screens/limits`, `screens/settings`, `screens/share` показывают живые метрики, историю двух окон квот за семь местных дней, настройки устройства и выбор сессии для файлов Android. Платформа `pwa/` добавляет установку, обновление по `_app/version.json` и очистку; service worker, offline-оболочки и push нет. Сборка и инструменты работают без обращения к внешним сервисам во время выполнения.

Рендеринг (`web/src/lib/render`): безопасный потоковый Markdown (markdown-it без сырого HTML, замена изображений на ссылки, строгий allow-list DOMPurify, открытие ссылок в новой вкладке с noopener/noreferrer/nofollow), кеширование закрытых блоков, plain-хвост свыше 8 КиБ, ленивая подсветка highlight.js и очистка ANSI.

Синхронизация (`web/src/lib/sync`): один `ConnectionManager` держит единственное WSS-соединение с подпротоколом `canterlot.v1` и следит за областями подписок (до 16). После обрыва каждая область сохраняет свой последний применённый курсор и возобновляется с него на новом сокете; пробелы в курсоре заполняются HTTP-запросами диапазона порциями до 256 событий, которые применяются по порядку, а буфер свыше 5 000 событий или 8 МиБ переводит область к пересинхронизации: unsubscribe, снимок с границей, подписка с неё. Вторая пересинхронизация за 60 секунд ждёт backoff (250 мс × 2ⁿ с джиттером, предел 15 секунд), третья ставит область в `degraded` с повтором раз в 30 секунд. На видимой странице соединение пингуется каждые 25 секунд; отсутствие pong за 5 секунд или любых кадров 45 секунд пересоздаёт сокет, а после пробуждения страницы пробный ping с дедлайном 2 секунды заменяет зависший сокет. Событие `hidden` двигает курсор без изменения состояния, крупное событие `oversized` догружается отдельным запросом диапазона из одного события до сдвига курсора. Подробности поведения: [Синхронизация в docs/design/pwa.md](docs/design/pwa.md#синхронизация).

Task 4 держит состояние сессии в `web/src/lib/stores`: текст блока сходится по ревизиям и UTF-8-смещениям, ввод встаёт в ленту по `feed_seq`, список и машины читают свои снимки. Task 5 рисует ленту в `web/src/lib/timeline`: в DOM только окно строк, прокрутка держит якорь, карточка ответа собирает текст, инструменты и субагентов, полоса очереди стоит отдельно от живой строки.

Логика поля ввода и очереди (`web/src/lib/composer`, `web/src/lib/timeline/QueueActions.ts`, Task 6a): мобильные правила Enter без случайных отправок (на сенсорном экране Enter переносит строку, IME-композиция игнорируется), затвор отправки с русскими причинами, сохранение черновиков, отложенная отправка по окну хаба и действия очереди. Само поле (`Composer.svelte` на экране чата, Task 6b) ставит капсулу, «Отправить позже» (лист на телефоне, поповер у кнопки с часами на столе), лоток вложений и пилюлю прерывания; оно не подписано на строки ленты.

Оболочка и список (`web/src/lib/shell`, `web/src/lib/screens/sessions`): телефон показывает один экран с нижней навигацией, планшет список и чат, стол добавляет панель поручений. Список группирует сессии, ищет на сервере и показывает полосу лимитов: по строке на провайдера, два счётчика «5ч» и «нед» с тонкой полосой и остатком в процентах (полная полоска означает 100%), цвет по остатку (предупреждение ниже 20%, тревога ниже 5%), метка «резерв» для недельного резерва. В раскрытом виде рядом со счётчиками время сброса, строка «обновлено» и кнопка обновления (`POST /api/v1/limits/refresh`, ожидание нового показания до 10 секунд). Счётчик окна, которого у провайдера нет (`five_hour_absent`), не рисуется ни в полосе, ни в шапке чата, ни на вкладке «Лимиты» (график прячет 5ч, когда в показанных днях нет снимков этого окна). Окно, сброс которого прошёл, или показание старше `[limits].stale_after` (35 минут по умолчанию: семь пропущенных опросов при интервале 5 минут) гаснет и подписывается «устарело». Та же полоса стоит в шапке чата и берёт данные из хранилища списка при каждом обновлении, а вкладка «Лимиты» и график показывают остаток, а не занятую долю. Новая сессия создаётся отдельным запросом с тем же ключом при повторе. Форма сама выбирает единственную машину, подставляет домашний каталог воркера и первую модель каталога, а при вводе пути подсказывает существующие каталоги машины (`WorkdirField.svelte`, `workdirSuggest.ts`); подсказки необязательны и форму не задерживают.

Шапка чата и управление сессией (`web/src/lib/screens/chat`, Task 8a): шапка чата (`ChatHeader`) с заголовком, цветной меткой сессии, строкой статуса (`StateChip`) и индикатором отложенной смены модели; меню управления (остановка с подтверждением принудительного прерывания, одно действие запуска: «Запустить» без записанного хода, «Возобновить» с ним; хаб сам решает, продолжать ли беседу); настройки посреди сессии (`SessionSettings`) с валидацией каталога машины и переключателем «кранча»; карточки пауз и удержаний (`PauseCards`, `pauseCards.ts`), включая переопределение недельного лимита (`spend-reserve`); подписи значений протокола собраны в `web/src/lib/format/labels.ts`, а сырое значение на экран не попадает; карточки запросов агента и защищённая диагностика сессии (`Diagnostics`).

Запросы агента и разрешения (`web/src/lib/requests`, `stores/requests.svelte.ts`, Task 7, Task 8): интерактивные карточки вопросов (`QuestionCard.svelte`) и разрешений (`PermissionCard.svelte`), встроенные над полем ввода через `RequestCard.svelte`. Отправка решения создаёт операцию в `OpJournal`, карточка проходит этапы «отправка → сохранено хабом → передано агенту → применено» на `StageTrack`, при конфликте решений от другого устройства показывает предупреждение и отключает ввод (AT-D-01), а после применения сворачивается в компактную строку с возможностью разворачивания по клику.

Сведения о ходе, экспорт и ленты субагентов (`web/src/lib/screens/chat`, `web/src/lib/export`, Task 8b): лист «Об ответе» (`InfoSheet`, `info.ts`) с моделями по частям, расходом токенов (основной + субагент), длительностью, контекстом, честной оценкой лимита и автоматическим обновлением по `run_finished`; экспорт в Markdown (`export/markdown.ts`, текст основного ответа, вызовы инструментов в виде кратких строк, исключение субагентов) с резервным полем при отказе буфера; экспорт в изображение (`export/image.ts`, `export/deliver.ts`) в теме по токенам с делением на страницы до 8 192 px и отправкой через Web Share или буфер/файлы; отдельная лента субагента (`Lane.svelte`, `lane.ts`, маршрут `/c/[id]/lane/[blockId]`) с карточкой задачи, единой лентой и возвратом в основную сессию; панель делегирования рабочего стола (`DelegationPanel.svelte`) с деревом поручений и живыми статусами.

### Модули `web/src/lib`

- `api/` — типизированный клиент OpenAPI (`client.ts`), HTTP-транспорт (`http.ts`), разбор и классификация ошибок (`errors.ts`), конфигурация и шлюз авторизации (`gate.ts`), сгенерированные схемы (`generated/`).
- `a11y/` — единый live region скринридера `announce.ts` (`aria-live="polite"`, `role="status"`) для озвучивания навигации и событий без дублирующих анонсеров.
- `app/` — корень композиции приложения (`runtime.ts`), связывающий хранилища, менеджер WSS, транспорт, черновики и загрузку вложений.
- `composer/` — компонент и модель поля ввода (`Composer.svelte`, `model.svelte.ts`), правила Enter на телефоне и десктопе, лоток вложений (`AttachmentTray.svelte`), лист отложенной отправки (`SendLater.svelte`) и кнопка прерывания (`InterruptButton.svelte`).
- `drafts/` — сохранение черновиков ввода (`store.ts`, `DraftStore`) с транзакционной записью в IndexedDB и синхронным зеркалом в `localStorage`.
- `export/` — экспорт ответа: markdown (`markdown.ts`), картинка по страницам (`image.ts`) и доставка через «Поделиться», буфер обмена или скачивание (`deliver.ts`).
- `format/` — русскоязычные форматтеры дат, относительного времени, длительностей и размеров байтов (`ru.ts`).
- `ops/` — журнал клиентских операций `OpJournal` (`journal.ts`, `op.svelte.ts`) и сборка HTTP-запроса операции с ключом повтора (`request.ts`).
- `perf/` — опциональный легковесный замер производительности и журнал метрик (`probe.ts`), выключенный по умолчанию.
- `pwa/` — платформа PWA: установка (`install.ts`), обновление по `_app/version.json` (`update.ts`), подключение контроллера (`register.ts`), время последней синхронизации (`sync-state.ts`), приём «Поделиться» (`share.ts`) и очистка при выходе (`signout.ts`).
- `render/` — безопасный потоковый рендеринг Markdown (`markdown.ts`, `streamingMarkdown.ts`), подсветка синтаксиса highlight.js (`highlight.ts`), очистка ANSI (`ansi.ts`) и санитайзинг DOMPurify (`sanitize.ts`, `Html.svelte`).
- `requests/` — карточки вопросов и разрешений (`QuestionCard.svelte`, `PermissionCard.svelte`, `RequestCard.svelte`), этапы `StageTrack` и валидация ввода.
- `screens/` — экраны PWA; экран чата (`screens/chat/ChatScreen.svelte`) собирает шапку, ленту сообщений, панель задач агента (`TodoPanel.svelte`), поле ввода и боковые панели.
- `shell/` — оболочка приложения: раскладка телефон / планшет / десктоп (`AppShell.svelte`, `layout.ts`), навигация (`Nav.svelte`), баннеры связи (`ConnectionBanner.svelte`) и диалог истёкшей сессии (`SessionExpired.svelte`).
- `storage/` — изоляция хранилища IndexedDB по `owner_id` пользователя (`owner.ts`) и типизированная обёртка транзакций `idb.ts`.
- `stores/` — реактивные хранилища состояния Svelte 5 на рунах: чат и сессия (`chat.svelte.ts`), список задач агента (`todos.svelte.ts`), запросы агента и разрешения (`requests.svelte.ts`), список сессий (`list.svelte.ts`), машины (`machines.svelte.ts`), блоки ленты (`blocks.ts`), лимиты (`limits.ts`), вводы (`inputs.ts`).
- `sync/` — постоянное WSS-соединение `ConnectionManager` (`connection.ts`), подписки на скоупы (`scope.ts`), дочитка пропусков по HTTP (`transport.ts`, `batcher.ts`) и экспоненциальный backoff (`backoff.ts`).
- `timeline/` — виртуализированная лента сообщений (`Timeline.svelte`, `virtual.ts`, `rows.ts`), карточки ответов (`AnswerCard.svelte`), пузырь сообщения владельца с дорожкой доставки и вложениями (`UserBubble.svelte`, `AttachmentTile.svelte`), полоса очереди для ждущих ход или паузу (`PendingStrip.svelte`), управление якорем прокрутки (`scrollController.ts`) и действия очереди (`QueueActions.ts`).
- `ui/` — библиотека дизайн-компонентов на CSS-токенах: `Button`, `IconButton`, `Sheet`, `Disclosure`, `Pill`, `Segmented`, `StageTrack`, `TopBar`, `BottomNav`, `icons.ts`.
- `uploads/` — возобновляемая загрузка вложений частями (`manager.ts`, `uploader.ts`, `patch.ts`) с параллельным расчётом SHA-256 в веб-воркере (`hasher.ts`, `hash.worker.ts`).

### Вспомогательные скрипты

- `web/scripts/check-tokens.mjs` (`check-tokens`) — проверка стилей `web/src` на прямое использование hex-цветов, пикселей и таймингов в обход дизайн-токенов (`npm run lint:tokens`).
- `web/scripts/token-lint.mjs` (`token-lint`) — ядро линтера токенов на PostCSS и TypeScript: валидация CSS-переменных, длин, анимаций и запрет неразрешённых эмодзи.
- `web/scripts/eslint-rules.mjs` (`eslint-rules`) — локальное правило ESLint `iconLabel`, требующее обязательный непустой атрибут `label` у компонентов `IconButton`.
- `web/scripts/gen-api.mjs` (`gen-api`) — генерация TypeScript-типов из `docs/api/openapi.json` и схем событий `docs/api/events.schema.json` в `web/src/lib/api/generated/`.
- `web/scripts/kit-announcer.mjs` (`kit-announcer`) — препроцессор сборки Svelte, вырезающий встроенный элемент `svelte-announcer` SvelteKit для исключения конфликтов с `a11y/announce.ts`.
- `scripts/spdx.py` — токенизатор, парсер сложных выражений лицензий SPDX и валидатор зависимостей по списку `ALLOWLIST`.
- `scripts/test-licenses.py` — юнит-тесты парсинга SPDX-выражений и проверки допустимости лицензий проекта.

```bash
cd web
npm ci
npm run dev
npm run check
npm run lint
npm run lint:tokens
npm run test:unit
npm run gen:api
npm run build
npm run preview
```

`gen:api` читает только `docs/api/openapi.json` и `docs/api/events.schema.json`; типы HTTP, событий, ClientOp и ServerOp не пишутся вручную. `node scripts/gen-api.mjs --check` только сверяет закоммиченные типы с контрактом; `check-web.sh` запускает сверку всегда, а в режиме `--fast` — когда менялись `docs/api`, `src/lib/api/generated` или сам генератор. Dev proxy `/api` направляет запросы на `HUB_URL` (по умолчанию `http://127.0.0.1:8080`), добавляет `Remote-User` из `HUB_OWNER` (по умолчанию `owner`) и заменяет Origin на `HUB_ALLOWED_ORIGIN` (по умолчанию `http://localhost:5173`). Это локальный режим разработки; наружу Vite не публикуют. В статической production-сборке такого прокси нет.

```bash
cd web
PLAYWRIGHT_BROWSERS_PATH="${XDG_CACHE_HOME:-$HOME/.cache}/canterlot-playwright" npx --no-install playwright install chromium firefox
cd ..
bash scripts/check-web.sh
bash scripts/check.sh
```

`check-web.sh` ставит зависимости только при новом lock-файле (`npm ci`), затем параллельно гонит svelte-check без предупреждений, ESLint, token lint и Vitest (три четверти ядер) вместе с двумя production-сборками, после чего Playwright UI и parity (Chromium телефон, Firefox рабочий стол; воркеров половина ядер, `PW_WORKERS` меняет число; тесты в файле независимы, `fullyParallel`; проект `budget` выполняется изолированно после остальных; проекты `layout-1024`…`layout-3840` проверяют раскладку рабочего стола в Firefox вплоть до 3840×2160). Проверки доступности и визуального языка разложены по паре «экран, тема», хаос-тест синхронизации по шести файлам. `--fast` пропускает сборки и Playwright, кроме изменённых спеков, `--stack` добавляет сквозной набор. Для тестов включается `VITE_UI_TESTS=1`: отдельная страница `/__test/primitives`, отсутствующая в обычной production-сборке. Браузеры хранятся в `${XDG_CACHE_HOME:-$HOME/.cache}/canterlot-playwright`; можно задать другой `PLAYWRIGHT_BROWSERS_PATH`.

Поведение синхронизации проверяет FakeHub (`web/testing/fake-hub`): двойник исполняет тот же wire-контракт, проверяет события схемой Ajv, хаос-режимом меняет доставку событий и рвёт соединения, а `hub.wire` даёт два транспорта: in-memory с `FakeClock` для Vitest и Playwright-перехват `page.routeWebSocket` на `/api/v1/ws` и HTTP-обработчиков `/api/v1/**` для смока `web/e2e/ui/fake-hub-wire.spec.ts`. Одни только тесты синхронизации запускаются из каталога `web` командой `npm run test:unit -- tests/unit/sync`. `check-web.sh` настраивает параллельность под многоядерную систему: переменные `TOKIO_WORKER_THREADS`, `RAYON_NUM_THREADS` и `MOZ_TASKCONTROLLER_THREADCOUNT` выставлены в 4, у Firefox два Playwright worker'а, а Vitest использует до 6 worker'ов. В `check-web.sh` входят и проверки `web/e2e/parity`: axe на каждом экране в обеих темах, отсутствие горизонтальной прокрутки на 360 и 320 px и при тексте 200 %, ходы только с клавиатуры, инварианты визуального языка по вычисленным стилям и контактный лист одиннадцати состояний макетов. Бюджет сборки (`tests/unit/budget`) входит в Vitest: JS входного маршрута не больше 180 КиБ gzip, разметка, DOMPurify, экспорт картинкой и хеш-воркер лежат в отдельных ленивых фрагментах.

#### Сквозные тесты на реальном хабе и замеры

Сквозной набор поднимает настоящие `canterlot-hub` и `canterlot-worker` с синтетическим адаптером за кромкой `web/e2e/edge/serve.mjs`: кромка подставляет `Remote-User` и секрет прокси, вычищает присланные клиентом `Remote-*`, `X-Forwarded-*` и `X-Canterlot-Proxy`, проксирует `/api/*` и WSS и умеет рвать, глушить, задерживать и переписывать канал через отдельный управляющий порт. Глобальная подготовка (`web/e2e/stack/global-setup.ts`) создаёт отдельную базу в тестовом PostgreSQL, одноразовые CA и сертификат, конфигурации хаба и воркера, запускает процессы и кромку; завершение останавливает только запущенные ею процессы по PID, включая супервизоры исполнений, и удаляет базу и временные каталоги. Логи хаба, воркера и кромки лежат в `web/e2e/.stack/`.

```bash
# собственная тестовая база PostgreSQL (порт задаётся CANTERLOT_TEST_PG_PORT, по умолчанию общий 55440)
export CANTERLOT_TEST_PG_PORT=55442
bash scripts/test-db.sh up
export DATABASE_URL=postgres://canterlot:canterlot@127.0.0.1:$CANTERLOT_TEST_PG_PORT/canterlot

cd web
VITE_UI_TESTS=1 npm run build   # собранный клиент, который раздаёт кромка
npm run stack:build             # хаб и воркер с `--features canterlot-worker/synthetic-adapter`; не вместе с браузерами
npm run test:stack              # сквозной набор в Chromium (телефон) и Firefox (рабочий стол)
npm run test:perf               # замеры P-01…P-06 и P-13/P-14, только телефонный профиль
npm run test:parity             # доступность, рефлоу, визуальный язык, контактный лист
node scripts/contact-sheet.mjs  # только контактный лист: web/e2e/.contact/index.html
bash ../scripts/check-web.sh --stack   # весь цикл: проверки, UI, parity и сквозной набор
```

Cargo-команды, которые обращаются к базе, нужно запускать с теми же `CANTERLOT_TEST_PG_PORT` и `DATABASE_URL`: `.cargo/config.toml` по умолчанию указывает на общий сервер 55440. Рядом с контактным листом ложатся снимки наших экранов; если рядом есть каталог утверждённых макетов (`CANTERLOT_MOCKUPS`, по умолчанию `.superpowers/design/mockups` в корне репозитория), лист показывает макеты рядом и печатает справочное совпадение пикселей. Макеты не нужны ни сборке, ни проверкам.

Где лежат отчёты:

- `docs/perf/pwa-first-measurements.md` собирается заново после каждого прогона `npm run test:perf` из файлов `web/e2e/.stack/perf/*.json` (по файлу на замер, поэтому повтор одного замера обновляет только его строку). Это первые информационные числа с условиями и нагрузкой хоста; промах мимо цели в них записан, а не скрыт.
- `docs/perf/phone-acceptance.md` ручной чек-лист владельца для Samsung Galaxy S25 Ultra и Firefox; ни один пункт не отмечен пройденным.
- `web/e2e/.contact/index.html` контактный лист одиннадцати состояний макетов для просмотра человеком.
- `web/test-results/` следы и снимки упавших тестов Playwright.

Клиентские правила и архитектура PWA: [docs/design/pwa.md](docs/design/pwa.md).

### Статическая публикация

После `npm run build` Caddy обслуживает `web/build`, глубокие ссылки получают `200.html`. Весь origin защищён одним `forward_auth`, включая HTML, JS, manifest, API и WSS. Перед аутентификацией удаляются присланные клиентом `Remote-*`, `X-Forwarded-*` и `X-Canterlot-Proxy`; секрет к хабу добавляет только `header_up`. Заголовок CSP формируется из `web/csp.mjs` с точными hashes bootstrap-скрипта текущей сборки и единственным `wss://<origin-host>`:

```bash
node web/scripts/print-csp.mjs https://chat.example
```

Вывод задают в переменной окружения Caddy `CANTERLOT_CSP` при каждом обновлении сборки. Это обязательная часть публикации: `script-src` не содержит `unsafe-inline`; hashes CSP должны соответствовать `200.html`. `kit.csp` сохраняет `connect-src 'self'`, заголовок Caddy добавляет конкретный WSS origin. Пример ниже заменяет API-only пример из раздела HTTP для установки с PWA:

```caddyfile
chat.example {
    route {
        request_header -Remote-*
        request_header -X-Forwarded-*
        request_header -X-Canterlot-Proxy
        forward_auth authelia:9091 {
            uri /api/authz/forward-auth
            copy_headers Remote-User Remote-Groups Remote-Name Remote-Email
        }
        header Content-Security-Policy "{$CANTERLOT_CSP}"
        header X-Content-Type-Options "nosniff"
        @api path /api/* /healthz
        handle @api {
            reverse_proxy hub:8080 {
                header_up X-Canterlot-Proxy {file./run/secrets/canterlot-proxy}
            }
        }
        @share {
            method POST
            path /_share
        }
        handle @share {
            redir * /share?error=sw_inactive 303
        }
        handle {
            @fresh path /manifest.webmanifest
            header @fresh Cache-Control "no-cache"
            root * /srv/canterlot/web/build
            try_files {path} /200.html
            file_server
        }
    }
}
```

WSS проходит через тот же reverse_proxy. В `redir` матчер `*` обязателен: единственный аргумент, начинающийся с `/`, Caddy читает как матчер пути, а не как адрес перехода. `POST /_share` остаётся переходным ответом `303` на `/share?error=sw_inactive` для старых установок; manifest больше не объявляет share target, потому что системный приём «Поделиться» требует service worker с обработчиком fetch, а service worker не публикуется. Для manifest используется `crossorigin="use-credentials"` и MIME `application/manifest+json` с `Cache-Control: no-cache`. Приёмка настроенной публикации включает `scripts/check-proxy.sh` (параметры в разделе HTTP); хаб сохраняет проверку trusted_proxies, Origin и proxy_secret_file. Реальный деплой эта задача не выполняет.

### Установка и обновление

Manifest генерирует Vite из тёмного `--bg` в `tokens.css`. PNG уже лежат в `web/static/icons`; после изменения токенов или `source.svg` пересоздайте их командой `cd web && PLAYWRIGHT_BROWSERS_PATH=/tmp/canterlot-playwright node scripts/gen-icons.mjs`. Знак повторяет букву c и круглую точку состояния интерфейса; весь рисунок помещается в безопасный круг maskable-иконки. На Chromium пункт «Установить» появляется в меню «Ещё», когда браузер разрешает установку, и исчезает после использования. В Firefox установка остаётся возможностью браузера, собственного неработающего пункта нет.

Баннер «Доступна новая версия» появляется, когда `_app/version.json`, запрошенный с `cache: 'no-store'`, расходится с версией сборки, либо хаб сообщает о несовместимости схемы. Явное нажатие «Обновить» сохраняет черновики в IndexedDB и делает полную перезагрузку; HTML запрашивается без кэша. Неудачная запись черновика показывает причину и «Повторить». Автоматическое применение в скрытой вкладке ждёт безопасного момента, когда непустое поле больше не в фокусе и запись завершена. После выхода локальные данные и настройки владельца очищаются через `purgeLocal` («Настройки» → «Выйти», затем переход по `auth.logout_url` из `/config`). Offline-оболочки нет: без сети открывается только уже загруженная страница, а связь и данные показывает баннер WebSocket.

В меню «Ещё» доступны «Настройки»: логин, связь и последняя синхронизация, тема, Enter, сохраняемая скорость анимаций, установка и обновление. Очистка данных и выход сначала останавливают подписки и загрузки и удаляют локальные данные; при `auth.logout_url = null` экран объясняет завершение сессии на странице Authelia.

`bash scripts/check-web.sh` собирает также вторую версию в игнорируемый `web/build-update`, чтобы проверить обновление по `_app/version.json` и одну перезагрузку по кнопке.
