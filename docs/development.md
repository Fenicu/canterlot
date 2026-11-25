# Разработка и структура

Краткие правила для агентов, которые разрабатывают canterlot, — в [AGENTS.md](../AGENTS.md) (`CLAUDE.md` его импортирует); этот документ даёт подробности.

## Структура

| Путь | Назначение |
| --- | --- |
| `web/` | Статический клиент SvelteKit: токены, тема, примитивы, StageTrack, типизированный HTTP, журнал операций, черновики, загрузка вложений, синхронизация WSS в `src/lib/sync`, FakeHub в `testing/fake-hub`, лента чата, поле ввода, оболочка и список сессий, экраны машин, истории лимитов, настроек и истории изменений, платформа `src/lib/pwa` (установка, обновление по version.json, очистка), манифест и иконки приложения, unit и браузерные проверки |
| `crates/canterlot-journal/` | Локальный журнал команд и событий воркера |
| `crates/canterlot-proto/` | Типы protobuf, контракты gRPC и локального IPC управляющей службы с сопровождением |
| `crates/canterlot-worker/` | Воркер: TLS-сеанс, журналы, сопровождение исполнений, адаптеры и `machine/{metrics,limits}` |
| `crates/canterlot-hub/` | Хаб: PostgreSQL, gRPC-сервер, диспетчер, outbox, факты агента и данные машин |
| `crates/canterlot-hub/src/{runs,limits}.rs` | Ходы, расход по моделям и агентам, снимки лимитов и история |
| `crates/canterlot-hub/src/provider_status.rs` | Опрос страниц статуса Anthropic и OpenAI (statuspage), уровень по компонентам харнесса, хранение и событие `provider_status_changed` |
| `crates/canterlot-hub/src/{crunch.rs,http/crunch.rs}` | Правила лимитов: удержания «кранча», недельный резерв, пробуждение после сброса, опрос лимитов, `spend-reserve` |
| `crates/canterlot-hub/src/{machine_info,singleton}.rs` | Каталог и метрики машин, проекция доступности и блокировки фоновых служб |
| `crates/canterlot-hub/src/http/` | HTTP хаба на axum: вход по заголовку прокси, CSRF и Origin, ошибки, `healthz`/`readyz`, `me`, `config`, OpenAPI |
| `crates/canterlot-hub/src/{background,dberr}.rs` | Единое место запуска фоновых служб; безопасное описание ошибок базы данных |
| `crates/canterlot-hub/src/{activity,bus,scheduler}.rs` | Проекция активности и непрочитанного, шина уведомлений и транзакционный планировщик |
| `crates/canterlot-hub/src/rescue.rs`, `migrations/0116_hung_rescues.sql` | Спасение хода, зависшего в генерации: единое условие `activity::hung_turn`, окно наблюдения по выборкам CPU, эпизод по ходу и прогрессу, охранный interrupt, толчок автопинка теми же проверками, подтверждение квитанцией воркера, заметка родителю после фиксации, карточка с кнопками по эпизоду; таблица свидетельств по харнессам `evidence_policy` |
| `crates/canterlot-hub/src/{conversations,groups,preview}.rs` | Создание сессий, атрибуты, группы и их значки, список, превью последнего ответа и прочитанность |
| `crates/canterlot-hub/src/{search,http/search,session_tools/search}.rs` | Поиск по истории: ответы агента и сообщения ленты через частичные индексы GIN миграции 0075, `POST /api/v1/search` и инструмент `history_search` |
| `crates/canterlot-hub/src/{presets,http/presets,grpc/agent}.rs`, `migrations/0021_instruction_presets.sql`, `migrations/0022_conversation_presets.sql` | Пресеты инструкций: версии текста, CRUD API, события, снимок, стартовый пресет исполнителя и вызовы инструментов агента (`presets.list`, `presets.save`, `presets.apply`) |
| `crates/canterlot-worker/src/adapter/{canterlot_mcp/,codex/home.rs}` | MCP-сервер canterlot (разрешения, вопросы, лимиты, инструменты пресетов) и собственный `CODEX_HOME` для пресетов, кроме «Как у владельца», плюс изолированные дома делегированных сессий |
| `crates/canterlot-hub/src/http/{conversations,groups,machines,limits}.rs`, `migrations/0023_machine_workdirs.sql`, `migrations/0026_machine_home_dir.sql` | HTTP сессий и групп, чтение машин, лимитов и недавних каталогов: хаб запоминает до 20 проверенных каталогов на машину и отдаёт последние 3, домашний каталог воркера приходит в отчёте машины; лист «Как у владельца» читает глобальные файлы и память с живой машины (`GET /api/v1/machines/{id}/owner-instructions` и `.../file`, возможность `owner_instructions_v1`, в базе не кэшируется) |
| `crates/canterlot-hub/src/requests.rs`, `migrations/0018_requests.sql`, `migrations/0024_decision_unconfirmed.sql` | Запросы агента и разрешения: таблица requests (с состоянием `decision_unconfirmed`), первый ответ побеждает, команда `deliver_decision` через outbox, `POST /api/v1/conversations/{id}/requests/{id}/decision`, события `request_opened`, `request_decided`, `request_transmitting`, `request_closed`, удержание очереди и активность `WaitingForUser` |
| `crates/canterlot-hub/src/todos.rs`, `migrations/0028_agent_todos.sql` | Список задач агента: последний полный список каждой дорожки, отсечение устаревших ревизий, снимок и событие `todo_updated` (pilot.md §12.2) |
| `crates/canterlot-hub/src/background_tasks.rs`, `migrations/0033_background_tasks.sql` | Фоновые задачи агента: последнее состояние каждой задачи по `(conversation_id, task_id)`, связь с блоком вызова, отсечение устаревшего, признак хода, начатого самим CLI, в `executions.wake_turn` |
| `crates/canterlot-hub/migrations/{0019_blocks_approved_by,0025_blocks_approved_by_check}.sql` | Отметка блока `approved_by`: единственное допустимое значение `cli_policy` (действие разрешено политикой самого CLI без запроса), прочие при накате обнуляются, остальное отсекает CHECK |
| `crates/canterlot-hub/src/deletion.rs`, `migrations/0027_conversation_tombstones.sql` | Удаление остановленной сессии одной транзакцией: надгробие без содержимого, скрытые заглушки в журналах владельца, освобождение файлов после commit, подтверждение поздних записей журнала без применения; на машине остаются журналы, копии вложений и история CLI |
| `crates/canterlot-worker/src/exec/{group,lost}.rs`, `crates/canterlot-hub/migrations/0037_resolve_owner_lost.sql` | Выход из `owner_lost`: сверка группы после смерти сопровождения (сессия, метка в окружении, снимок `group.json`), доотправка его журнала, `failed(supervisor_died)` и `stopped(machine_rebooted)`, решение владельца `POST /api/v1/conversations/{id}/resolve-owner-lost` (возможность `owner_exit_v1`) |
| `crates/canterlot-hub/src/compactions.rs`, `migrations/{0038_compaction_timeline,0039_close_open_compactions}.sql` | Сжатие контекста строками ленты `compaction`, закрытие незавершённых у остановленных исполнений |
| `crates/canterlot-hub/migrations/0040_monthly_limits.sql` | Месячное окно лимитов (OpenCode Go) |
| `crates/canterlot-hub/migrations/0042_block_files.sql` | Файлы, отданные агентом через `share_file`: строка `block_files` и готовое вложение |
| `crates/canterlot-hub/migrations/{0043_run_working,0046_run_working_union}.sql` | Рабочее время хода: длительность без ожидания владельца (открытых запросов) внутри хода |
| `crates/canterlot-hub/migrations/{0044_steer_input,0045_input_revision}.sql` | «Отправить сейчас» (команда `steer_input`) и правка ввода в очереди по ревизии |
| `crates/canterlot-hub/migrations/{0041_queue_run_slot_since,0047_dispatch_claim}.sql` | Начало текущего хода для «Подвисла?» и граница выдачи: команду, выбранную outbox, больше не правят |
| `crates/canterlot-hub/src/session_tools/`, `crates/canterlot-hub/src/http/session_tools.rs`, `crates/canterlot-hub/migrations/0035_session_tools.sql` | Межсессионные инструменты модели (`pilot.md` §12): `settle` (устоявшееся состояние и отпечаток), `subs` (подписки), `jobs` и `summary` (событие и итог), `access`, `origin`, `read`, `send`, `spawn`, `whitelist`, `wait`, `accept`; белый список и приём результата по HTTP |
| `crates/canterlot-worker/src/worktree.rs`, `crates/canterlot-worker/src/supervisor/git_facts.rs` | Отдельная рабочая копия делегированной сессии (подготовка и уборка) и факты git на конец хода |
| `crates/canterlot-hub/src/session_tools/plan.rs`, `crates/canterlot-hub/migrations/0115_plan_tasks_archived.sql` | Архивация завершённых задач доски плана: `plan.archive` по номерам или сразу все, автоархивация старых и самых старых завершённых при нехватке места, компактный `plan_list` с `include_archived` и `number` |
| `crates/canterlot-worker/tests/it/session_tools_e2e.rs` | Сквозные сценарии межсессионных инструментов: запуск в копии, подписки, одобрение, приём, цикл и перезапуски |
| `crates/canterlot-hub/migrations/0020_midsession_effort.sql` | Признак `settings_midsession_effort` харнесса в каталоге машины: можно ли менять усилие у идущего исполнения (по умолчанию да; `false` присылают воркеры, чей харнесс меняет усилие только перезапуском) |
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
| `crates/canterlot-worker/tests/it/exec_media.rs` | Сквозные проверки картинок: ссылка только после публикации, перезапуски хаба и воркера, пропуск крупных и чужих файлов, независимость SJ и управления |
| `crates/canterlot-worker/tests/it/pilot_e2e.rs` | Сквозные сценарии пилота через HTTP и WSS: настоящий хаб, воркер, сопровождение и синтетический CLI |
| `crates/canterlot-hub/tests/it/contract_coverage.rs` | Заморозка контракта: таблицы `api.md` против OpenAPI и схемы событий, ключ идемпотентности у каждого изменяющего маршрута |
| `docs/api/openapi.json` | Машиночитаемый контракт API, генерируется `scripts/gen-openapi.sh` |
| `scripts/{gen-openapi,check-proxy}.sh` | Перегенерация OpenAPI; приёмка Caddy и собственного входа хаба |
| `docs/adr/0004-browser-auth-forward-auth.md` | Вход браузера через прокси с forward-auth (заменено ADR-0005) |
| `docs/adr/0005-owner-sign-in.md` | Собственный вход владельца: сеансы, пароль, восстановление, защита от перебора |
| `spikes/` | Прототипы этапа 0, не продуктовый код |
| `docs/stage0/` | Отчёты этапа 0: матрица возможностей, доказательства, ограничения |
| `crates/canterlot-proto/src/harness.rs` | Единая таблица харнессов и провайдеров |
| `docs/design/api.md` | Контракт API пилота: представление данных, ошибки, маршруты, виды событий, покрытие экранов |
| `docs/design/` | Автоматы состояний, правила идемпотентности и протоколы восстановления |
| `docs/adr/` | Решения по архитектуре и их обоснования |
| `docs/features.md` | Полный перечень возможностей интерфейса и инструментов агента |
| `docs/deploy/` | Развёртывание: пилотный контур (`pilot.md`), установка из готовых архивов (`release-archives.md`), образец значений стенда |
| `docs/screenshots/`, `web/e2e/screenshots/readme.spec.ts` | Картинки README и спека, которая их снимает на выдуманных данных |

## Тулчейн

- Rust 1.100.0 (закреплён в `rust-toolchain.toml`)
- protoc 36.2
- Node.js 24 LTS (сборка PWA)

## Разработка

Проверки идут через `cargo-nextest` (`scripts/install-tools.sh` ставит готовый бинарник; `check.sh` делает это сам). Три уровня:

- `scripts/check.sh --fast` — для небольшой правки и для помощников по ходу задачи: fmt, clippy со всеми фичами, тесты изменённых крейтов и тех, кто от них зависит (для изменённого файла `tests/it/x.rs` только модуль `x` в бинарнике-группе `it_<group>`, корень группы и `support/` — все группы), первая же ошибка останавливает прогон; в вебе svelte-check, eslint, токены и vitest по изменённым файлам. Сгенерированные типы API сверяются только когда менялся контракт или сами типы.
- `scripts/check.sh` — полная, перед слиянием в main: сравнивает с main и пропускает Rust, если менялись только web или документы, и web, если менялся только Rust; файлы `docs/api/*` (сгенерированные контракты), `scripts/*` и `.config/*` включают обе половины. `--all` отключает выбор по путям, `--base REF` меняет основу сравнения. Один clippy `--all-targets --all-features` идёт в отдельном каталоге `target/lint` одновременно с тестами; сборка без фич только проверяется `cargo check`, и из её тестов запускается единственный собственный: синтетический адаптер отключён. `canterlot-speech` исключён из прогонов `--all-features` (его фича `cuda` требует CUDA): его clippy и тесты идут отдельно, `-p canterlot-speech` без фич (transcribe.cpp на CPU, нужны `cmake` и C++), в `--fast` — только когда менялся крейт, `Cargo.lock` или `scripts/check.sh`. Тест с настоящими моделями помечен `#[ignore]`: `CANTERLOT_SPEECH_TEST_MODEL=<Parakeet GGUF> CANTERLOT_SPEECH_TEST_VAD=<Silero GGUF> [CANTERLOT_SPEECH_TEST_WAV=<запись>] cargo test -p canterlot-speech --release --test real_model -- --ignored --nocapture`.
- `scripts/check-web.sh` — сверка сгенерированных типов API, svelte-check, eslint, token lint, vitest и проверки собранных артефактов. Браузерные и сквозные тесты удалены 22.11.2026 по решению владельца; `--stack` принимается как no-op.

### Разовая проверка экрана в браузере

Обвязка Playwright осталась, хотя тестов больше нет: ей можно открыть любой экран в headless-браузере и снять снимок.

1. Соберите клиент: `cd web && npm run build`.
2. Браузеры лежат в `~/.cache/canterlot-playwright`; другой путь задаётся переменной `PLAYWRIGHT_BROWSERS_PATH`. Установка: `npx playwright install chromium firefox` (в `web/`).
3. Положите одноразовую спеку в `web/e2e/tmp/` (каталог не отслеживается git). Для вторичного экрана используйте `bootSecondary` из `e2e/ui/secondary-fixture.ts`, для любого экрана с готовыми данными — `bootScreen` и `SCREENS` из `e2e/parity/states.ts`.
4. Запустите её: `npx playwright test e2e/tmp/<name>.spec.ts --project=phone --workers=1` из `web/`.
5. Посмотрите снимок и удалите файл спеки.

### Скриншоты README

Картинки `docs/screenshots/*.png` снимает `web/e2e/screenshots/readme.spec.ts` на FakeHub с закреплёнными часами: `cd web && npm run build && npm run screenshots`. Кадры телефона — 390×844 при плотности 2, кадры стола — 1600×1000 при плотности 2 (от 1600 px ширины правая панель плана и делегатов открыта сама). Если установлен `oxipng`, спека сжимает PNG сама.

Данные в спеке выдуманы целиком: проект `lighthouse`, машины `workstation` и `mini`, каталог `/srv/lighthouse`. Настоящие имена хостов, адреса, пути домашних каталогов и идентификаторы в спеку и на картинки не попадают. После прогона откройте каждый PNG: текст не обрезан, нет индикаторов загрузки и случайных пустых состояний, размеры кадров одинаковы. Файл, на который README больше не ссылается, удаляется из репозитория.

### Обвязка выката, храповик и пул тестов

Обвязка выката (`scripts/deploy/tests/run.sh`: shellcheck, синтаксис python, статическая проверка pipe-lint, юнит-проверки `lib.sh` и наборы параллельно) идёт около 10 минут, поэтому в `--fast` она запускается только когда diff затронул `scripts/deploy/**`, `scripts/release.sh`, `scripts/check-proxy.sh`, `scripts/check-version.sh` или `scripts/lease.sh`; иначе печатается одна строка `deploy: skipped`. `scripts/gate.sh` запускает те же наборы отдельным шагом между `check.sh --all --no-web` и `check-web.sh`: они содержат тайминги и не должны идти рядом с тяжёлыми проверками. Diff для гейта берётся от последнего тега `vX.Y.Z`, а без тега наборы идут всегда. Аттестация гейта пишет `deploy_exit=0` или `deploy_exit=skipped`; `release.sh ship` принимает оба. Наборы быстрого пути выпуска без миграций — `fastpath`, `fastcutover`, `release-fast` (описание — §11.6 `docs/deploy/pilot.md`). Выбор проверяет `scripts/deploy/tests/test-changed.sh` (набор `changed`), а ограничить прогон можно `RUN_SUITES="…"`.

Храповик флейковых паттернов (`scripts/flaky-ratchet.py`) не пускает в тесты новые рискованные конструкции: сон (`tokio::time::sleep`, `std::thread::sleep`), замер времени (`Instant::now`, `SystemTime::now`; `Utc::now` для данных в базе не считается), голый `#[ignore]`, `page.waitForTimeout`, `setTimeout` в ожидании, `test.skip`/`test.fixme` без причины, `retries` без пометки `QUARANTINE` в вебе и `.config/nextest.toml`. Счётчики лежат по правилу и файлу в `scripts/flaky-ratchet.baseline.json`: рост в любом файле валит проверку, уменьшение только печатает подсказку. `scripts/check.sh` гоняет храповик вместе с его самотестами (`scripts/test-flaky-ratchet.py`) в быстром и полном режиме. База переписывается только явно: `python3 scripts/flaky-ratchet.py update` сохраняет меньшие значения и удаляет исчезнувшие файлы, а повышение требует `python3 scripts/flaky-ratchet.py update --allow-increase <файл> <правило> "<причина>"` — причина попадает в базу. Исправление одного файла не оплачивает появление паттерна в другом. Комментарии и строковые литералы не считаются; в Rust-исходниках считаются только `#[cfg(test)]`-модули, в `crates/*/tests/**` — весь файл.

Адреса `resolved` в отслеживаемых `package-lock.json` проверяет `scripts/lockfile-registry-check.py` (самотесты в `scripts/test-lockfile-registry-check.py`): любой адрес не с `https://registry.npmjs.org/` валит проверку, скрипт печатает файл и первые строки. Причина проверки: приватное зеркало npm в локальной сети делегата однажды записало в `web/package-lock.json` 113 таких адресов. Лечится заменой origin через `sed` (подсказка печатается вместе с ошибкой); хеши `integrity` остаются верными, потому что зеркало лишь проксирует npm. `scripts/check.sh` гоняет проверку в быстром и полном режимах сразу после храповика. Чтобы `npm install` не брал зеркало из пользовательских настроек, `web/.npmrc` закрепляет публичный реестр.

Все тесты всех бинарников идут одним пулом, процесс на тест (`.config/nextest.toml`): потоков по числу ядер (`CANTERLOT_TEST_THREADS`); вдвое больше ускоряет пул, но рядом с линтом ломает тайминги тестов с сопровождением. Лимиты внутри процесса в таком режиме не работают, поэтому тяжёлые сценарии с сопровождением ограничены группой `heavy-e2e`, а три теста на голодание управляющего канала идут по одному и занимают часть пула (группа `starvation`). Долгие тесты видны в итоге как SLOW, `scripts/slowest-tests.py` печатает самые долгие. Повтор упавшего теста разрешён только для карантина: записи помечены `QUARANTINE` с причиной в `.config/nextest.toml` и в тестах, `scripts/list-quarantine.sh` их перечисляет, а `.superpowers/carry/flaky-tests.md` хранит историю; остальные тесты молча не перезапускаются. `cargo test` по-прежнему работает, но без групп. Сквозные сценарии пилота: `cargo nextest run --workspace --exclude canterlot-speech --all-features -E 'binary(it) & test(/^pilot_e2e::/)'`; процессы сопровождения и синтетического CLI тест убивает при завершении. Стенд с настройками как у `serve` (лимиты, активность, планировщик, хранилище вложений) собирает `testkit::spawn_test_hub_with`. Перед запуском `scripts/reap-stray-tests.sh` снимает тестовые супервизоры этой копии старше 30 минут: они намеренно переживают воркера, поэтому убитый прогон оставляет их работать. Каждая пересборка оставляет старые копии тестовых бинарников; `scripts/prune-target.sh` (его зовёт каждая проверка, включая `--fast`, и при падении тоже) убирает все, кроме свежих. После `check.sh --all` скрипт удаляет и `target/debug/incremental` своей копии: при тех же исходниках это ничего не пересобирает, а следующая правка один раз соберёт изменённые крейты без инкрементального кэша. Сборка тестов гейта с `CARGO_INCREMENTAL=0` проверена и отвергнута: переключение режима само не пересобирает, но держит вторую копию тестовых бинарников (`deps` 26 ГБ против 19), а `prune-target.sh` удаляет инкрементальный вариант, и следующий `--fast` перелинковывает четыре крейта (71 с).

В профиле `dev` зависимости собираются с `opt-level = 1`: SCRAM-аутентификация sqlx в неоптимизированной сборке стоила 0,3 с процессора на каждое подключение к тестовой базе. Инкрементальная сборка включена (кеш выходит на плато около 6 ГБ).

Спека на диске и IPC переживают релиз, который их записал: сопровождение живёт дольше воркера. `tests/it/spec_compat.rs` держит замороженную спеку старого релиза в `tests/it/fixtures/spec/`: при удалении или переименовании поля настройки адаптера добавляйте сюда спеку уходящего релиза. Совместимость со старым сопровождением проверяется вручную, без переменных тесты идут на текущем бинарнике: `CANTERLOT_OLD_RELEASE_BIN=<бинарник релиза> cargo test -p canterlot-worker --all-features --test it spec_compat::` (рукопожатие и журнал настоящего выпущенного бинарника; тест берёт из `tests/it/fixtures/spec/` спеку, которую бинарник принимает, поэтому для каждого выпуска с удалённым полем настройки нужна своя) и `CANTERLOT_E2E_SUPERVISOR_BIN=<бинарник старого коммита, собранный с --all-features> cargo test -p canterlot-worker --all-features --test it exec_e2e::` (весь сценарий на синтетическом CLI).

Перегенерация перечня лицензий зависимостей — `scripts/gen-licenses.sh` (обновляет `docs/licenses/THIRD_PARTY.md`).

Для тестов хаба нужны Docker и `strace` (регрессии порядка fsync); `strace` также используется сквозной проверкой долговечности каталогов worker. Из корня репозитория запустите PostgreSQL 18.6 и тесты:

```bash
scripts/test-db.sh up
source ~/.cargo/env
cargo nextest run --workspace --exclude canterlot-speech --all-features
```

База доступна на `127.0.0.1:55440`. Тестовые пользователь, пароль и база: `canterlot`.
`DATABASE_URL` задан в `.cargo/config.toml`; переменная окружения позволяет использовать другую тестовую базу.
Каждый тест с базой получает свою базу, поэтому пользователю PostgreSQL нужно право `CREATEDB`. Тест хаба или воркера объявляется `#[tokio::test]` и первой строкой берёт `let pool = canterlot_hub::testkit::db().await;`: миграции хаба применяются один раз в шаблон `canterlot_tpl_<хэш набора миграций>` (его строит первый тест под advisory-замком, на сервере остаются текущий и один предыдущий), а тест получает `CREATE DATABASE … TEMPLATE`; клон удаляется, когда завершается поток теста (после рантайма теста), в том числе при панике. Тесты миграций и схемы (`schema`, `installation`, `final_fix_cli`, тесты с `migration` в имени) остаются на `#[sqlx::test(migrator = …)]` и прогоняют миграции сами.
Функции подготовки данных доступны с feature `testkit` и автоматически подключаются при тестировании хаба.
Параллельные рабочие копии на одной машине поднимают отдельный сервер: `CANTERLOT_TEST_PG_PORT=55441 scripts/test-db.sh up`.
Затем `DATABASE_URL=postgres://canterlot:canterlot@127.0.0.1:55441/canterlot cargo nextest run --workspace --exclude canterlot-speech --all-features`. Одной переменной порта для `cargo test` мало: `.cargo/config.toml` указывает на общую базу 55440. `scripts/check.sh` сам выставляет `DATABASE_URL` по `CANTERLOT_TEST_PG_PORT`, если `DATABASE_URL` не задан.
sqlx и `testkit::db()` называют тестовые базы по пути теста, поэтому две копии на одном сервере удаляли бы базы друг друга.
`down` и `psql` принимают ту же переменную.

```bash
scripts/test-db.sh psql
scripts/test-db.sh down
```

Данные контейнера хранятся в tmpfs и теряются при его остановке.

### Очередь тяжёлых задач

Тяжёлые прогоны одной машины встают в очередь воркера, а не толкаются: `scripts/check.sh` и `scripts/check-web.sh` перезапускают себя под `canterlot-worker lease run`, если аренда ещё не взята. `scripts/deploy/tests/run.sh` тоже берёт аренду `build`, когда его запускают отдельно; под `check.sh` или `gate.sh` он видит уже взятую и второй не берёт. Вид аренды:

- `gate` (весь станок) — `check.sh --all` и полный гейт;
- `build` (один слот) — всё остальное, в том числе обычный `check.sh` без флагов и обычный `check-web.sh`.

Ёмкость — `[machine].lease_slots` воркера (по умолчанию 2). Очередь FIFO по времени запроса с преимуществом гейта: пока ждёт гейт, сборка, пришедшая после него, не начинается раньше; сборка, пришедшая раньше, получает слот, если помещается. Пока ждёт, обёртка печатает в stderr одну строку и повторяет её при смене места: `canterlot lease: waiting for gate, position 2 (holders: build «check.sh --fast · <каталог>» 0190a1b2, build owner)` — метка `--label`, имя рабочего каталога и короткий номер исполнения (`owner` — процесс без метки исполнения, например терминал владельца). `canterlot-worker lease status` показывает держателей и очередь с pid обёртки и его состоянием (`T` — остановлен, например Ctrl-Z в терминале: такая аренда держится, пока процесс жив).

Флаг `lease run --strict` включает строгий режим: без воркера или сокета обёртка не запускает команду вне аренды, а ждёт (отказ сервера три раза подряд завершает её кодом 75). Так ждут своей очереди задачи на месте (`docs/configuration.md`); обычные вызовы флага не используют и ведут себя как прежде.

Ограничения ресурсов приходят командам через окружение. Бюджет `B = max(2, nproc · вес / слоты)` (гейт — `nproc`): `CANTERLOT_LEASE_BUDGET` и `CANTERLOT_TEST_THREADS` равны `B`, `CARGO_BUILD_JOBS` — `B`, но не больше уже заданного меньшего значения (воркер ставит сессиям 6). `check.sh` делит потоки сборки между параллельными линтом и тестовой сборкой: каждой `⌈CARGO_BUILD_JOBS / 2⌉`, тестам `B` потоков. В начале каждого прогона `check.sh` печатает `load1`, число ядер и номер аренды: гейт исключителен только среди арендованных задач, и эта строка объясняет ложные падения под чужой нагрузкой.

Аренда держится, пока жива обёртка (pid и время старта) и открыто её соединение с воркером; выданные аренды переживают рестарт воркера (`<data_dir>/leases.json`), а обёртка переподключается сама. Запись очереди на диск идёт вне замка состояния и в блокирующем пуле: медленный `fsync` не задерживает ответы аренде. В файл идёт и число ожидающих: если перед рестартом очередь была непустой, воркер три секунды не выдаёт слоты — ожидающие переподключаются шагом 250 мс, возвращаются на свои места по времени запроса, и сборка, пришедшая в это окно, не обгоняет ждущий гейт. Ожидающий, который так и не вернулся за три секунды, теряет место — это цена того, что очередь не хранится поэлементно. Воркер никого не убивает. Команда остаётся в сессии обёртки, а в её группе процессов — только когда терминал принадлежит группе обёртки, то есть при запуске на переднем плане из терминала: Ctrl-C, Ctrl-Z и `tcsetpgrp` команды работают как у обычного процесса. В исполнениях агента, скриптах и фоновых заданиях терминал обёртке не принадлежит, и команда получает свою группу процессов: групповой сигнал (`kill -TERM -<pgid>`, Ctrl-C, закрытие терминала) достаётся одной обёртке и приходит команде ровно один раз — пересланным. TERM и HUP пересылаются всегда, INT и QUIT — только в отдельной группе (там группа их не получила). Остаточный компромисс: при запуске на переднем плане из терминала групповой TERM и HUP по-прежнему доходят до команды дважды, от ядра и пересланные: отличить групповую доставку от адресной нельзя, а без пересылки `kill -TERM <обёртка>` не дошёл бы до команды; Ctrl-C в терминале и так достаётся команде один раз, потому что INT и QUIT обёртка не пересылает. В отдельной группе убийство группы обёртки (`kill -KILL -<pgid>`) команду не забирает: осиротевшую команду находит и завершает группа исполнения по сессии. Код выхода — код команды, смерть от сигнала повторяется тем же сигналом, 127 — команда не найдена, 2 — неверные аргументы. `kill -9` одной обёртки освобождает слот раньше, чем закончится команда.

Аренда никогда не мешает сборке. Без воркера, без пути к сокету, при отказе подключения (в том числе EACCES из песочницы), при сокете, который не отвечает дольше 60 с, при ошибке протокола и со старым бинарником без подкоманды `lease` (скрипты проверяют `"$bin" lease --help`) команда идёт без аренды с одной строкой в stderr, а `CANTERLOT_LEASE_HELD=unleased` не даёт вложенным скриптам пробовать снова. Внутри аренды (`CANTERLOT_LEASE_HELD` задан) `lease run` сразу запускает команду с одной строкой-пометкой: вложенный `check.sh` → `check-web.sh` слота не занимает. Бинарник ищется в `$CANTERLOT_WORKER_BIN` (воркер передаёт его исполнениям вместе с `CANTERLOT_LEASE_SOCKET`), затем `canterlot-worker` в `PATH`, затем установка воркера `$HOME/.local/opt/canterlot/current/bin/canterlot-worker`, если файл исполняемый: прод-юнит запускает воркер по этому пути, а в `PATH` его нет. В `CANTERLOT_WORKER_BIN` воркер кладёт абсолютный argv[0], если это исполняемый файл (ссылка `current/bin` переживает выкатку и чистку релизов), иначе свой `current_exe` без суффикса ` (deleted)`; сокет — в `$CANTERLOT_LEASE_SOCKET`, иначе `$XDG_RUNTIME_DIR/canterlot/lease.sock` (терминал владельца).

`CANTERLOT_NO_LEASE=1` отключает обёртку (для отладки). Очередь не защищает от обмана, у машины один владелец: подделанная метка `CANTERLOT_EXECUTION_GROUP` только неверно подписывает держателя, а `CANTERLOT_NO_LEASE=1` или `CANTERLOT_LEASE_HELD=x` только выводят прогон из очереди. `sudo`, `env -i`, `systemd-run` и `docker run` теряют метку и сокет: такой прогон идёт как `owner` или без аренды. Голый `cargo test`, набранный моделью, не арендуется.

Свой скрипт гейта (например `scripts/gate.sh`, который ставит зависимости веба один раз и зовёт `check.sh --all --no-web` и `check-web.sh`) берёт одну аренду `gate` на весь прогон, иначе между двумя гейтами в очередь вклиниваются сборки. Проще всего — обернуть запуск: `canterlot-worker lease run --kind gate --label gate -- bash scripts/gate.sh`. Чтобы скрипт делал это сам, первые строки после `set -euo pipefail`:

```bash
source /path/to/canterlot/scripts/lease.sh
lease_self_wrap gate gate bash "$(realpath "$0")" "$@"
```

Вложенные `check.sh` и `check-web.sh` видят `CANTERLOT_LEASE_HELD` и второй аренды не берут. `CANTERLOT_LEASE_DRY=1` печатает решение обёртки (вид и причину) и завершает скрипт без прогона: так тест `tests/it/lease_scripts.rs` проверяет классификацию.

## Гейт перед выпуском

Полный гейт перед выпуском — `scripts/gate.sh`. Он берёт одну аренду `gate` на весь прогон и проводит веб ровно один раз:

1. один `npm ci` в `web/`;
2. `scripts/check.sh --all --no-web` — версия, fmt, clippy, nextest и `canterlot-speech`; флаг `--no-web` снимает только веб-этап, остальные шаги не меняются;
3. `scripts/deploy/tests/run.sh` — только если с последнего тега `vX.Y.Z` менялась обвязка выката; идёт отдельным шагом, чтобы тайминговые наборы не пересекались с тяжёлыми проверками, в аттестацию пишется `deploy_exit=0` или `deploy_exit=skipped`;
4. `scripts/check-web.sh` — сверка типов API, svelte-check, eslint, token lint, vitest, две сборки PWA и `vitest.build`.

Гейт гоняет веб ровно один раз и не собирает web отдельным шагом — `check-web.sh` собирает сам.

Порт тестовой PostgreSQL по умолчанию 55441 (`CANTERLOT_TEST_PG_PORT`), временный каталог — `/tmp` (`CANTERLOT_GATE_TMPDIR`: сокет исполнения не влезает в `SUN_LEN` при длинном пути), бинарник воркера для аренды — установка воркера в `$HOME/.local/opt/canterlot/current/bin/canterlot-worker` (`CANTERLOT_WORKER_BIN`); все три переменные перекрываются окружением.

Сводка в формате прежнего `result.txt` (sha, коды возврата и длительности шагов, включая `deploy`, строки nextest и первые упавшие тесты) печатается в stdout и пишется в `.superpowers/gate/<sha8>/` вместе с логами `npm-ci.log`, `check.log`, `deploy.log` и `web.log`. При полном успехе, чистой рабочей копии в начале и в конце и неизменном HEAD гейт атомарно (временный файл и `rename`) пишет аттестацию `.superpowers/gate/<full sha>.ok` с sha, временем начала и конца, длительностями и строками сводки — её требует `release.sh ship`; из кодов шагов пропуск допускается только для `deploy_exit=skipped`. Грязная рабочая копия аттестации не даёт: гейт отказывается работать, пока его не попросят явно с `--no-attest` (тогда прогон идёт, аттестация не пишется). `.superpowers/` не отслеживается git.

Состояние каждого прогона пишется в `.superpowers/gate/<sha8>/state.tsv` строками
`<ключ><TAB><значение>`: `schema`, полный `sha`, `started`, `finished`,
`completed`, `mode`, `*_exit`, `*_duration_s`, `*_failed` (`ok`, `tests` или
`other`) и идентичности упавших тестов — `rust_fail=rust|binary|test`
(nextest JUnit) и `deploy_fail=deploy|suite` (файлы `.rc` набора).
Также в состоянии хранятся `rerun_of`, `rerun_done` и `flaky`.

`scripts/gate.sh --rerun-failed` читает состояние предыдущего завершённого
прогона ровно этого HEAD при чистом дереве, берёт ту же аренду `gate` и
перезапускает только упавшие шаги: прошедшие не вызываются, шаг с
`*_failed=tests` получает точный фильтр (`binary_id(=…) & test(=…)` для nextest,
`RUN_SUITES` для наборов выката), шаг с `*_failed=other` идёт целиком.
У веба идентичностей тестов больше нет, поэтому его провал всегда считается
`other` и повторяет шаг целиком. Каждая идентичность попадает в `rerun_done`;
повторный `--rerun-failed` для неё отказывает, а повторное падение проваливает гейт.
При успехе в аттестацию добавляются `rerun_of=<started первого прогона>` и
`flaky=<идентичность>` для тестов, прошедших только на перезапуске. В конце
печатается отчёт о нестабильных тестах с напоминанием про
`.superpowers/carry/flaky-tests.md`; гейт этот файл не редактирует.
`--no-attest` сохраняет работу и с полным прогоном, и с перезапуском.

Что гейт состоит из тех же шагов, проверяет `scripts/gate-coverage.sh`: он сверяет шаги версии, Rust, речи, статики и vitest. Те же проверки доказывают паритет контейнерного пути: `scripts/gate/in-container.sh` не перечисляет шаги заново, а зовёт `scripts/gate.sh --no-attest` (с `CANTERLOT_NO_LEASE=1`), поэтому `scripts/gate-remote.sh` и гейт на раннере гоняют те же `npm ci`, `check.sh --all --no-web`, наборы выката и `check-web.sh`, без тестовых швов `CANTERLOT_GATE_*`; образ `docker/gate/Dockerfile` несёт `shellcheck` и `zstd` для наборов выката. Сверка падает, если контейнерный путь начал вызывать шаги сам, подменять их или перестал запускать наборы выката.

### Гейт на раннере

`scripts/gate.sh --runner prefer|local|auto|<имя машины>` по умолчанию (`prefer`) идёт на первого доступного статического раннера, а если такого нет — гейтит на этой VM (`local`, всё как выше, без изменений). `prefer` читает `[[runners.ssh]]` из конфига воркера `${CANTERLOT_WORKER_CONFIG:-$HOME/.config/canterlot/worker.toml}` через `tomllib`, как `scripts/deploy/verify.sh`, и в порядке записей проверяет `ssh -o BatchMode=yes -o ConnectTimeout=5 <ssh> true`. Выбор объясняется одной строкой: `гейт: раннер <machine> доступен — гейт идёт на нём` или `гейт: раннеров нет или они недоступны — гейт идёт на этой машине`. Нет конфига, записей или доступных раннеров — локальный гейт. Облако `prefer` не выбирает: оно стоит денег; для облака нужно явное `--runner`. Шов `CANTERLOT_GATE_RUNNER_PROBE` (команда, которой передаётся ssh-алиас, код 0 — доступен) заменяет пробу в тестах, без сети; в обычном прогоне он пуст. Явные `auto` и имя машины сохраняют прежний смысл. Не-локальное значение требует чистой рабочей копии (`--no-attest` её не отменяет) и берёт полный sha `HEAD`; аренду `gate` оно берёт на раннере, а не на VM:

```
canterlot-worker job run --title "Гейт <sha8>" --machine <значение> \
    --lease gate --timeout-minutes 240 --expected-minutes 45 --source <repo>@<sha> \
    --output .gate-out --wait -- scripts/gate/runner-gate.sh <sha>
```

Метка ядра `kernel-6.12` в задаче больше не требуется (решение владельца 23.11.2026): гейт идёт на первого доступного статического раннера, риск немногочисленных ядерно-зависимых тестов принят.

`scripts/gate/runner-gate.sh <sha>` работает в подготовленной носителем рабочей копии `<корень>/jobs/<домен>/<задача>`: проверяет `git rev-parse HEAD` против аргумента, находит корень по пути (без `<корень>/worker.toml` отказывает), собирает или берёт готовый образ `canterlot-gate:<hash Dockerfile + rust-toolchain.toml>` из `docker/gate/Dockerfile` этой же копии, запускает контейнер `canterlot-gate-<sha12>-<случайное>` (`--cpus` и `--memory` от `nproc` и `free` минус 2 ядра и 4 ГБ; TERM и INT делают `docker kill` по имени) с кэшами `<корень>/cache/{cargo,target,npm,tmp}` и оставляет `result.txt`, `state.tsv` и логи в `./.gate-out`. В `state.tsv` попадает строка `runner<TAB>имя` (`CANTERLOT_RUNNER_NAME`, иначе имя хоста). Код выхода — код гейта.

`gate.sh --runner` читает собранные `<каталог результатов>/.gate-out/result.txt` и `state.tsv` (каталог печатает `job run` строкой `canterlot job: outputs:`) и атомарно пишет `.superpowers/gate/<full sha>.ok` в прежнем формате плюс `runner=<имя, как его назвал раннер>`, только если result.txt называет этот sha8, `state.tsv` — этот полный sha без тестовых швов, каждый шаг вышел с 0 (`deploy_exit=skipped` допустим), код задачи 0, `HEAD` не сместился и дерево осталось чистым. Иначе называет причину и ничего не пишет. Копия собранного лежит в `.superpowers/gate/<sha8>/runner/`, а вывод `job run` — в `runner-job.out` и `runner-job.err` рядом. `release.sh ship` принимает такую аттестацию без изменений (набор `release` проверяет строку `runner=`). С явным `--runner auto|<машина>` перезапуск в v1 отказывается: такой гейт идёт целиком. При значении по умолчанию `prefer` поведение `--rerun-failed` другое: если на этом коммите есть локальное состояние `.superpowers/gate/<sha8>/state.tsv`, перезапуск идёт локально, как описано выше; если его нет (первый прогон ушёл на раннер, и его результаты лежат в `.superpowers/gate/<sha8>/runner/`), полный гейт повторяется через `prefer`. Обычная пара `scripts/gate.sh || scripts/gate.sh --rerun-failed` работает в обоих случаях.

Отмены у `job run` нет, и новых API хаба для неё не заведено: Ctrl-C (или TERM) на `gate.sh --runner` останавливает только ожидание, печатает идентификатор задачи, а сама задача на раннере продолжает работать (иначе она остановится по своему таймауту в 240 минут). Отменить задачу можно в PWA, в её карточке. Проверки без сети и без настоящего docker — `scripts/gate/test.sh` с подменами `scripts/gate/fake-worker` (`CANTERLOT_WORKER_BIN`) и `scripts/gate/fake-docker` (`CANTERLOT_DOCKER`).

### Полный гейт на сборочной машине

Тот же прогон можно снять на сборочной машине (`CANTERLOT_BUILD_HOST`) в контейнере Debian 13, без влияния на рабочую VM:

```bash
scripts/gate-remote.sh 4d884712            # коммит на сборочной машине
scripts/gate-remote.sh 4d884712 --local    # то же на местном docker, для проверки
```

`REV` — любой коммит, не только тег (по умолчанию `HEAD`). Скрипт отправляет его в `~/canterlot.git` как `refs/gate/<sha12>`, создаёт отсоединённую рабочую копию `~/gate/src-<sha12>` и запускает в контейнере тот же `scripts/gate.sh --no-attest`, что идёт на VM (через `scripts/gate/in-container.sh`, с тестовой PostgreSQL). Без тега `vX.Y.Z` в рабочей копии наборы выката идут всегда.

Образ `canterlot-gate:<hash>` (`docker/gate/Dockerfile`) — Debian trixie по digest, Rust из `rust-toolchain.toml`, Node 24, PostgreSQL 18 и те же шрифты, что на VM. Тег считается из Dockerfile и `rust-toolchain.toml`. База поднимается внутри контейнера на `127.0.0.1:55441` с выключенным fsync; на хосте порт не занимается, поэтому гейт на VM можно запускать одновременно.

Ограничения контейнера — `--cpus 30 --memory 48g --pids-limit 8192 --shm-size=2g`: общая память 2 ГБ вместо 64 МБ по умолчанию, `strace`-тестам выдаётся `SYS_PTRACE`, `--init` пожинает осиротевших потомков. Кэши на сборочной машине — `~/gate/cache/{cargo,target,npm,tmp}`, локально — `~/.cache/canterlot-gate` (`CANTERLOT_GATE_CACHE`); повторный прогон того же коммита их переиспользует. `tmp` — это `/tmp` контейнера (база и временные каталоги тестов): на overlayfs и на tmpfs те же проверки ловят таймауты, на обычной ФС хоста проходят.

Результат и логи (`gate.log`, `npm-ci.log`, `check.log`, `deploy.log`, `web.log`, `result.txt` и `state.tsv` в формате `scripts/gate.sh`) складываются в `/tmp/gate-remote/<sha12>` (`CANTERLOT_GATE_OUT`). Код возврата — код гейта. Рабочая копия и ref на сборочной машине убираются автоматически (`git worktree remove --force` и `update-ref -d`, без `rm`).

## Выпуск

Запись версии в `CHANGES.rst` пишется руками и только плоскими пунктами: вложенные списки экран «Что нового» теряет, и `web/tests/unit/changelog.test.ts` роняет гейт. Правила бампа — в [versioning.md](versioning.md). Дальше три команды (ранбук — §11.7 [deploy/pilot.md](deploy/pilot.md)):

```bash
scripts/release.sh prepare 1.35.0    # бамп версии во всех местах, cargo update -w, check-version, коммит «Выпустить версию 1.35.0»
scripts/gate.sh                      # полный гейт, аттестация .superpowers/gate/<sha>.ok
scripts/release.sh ship 1.35.0       # тег, push, сборка, stage, репетиция, боевой прогон, check-proxy, публикация, журнал
scripts/release.sh ship 1.35.0 --full   # то же без быстрого пути: всегда репетиция и vzdump
```

`prepare` правит `Cargo.toml`, `web/package.json`, `docs/api/openapi.json`, бейдж версии и строку «Текущая версия — X.Y.Z.» в `README.md`; формат обоих мест менять нельзя, его сверяет `scripts/check-version.sh`.

Выпуск прикладного кода без миграций идёт быстрым путём: `ship` сам сравнивает развёрнутый релиз с целью (`scripts/deploy/fastpath.sh`, точные хэши коммитов и список прикладного кода). Если изменились только исходники, тесты, веб, документация и номер версии, репетиции нет (`cutover.sh --fast` сам повторяет проверки, и все ворота остаются), а vzdump пропускается, только когда манифест миграций бинаря цели (`canterlot-hub migrations manifest --json`) точно совпал с `_sqlx_migrations` живой базы (проверенный `pg_dump` остаётся точкой отката). Любая неясность — полный путь; путь и причины пишутся в лог, файл итога и журнал. Подробности — §11.1 и §11.7 [deploy/pilot.md](deploy/pilot.md).

`ship` можно запускать в фоне: ход — в `.superpowers/deploy/release-1.35.0.log`, итог — в `release-1.35.0.result`. Он публикует только после `cutover done` без мягких ошибок, репетицию повторяет один раз и только на переходных кодах (стартующая сессия, идущий vzdump), боевой прогон не повторяет никогда; после остановки — `scripts/release.sh ship 1.35.0 --continue`.

Выкат production идёт только через `scripts/deploy` (§11 [deploy/pilot.md](deploy/pilot.md)): он проверяет готовый dist из `build-release.sh`, `cutover.sh <id> --dry-run` или `--rehearse` (весь путь записи, записи только печатаются) выдаёт токен, а боевой прогон `cutover.sh --execute <id> --confirm <токен>` принимает его 2 часа и только для того же плана. Значения своего стенда — в `.superpowers/site.env`, образец в [deploy/site.env.example](deploy/site.env.example).

Архивы выпуска публикует `scripts/publish-release.sh vX.Y.Z` из того же каталога сборки, без повторной компиляции.

## Сборка выпуска и кэш

Выпуск собирается один раз в контейнере Debian 13, а готовый каталог затем используют и выкат, и публикация. Собирать напрямую, без контейнера, нельзя: прод — Debian 13 с glibc 2.41, и бинарники, собранные на системе с более новой glibc, на нём не запустятся.

```bash
scripts/build-release.sh v1.24.0            # сборочная машина (CANTERLOT_BUILD_HOST, по умолчанию canterlot-build)
scripts/build-release.sh v1.24.0 --local    # та же сборка на местном docker
```

Тег должен быть локально и на origin. На сборочной машине нужны ssh-доступ (алиас `canterlot-build`), работающий docker и bare-репозиторий `~/canterlot.git`. Скрипт:

1. отправляет тег в bare-репозиторий `~/canterlot.git` на сборочной машине;
2. создаёт там чистую рабочую копию `~/release/src-<sha12>` на коммите тега;
3. собирает или переиспользует образ `canterlot-release-build:<hash Dockerfile + rust-toolchain.toml>` из `docker/release-build/Dockerfile`: Debian trixie по digest, Rust из `rust-toolchain.toml`, Node 24 с проверкой sha256;
4. запускает контейнер от uid хоста с ограничениями `--cpus 24 --memory 40g --pids-limit 4096` (systemd-лимиты сборочной машины на контейнеры dockerd не действуют);
5. переносит готовый каталог по ssh tar-потоком в `.superpowers/deploy/dist/<sha12>` этой рабочей копии (`CANTERLOT_DIST_ROOT`), проверяет SHA256SUMS, `RELEASE`, `--version` и состав файлов; существующий каталог не перезаписывается;
6. убирает рабочую копию на сборочной машине через `git worktree remove --force`.

Внутри контейнера `scripts/release-build/in-container.sh` собирает хаб, воркер, синтетический воркер, `canterlot-speech` с CUDA 12.9 для sm_70 (`hub/bin/canterlot-speech`; образ содержит nvcc, cudart, cublas и cccl из архивов redist NVIDIA с проверкой sha256) и PWA (`npm ci`, `PWA_VERSION=<sha12> npm run build`) и проверяет то же, что выкат: отсутствие libssl и CUDA у хаба, прямые зависимости `canterlot-speech` (только glibc, libstdc++ и `libcuda.so.1`, `libcudart.so.12`, `libcublas*.so.12`), `hub openapi` против `docs/api/openapi.json`, CSP, `_app/version.json`, `synthetic-cli` только в синтетическом воркере, SHA256SUMS и максимальную версию GLIBC не выше 2.41. Тестов здесь нет: полный гейт уже прошёл на этом коммите. Сборочный инструмент берётся из этой рабочей копии, а не из тега.

Сборка переиспользует общий каталог кэша (`CANTERLOT_BUILD_CACHE`; локально
`~/.cache/canterlot-release-build`, на сборочной машине `~/release/cache`):

| Путь | Что лежит | Когда обновляется |
| --- | --- | --- |
| `cargo/` | реестр crates.io и распакованные исходники зависимостей из `Cargo.lock` | при новых зависимостях |
| `npm/` | кэш `npm ci` для PWA | при новых зависимостях веба |
| `target/<ключ>/` | весь `CARGO_TARGET_DIR`: объекты Rust и нативные объекты `transcribe.cpp`/ggml (CMake и nvcc) | `cargo build --locked` идёт каждый раз и пересобирает изменившееся; нативные объекты переиспользуются, пока не менялись ключ и код |

`<ключ>` — 16 шестнадцатеричных символов sha256 от версии схемы, **id сборочного
образа** (`docker image inspect --format '{{.Id}}'`) и строки `TRANSCRIBE_CMAKE_ARGS`
(`-DCMAKE_CUDA_ARCHITECTURES=70 -DGGML_NATIVE=OFF -DTRANSCRIBE_USE_SYSTEM_BLAS=OFF`).
Поэтому:

- изменился `docker/release-build/Dockerfile` (база, CUDA, nvcc, Rust),
  `rust-toolchain.toml` или digest базы — у нового образа другой id, другой
  `<ключ>`, и нативная часть собирается заново один раз (5–9 минут на 36
  ядрах); каталог прежнего ключа удаляется после успешной сборки;
- изменились флаги CMake (переменная `CANTERLOT_TRANSCRIBE_CMAKE_ARGS`) — другой
  `<ключ>` и тоже полная пересборка нативной части;
- изменился только код Canterlot или `Cargo.lock` — `<ключ>` прежний, cargo
  пересобирает изменившееся, а объекты `transcribe.cpp`/ggml остаются;
- `canterlot-speech` не менялся вовсе — шаг службы речи занимает секунды вместо
  5–9 минут.

Готовый бинарник из кэша не копируется: каждый шаг — это `cargo build --release
--locked`, и cargo сам решает, что пересобрать.

На всё время сборки `build-release.sh` берёт `flock` на `<кэш>/.build.lock`:
вторая сборка с тем же кэшем ждёт, а не пишет в те же каталоги. После успешной
сборки скрипт оставляет в `target/` только каталог текущего ключа; каталоги с
другим 16-символьным именем-ключом удаляются, прочие файлы (в том числе старый
расклад `target/release` и `target/debug` до появления ключей) не трогаются.
После перехода на ключи старый расклад больше не используется: его можно удалить
руками, следующая сборка один раз соберёт всё заново.

Очистить кэш вручную: `rm -rf ~/.cache/canterlot-release-build` — всё, включая
загрузки cargo и npm; или только `target/`, если нужно пересобрать Rust и нативную
часть, сохранив загрузки. Не удаляйте каталог, пока идёт сборка: она пишет в него.

Если меняется само правило сборки (другая CUDA, другая схема ключа), меняйте
версию схемы в `scripts/release-build/cache.sh`: иначе старый каталог может быть
принят за текущий.

## Обновление контракта пилота

Порядок обновления: хаб первым, затем воркер. Возможность `todo_v1` разрешает запись `TodoUpdated`: со старым хабом воркер отбрасывает список задач, ход не страдает. Возможность `background_v1` так же разрешает `BackgroundWorkChanged`. Возможность `pilot_v1` согласуется через `Hello.optional_capabilities` и `Accepted.capabilities`; до её подтверждения записи и команды пилота не используются. Возможность `session_tools_v1` разрешает рабочие копии (`StartExecution.worktree`, `RemoveWorktree`, `WorktreeRemoved`), факты git в `RunFinished` и признак `isolate_mcp` делегированных сессий; миграция `0035_session_tools` выполняется внутри `serve`, поэтому хаб обновляется первым (неизвестная запись `RecordBody` останавливает журнал, `streams.md` §3), затем PWA и воркер. Со старым воркером команды с этой возможностью остаются ожидающими, изолированный запуск отвечает `capability_missing`, а делегированная сессия ждёт обновления воркера и ни при каких условиях не стартует с серверами владельца; обычные сессии не меняются. Работающие CLI сохраняют прежний список инструментов и инструкции до перезапуска. Возможность `limits_fable_v1` разрешает записи окна `weekly_fable`: со старым хабом воркер подменяет наблюдение Fable безвредной диагностикой, а упор отправляет недельным окном с тем же сбросом, поэтому журнал подтверждается до конца и сессия не останавливается; строку Fable в машинном отчёте старый хаб не получает. `owner_exit_v1` так же требует хаб первым: без неё команда `resolve_owner_lost` остаётся ожидающей. `guarded_interrupt_v1` (охранный `interrupt` с именем хода и отметкой прогресса, `docs/design/streams.md`): со старым воркером хаб прерывает зависший ход только по кнопке владельца, а автоматическое спасение остаётся карточкой. `send_now_v1` («Отправить сейчас», команда `steer_input`) и `owner_instructions_v1` (лист «Как у владельца») так же требуют обновлённого воркера; со старым кнопка отвечает отказом с подписью (`capability_missing`). Откат хаба ниже возможности, которую воркер уже использовал, не поддерживается; после подключения воркера откат через миграцию 0035 невозможен. Если обновление всё же разошлось и хаб не может применить запись журнала, запись не теряется: она остаётся на машине неподтверждённой, а на экране машины появляется предупреждение `cj_journal_stopped` или `sj_journal_stopped`. Журнал команд держит всю машину, журнал сессии только свою сессию. Обновление хаба или воркера снимает стоп само, ручных действий не требуется; в логах искать `journal_stopped` у хаба и `journal_refused` у воркера. Обновление управляющей службы воркера сохраняет живые процессы сопровождения и их исполнения.

Фоновые службы (`singleton.rs`) восстанавливают отдельное соединение и advisory lock после сбоя с задержкой 250 мс → 30 с, которая сбрасывается после 60 с непрерывной работы; второй хаб остаётся пассивным. `readyz` сообщает имена недоступных служб (`reason: service`, `services`). Планировщик считает ошибки отдельно от опросов и обрабатывает задания независимо. Временные ошибки пробуждений и проверок лимита повторяются неограниченно с задержкой не более 60 с; для остальных заданий действует `max_attempts`. Ежечасное обслуживание удаляет завершённые и ошибочные задания через 30 дней, общие ключи идемпотентности и события через 7 дней. Ключ отправки ввода и хеш запроса сохраняются в самом вводе: поздний повтор после очистки возвращает исходный ответ. Новый лимит получает собственный срок автопродолжения; поздний факт прерванного владельцем хода его не включает. Подробности восстановления, срок автоматического продолжения и форматы событий: [контракт API](design/api.md#восстановление-фоновых-служб-и-обслуживание).
