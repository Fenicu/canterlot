# Canterlot

<p align="center">

[![GitHub](https://img.shields.io/badge/GitHub-Fenicu%2Fcanterlot-brightgreen?logo=github)](https://github.com/Fenicu/canterlot)
![версия](https://img.shields.io/badge/%D0%B2%D0%B5%D1%80%D1%81%D0%B8%D1%8F-1.31.0-00f4ee)
[![история](https://img.shields.io/badge/%D0%B8%D1%81%D1%82%D0%BE%D1%80%D0%B8%D1%8F-CHANGES.rst-6c757d)](CHANGES.rst)
[![лицензия](https://img.shields.io/badge/%D0%BB%D0%B8%D1%86%D0%B5%D0%BD%D0%B7%D0%B8%D1%8F-MIT-blue)](LICENSE)
![агенты](https://img.shields.io/badge/%D0%B0%D0%B3%D0%B5%D0%BD%D1%82%D1%8B-Claude%20Code%20%C2%B7%20Codex%20%C2%B7%20OpenCode%20%C2%B7%20Antigravity-1f5457)
![Rust](https://img.shields.io/badge/rust-1.99.1-orange?logo=rust)
![SvelteKit](https://img.shields.io/badge/SvelteKit-3-FF3E00?logo=svelte&logoColor=white)
![PostgreSQL](https://img.shields.io/badge/PostgreSQL-18-4169E1?logo=postgresql&logoColor=white)
![Node](https://img.shields.io/badge/node-24-339933?logo=node.js&logoColor=white)

</p>

Canterlot — self-hosted хаб для агентов программирования. Claude Code, Codex, OpenCode и Antigravity работают на машинах владельца, а сессии открываются с телефона через PWA: лента, разрешения и вопросы, очередь сообщений, доска плана, поиск по истории, лимиты и настройки. Antigravity работает на Google-аккаунте владельца и использует его каталог `~/.gemini`: настройки, MCP-серверы и история разговоров общие с Antigravity владельца.

Разработка идёт в [Forgejo](https://git.fenicu.com/Fenicu/canterlot); GitHub — зеркало.

Хаб написан на Rust и хранит состояние в PostgreSQL. На каждой рабочей машине запущен Rust-воркер, хаб и воркеры общаются по gRPC поверх TLS. Клиент — статическая сборка SvelteKit; её раздаёт обратный прокси (Caddy), а владельца проверяет сам хаб: страница входа в PWA, пароль, ссылка восстановления и сеансы браузера в cookie. Отдельного сервера приложений у клиента нет.

Что можно делать из интерфейса:

- смотреть ленту сессии: ответы, вызовы инструментов, субагентов и служебные строки;
- отвечать на разрешения и вопросы агента карточками; ответ проходит этапы «отправка → сохранено → передано → применено»;
- вести очередь сообщений: «Отправить сейчас» вливает сообщение в идущий ход, а ждущее сообщение можно изменить до отправки;
- убирать сессию в корзину на 24 часа, возвращать её одним нажатием или удалять окончательно;
- продолжать черновик с любого устройства: текст и готовые вложения хранятся на хабе, удаление и отправка сразу видны на телефоне и компьютере;
- принимать подсказку следующего сообщения от Claude Code: она видна серым в пустом поле ввода и вставляется по Tab или кнопкой «Подставить», выключается в настройках;
- прерывать текущий ход, не закрывая сессию: контекст и процесс остаются, следующее сообщение продолжает работу;
- менять усилие (effort) посреди диалога, в том числе у Claude Code: новое значение действует со следующего хода;
- видеть заполнение контекста и сжимать его, в том числе между ходами;
- запускать делегированные сессии через MCP-инструменты: отдельная рабочая копия, вложенный список и панель делегирования, приём результата; итог делегата остаётся в ленте оркестратора и после удаления делегата, а о делегате, который дольше 15 минут простаивает с работающей фоновой командой, оркестратор слышит отдельно;
- давать агенту будильники: по времени, по состоянию других сессий (в том числе одним сообщением за всю пачку и со сторожем по сроку), по сбросу лимита провайдера, по появлению файла на машине и по освобождению машины; в чате они видны плашками;
- вести доску плана оркестратора в хабе: задачи с номерами, состоянием, исполнителем, находками и причиной блокировки переживают сжатие контекста и перезапуск, сами переходят в работу при запуске делегата с `plan_task`, на проверку по его завершении и в готовые при приёме результата, а коммиты и отчёт исполнителя подтягиваются при чтении; план открывается вкладкой «План» в панели делегирования, вызовы плана видны в ленте карточками, у сессии в списке — счётчик «План · 2/7», все планы — в меню «Планы»;
- включать автопинок делегата: хаб сам подталкивает исполнителя, бросившего ход без маркера `STATUS: DONE`, и будит оркестратора один раз, когда толчки кончились; состояние, счётчики и последние сигналы видны в чате, владелец ставит правило на паузу или снимает его, а текст толчка редактируется во вкладке «Текст толчка» экрана «Инструкции»; файл отчёта считается прогрессом, а его отсутствие называется в толчке;
- искать по истории сессий: ответы агентов, свои сообщения, сообщения между сессиями и итоги делегатов в любых формах слова, с фильтрами по периоду и виду; найденные слова подсвечены, касание открывает ленту на месте совпадения; поиск запускается из списка («Искать в сообщениях») или из меню сессии, агент ищет тем же поиском через `history_search`;
- выбирать исполнителя по политике маршрутизации владельца: тип задачи, упорядоченные кандидаты, пороги лимитов и параллельности; предпросмотр показывает выбор и причины по каждому кандидату;
- видеть публичный статус Anthropic и OpenAI рядом с лимитами: хаб сам опрашивает их страницы статуса (`[provider_status]`), при сбое полоса лимитов пишет «сбой» или «частичный сбой» со ссылкой на страницу, а маршрутизация берёт другого кандидата, если он есть;
- следить за лимитами провайдеров по пятичасовому, недельному и месячному окнам и за скоростью ответа; настраивать пороги жёлтого и красного предупреждений и недельный резерв — изменения действуют сразу, без перезапуска хаба; видеть прогноз расхода по темпу и обратный отсчёт до сброса, а маршрутизация пропускает кандидата, которому не хватит лимита на ожидаемую длительность задачи;
- видеть в сведениях о сессии расход токенов за всю сессию и всех её делегатов, по моделям и с кэшем;
- вставлять скриншоты и файлы из буфера или перетаскиванием; агент может отдать файл в ленту;
- открывать вложенные `.md`, документы Word (`.docx`) и таблицы Excel (`.xlsx`) прямо в приложении: отрендеренный текст, листы с закреплённой шапкой, копирование и скачивание;
- получать пуши на телефон через Gotify (`[notify.gotify]`): сессия ждёт ответа или разрешения, ход завершён, сбой, зависание или пауза по лимиту; касание открывает чат;
- видеть свободное место на машинах и тихое предупреждение в списке сессий, когда его меньше `[machines] disk_warn_free_gb = 30` ГБ (критический уровень — `disk_critical_free_gb = 10`);
- следить за машиной через сторожа: самые крупные кэши сборки с подсказкой, как их убрать, нехватка памяти (`mem_warn_gb`), процессы, пережившие свою сессию, и сборки вне очереди; об этом слышат и активные оркестраторы на машине;
- видеть очередь тяжёлых задач машины: кто держит гейт или сборку и сколько, кто ждёт и на каком месте; сессия в очереди пишет «Ждёт очереди на сборку · 2-й» и не считается подвисшей, а об аренде, занятой дольше 90 минут, оркестраторы на машине слышат один раз;
- запускать сессию в режиме «только чтение»: выход из режима планирования canterlot отклоняет сам, не спрашивая владельца, и сессия заканчивает ход ответом;
- выбирать одну из пятнадцати тем по карточкам с предпросмотром, в тёмном и светлом режиме; новая тема плавно раскрывается от нажатой кнопки;
- видеть версию в настройках, окно «Что нового» и историю изменений.

## Скриншоты

Телефон и рабочий стол: темы Аквамарин, Стандартная, VS Code и Норд, тёмные и светлые; остальные темы — в настройках.

<table align="center">
  <tr>
    <td align="center" valign="top"><a href="docs/screenshots/sessions-dark.png"><img src="docs/screenshots/sessions-dark.png" width="240" alt="Список сессий с лимитами и группами"></a><br><sub>Сессии, лимиты и группы</sub></td>
    <td align="center" valign="top"><a href="docs/screenshots/chat-dark.png"><img src="docs/screenshots/chat-dark.png" width="240" alt="Чат с кодом, инструментами и заполнением контекста"></a><br><sub>Чат, код и инструменты</sub></td>
    <td align="center" valign="top"><a href="docs/screenshots/permission-light.png"><img src="docs/screenshots/permission-light.png" width="240" alt="Карточка разрешения на команду"></a><br><sub>Разрешения агента</sub></td>
  </tr>
  <tr>
    <td align="center" valign="top"><a href="docs/screenshots/question-dark.png"><img src="docs/screenshots/question-dark.png" width="240" alt="Карточка вопроса с вариантами ответа"></a><br><sub>Вопросы с вариантами</sub></td>
    <td align="center" valign="top"><a href="docs/screenshots/plan-dark.png"><img src="docs/screenshots/plan-dark.png" width="240" alt="Вкладка «План»: задачи по состояниям, исполнители, находки и коммиты"></a><br><sub>Доска плана</sub></td>
    <td align="center" valign="top"><a href="docs/screenshots/plan-feed-light.png"><img src="docs/screenshots/plan-feed-light.png" width="240" alt="Лента оркестратора с карточками запуска делегата и правки плана"></a><br><sub>Карточки плана в ленте</sub></td>
  </tr>
  <tr>
    <td align="center" valign="top"><a href="docs/screenshots/delegation-dark.png"><img src="docs/screenshots/delegation-dark.png" width="240" alt="Панель делегирования: делегаты и их состояние"></a><br><sub>Панель делегирования</sub></td>
    <td align="center" valign="top"><a href="docs/screenshots/search-light.png"><img src="docs/screenshots/search-light.png" width="240" alt="Поиск по истории с подсвеченными совпадениями"></a><br><sub>Поиск по истории</sub></td>
    <td align="center" valign="top"><a href="docs/screenshots/machines-dark.png"><img src="docs/screenshots/machines-dark.png" width="240" alt="Машина: ресурсы, сторож, крупные кэши и очередь тяжёлых задач"></a><br><sub>Машина, сторож и очередь</sub></td>
  </tr>
  <tr>
    <td align="center" valign="top" colspan="3"><a href="docs/screenshots/limits-light.png"><img src="docs/screenshots/limits-light.png" width="200" alt="История лимитов и скорость моделей"></a><br><sub>История лимитов и скорость</sub></td>
  </tr>
  <tr>
    <td align="center" colspan="3"><a href="docs/screenshots/desktop.png"><img src="docs/screenshots/desktop.png" width="760" alt="Рабочий стол: список сессий, чат оркестратора и доска плана"></a><br><sub>Рабочий стол: список, чат и план рядом</sub></td>
  </tr>
</table>

## Сборка выпуска

Выпуск собирается один раз в контейнере Debian 13 на отдельной машине, а готовый
каталог затем используют и выкат, и публикация. Собирать напрямую, без контейнера,
нельзя ни на этой VM, ни на сборочной машине: прод — Debian 13 с glibc 2.41, а
сборочная машина — Arch с glibc 2.44, и собранные там бинарники на проде не запустятся.

```bash
scripts/build-release.sh v1.24.0            # сборочная машина (CANTERLOT_BUILD_HOST, по умолчанию canterlot-build)
scripts/build-release.sh v1.24.0 --local    # та же сборка на местном docker, для проверки
```

Тег должен быть локально и на origin. На сборочной машине нужны ssh-доступ (алиас
`canterlot-build`), работающий docker и bare-репозиторий `~/canterlot.git`. Скрипт:

1. отправляет тег в bare-репозиторий `~/canterlot.git` на сборочной машине;
2. создаёт там чистую рабочую копию `~/release/src-<sha12>` на коммите тега;
3. собирает или переиспользует образ `canterlot-release-build:<hash Dockerfile + rust-toolchain.toml>`
   из `docker/release-build/Dockerfile`: Debian trixie по digest, Rust из
   `rust-toolchain.toml`, Node 24 с проверкой sha256;
4. запускает контейнер от uid хоста с ограничениями `--cpus 24 --memory 40g --pids-limit 4096`
   (systemd-лимиты сборочной машины на контейнеры dockerd не действуют);
5. переносит готовый каталог по ssh tar-потоком в
   `.superpowers/deploy/dist/<sha12>` этой рабочей копии (`CANTERLOT_DIST_ROOT`),
   проверяет SHA256SUMS, `RELEASE`, `--version` и состав файлов; существующий
   каталог не перезаписывается;
6. убирает рабочую копию на сборочной машине через `git worktree remove --force`.

Внутри контейнера `scripts/release-build/in-container.sh` собирает хаб, воркер,
синтетический воркер и PWA (`npm ci`, `PWA_VERSION=<sha12> npm run build`) и проверяет
то же, что выкат: отсутствие libssl, `hub openapi` против `docs/api/openapi.json`,
CSP, `_app/version.json`, `synthetic-cli` только в синтетическом воркере, SHA256SUMS
и максимальную версию GLIBC не выше 2.41. Тестов здесь нет: полный прогон
`scripts/check.sh` и `check-web.sh` уже прошёл на этом коммите.

Кэши: на сборочной машине `~/release/cache/{cargo,target,npm}`, локально
`~/.cache/canterlot-release-build` (`CANTERLOT_BUILD_CACHE`). Повторная сборка того
же коммита инкрементальная. Сборочный инструмент берётся из этой рабочей копии, а
не из тега: первый выпуск после появления скрипта собирает тег, в котором его ещё нет.

Публикация — из того же каталога:

```bash
scripts/publish-release.sh v1.24.0
```

Выкат production — только через `scripts/deploy` (см. §11 `docs/deploy/pilot.md`, ранбук — §11.7): он проверяет готовый dist из `build-release.sh`, `cutover.sh <id> --dry-run` или `--rehearse` (весь путь записи, записи только печатаются) выдаёт токен, а боевой прогон `cutover.sh --execute <id> --confirm <токен>` принимает его 2 часа и только для того же плана. Значения своего стенда — в `.superpowers/site.env`, образец в [docs/deploy/site.env.example](docs/deploy/site.env.example).

### Полный гейт на сборочной машине

Тот же прогон, что перед выпуском, можно снять на сборочной машине
(`CANTERLOT_BUILD_HOST`) в контейнере Debian 13: быстрее, чем на рабочей VM, и без
влияния на неё.

```bash
scripts/gate-remote.sh 4d884712            # коммит на сборочной машине (CANTERLOT_BUILD_HOST)
scripts/gate-remote.sh 4d884712 --local    # то же на местном docker, для проверки
```

`REV` — любой коммит, не только тег (по умолчанию `HEAD`). Скрипт отправляет его
в `~/canterlot.git` как `refs/gate/<sha12>`, создаёт отсоединённую рабочую копию
`~/gate/src-<sha12>` и запускает в контейнере ровно три шага `/tmp/gate/run.sh`:

1. `VITE_UI_TESTS=1 npm run build` (после своего `npm ci`);
2. `scripts/check.sh --all` — fmt, clippy, nextest с тестовой PostgreSQL;
3. `scripts/check-web.sh --stack` — проверки web, Playwright и сквозные сценарии
   на настоящем хабе и воркере.

Образ `canterlot-gate:<hash>` (`docker/gate/Dockerfile`) — Debian trixie по digest,
Rust из `rust-toolchain.toml`, Node 24, PostgreSQL 18 (как у тестовой базы VM),
браузеры Playwright из `web/package-lock.json` и те же шрифты, что на VM. Тег
считается из Dockerfile, `rust-toolchain.toml` и версии Playwright. База
поднимается внутри контейнера на `127.0.0.1:55441` с выключенным fsync; на host
`55441` не занимается, поэтому гейт на VM можно запускать одновременно.

Ограничения контейнера — `--cpus 30 --memory 48g --pids-limit 8192 --shm-size=2g`:
у 7950X 32 потока и 61 ГБ, два потока и ~13 ГБ остаются хосту. Общая память
браузеров — 2 ГБ вместо 64 МБ по умолчанию: иначе Firefox не доводит страницы до
`[data-ready]` и тесты падают по таймауту. Число браузерных воркеров Playwright
считается от квоты CPU (один на ~4 CPU: `PW_WORKERS`), а `strace`-тестам выдаётся
`SYS_PTRACE`; `--init` пожинает осиротевших потомков. Кэши на сборочной машине —
`~/gate/cache/{cargo,target,npm,playwright,tmp}`, локально —
`~/.cache/canterlot-gate` (`CANTERLOT_GATE_CACHE`); повторный прогон того же
коммита их переиспользует. `tmp` — это `/tmp` контейнера (база, временные
каталоги тестов и профили браузеров): на overlayfs и на tmpfs те же проверки
ловят таймауты, на обычной ФС хоста проходят.

Результат и логи (`build.log`, `check.log`, `web.log`, `result.txt` в формате
`/tmp/gate/run.sh`) складываются в `/tmp/gate-remote/<sha12>`
(`CANTERLOT_GATE_OUT`). Код возврата — код гейта. Рабочая копия и ref на
сборочной машине убираются автоматически (`git worktree remove --force` и
`update-ref -d`, без `rm`).

Гейт на самой VM (`/tmp/gate/run.sh` с `CANTERLOT_TEST_PG_PORT=55441`) продолжает
работать и остаётся эталоном: контейнер повторяет его шаги.

## Развёртывание из готовых архивов

В [выпусках на GitHub](https://github.com/Fenicu/canterlot/releases) лежат собранные хаб, воркер и PWA. Хаб и воркер собраны для x86_64 под glibc 2.41 и новее (Debian 13, Ubuntu 25.04 и новее). Подробности каждого шага — в [docs/deploy/pilot.md](docs/deploy/pilot.md), ключи конфигурации — в [docs/configuration.md](docs/configuration.md). Собрать всё из исходников можно по [docs/development.md](docs/development.md).

Понадобятся:

- PostgreSQL 18 с базой в UTF-8;
- Caddy (TLS, статика PWA и обратный прокси к хабу); SSO не нужен, вход проверяет сам хаб;
- на рабочих машинах — systemd и CLI агентов, которыми будете пользоваться: `claude`, `codex`, `opencode`, `agy`.

Поддерживаемые версии CLI (более новая запускается, но воркер пишет диагностику `untested_cli_version`):

| Агент | CLI | Минимум | Проверены |
|---|---|---|---|
| Claude Code | `claude` | 2.1.311 | 2.1.311 |
| Codex | `codex` | 0.164.0 | 0.164.0, 0.167.0 |
| OpenCode | `opencode` | 2.1.4 | 2.1.4, 2.2.5, 2.2.6 (ветка 1.x не поддерживается) |
| Antigravity | `agy` | 1.4.12 | 1.4.12, 1.5.0 |

**1. Скачать и проверить.**

```bash
VER=$(curl -fsS https://api.github.com/repos/Fenicu/canterlot/releases/latest | jq -r .tag_name | sed 's/^v//')
base=https://github.com/Fenicu/canterlot/releases/download/v$VER
for f in SHA256SUMS canterlot-hub-$VER-x86_64-linux-gnu.tar.gz \
         canterlot-worker-$VER-x86_64-linux-gnu.tar.gz \
         canterlot-web-$VER.tar.gz canterlot-csp-$VER.txt; do
    curl -fLO "$base/$f"
done
sha256sum -c SHA256SUMS
```

**2. Хаб.** Установите бинарник и создайте системного пользователя:

```bash
sudo tar -xzf canterlot-hub-$VER-x86_64-linux-gnu.tar.gz -C /usr/local/bin
sudo useradd --system --home-dir /var/lib/canterlot --shell /usr/sbin/nologin canterlot
```

Дальше по [docs/deploy/pilot.md](docs/deploy/pilot.md):

- роль и база PostgreSQL, строка подключения в `/etc/canterlot/db.url` (§2.2);
- `/etc/canterlot/hub.toml` с таблицей `[http]`: `trusted_proxies` — адрес Caddy, `owner_login` — логин для страницы входа, `allowed_origins` — адрес PWA (§2.4);
- секрет прокси, общий для хаба и Caddy (§2.5);
- сертификат gRPC от своего УЦ, имя хоста хаба в SAN (§3);
- по желанию — пуши на телефон через Gotify: таблица `[notify.gotify]` с `url` сервера и `token_file` с ключом приложения; события включаются ключами `waiting`, `turn_finished` и `failures` ([docs/configuration.md](docs/configuration.md)).

Затем создайте каталоги из §2.6, один раз инициализируйте хаб и включите службу из §2.7:

```bash
sudo -u canterlot canterlot-hub init --config /etc/canterlot/hub.toml
sudo systemctl enable --now canterlot-hub
```

`init` применяет миграции и создаёт владельца. При обновлении хаб сам применяет новые миграции на старте.

**Вход.** Пароля сразу после установки нет: первый вход — одноразовая ссылка восстановления, выпущенная на хосте хаба.

```bash
sudo -u canterlot canterlot-hub auth recovery-link --config /etc/canterlot/hub.toml          # печатает ссылку
sudo -u canterlot canterlot-hub auth recovery-link --config /etc/canterlot/hub.toml --push   # шлёт её только в Gotify
```

Ссылка живёт 10 минут (`--minutes`, до 60) и открывается один раз. После входа задайте пароль или добавьте ключ доступа в «Настройки → Сеансы и вход». Там же список сеансов с устройством, адресом и местом, завершение любого из них, срок бездействия (по умолчанию 90 дней) и «Сканировать код» для входа по QR на новом устройстве. Те же действия на хосте: `auth set-password` (из TTY без эха или `--stdin`, `--revoke-sessions` завершает все сеансы), `auth sessions`, `auth revoke-all`. Подробности — в [docs/http.md](docs/http.md) и [ADR-0005](docs/adr/0005-owner-sign-in.md).

Место сеанса хаб определяет сам по локальной базе DB-IP City Lite (CC BY 4.0); скачайте её и обновляйте раз в месяц:

```bash
sudo -u canterlot canterlot-hub geoip update --config /etc/canterlot/hub.toml   # в [auth.geoip].path, прежний файл остаётся при ошибке
```

**3. PWA и Caddy.** Разверните статику и подставьте свой адрес PWA в CSP:

```bash
sudo mkdir -p /var/www/canterlot/build
sudo tar -xzf canterlot-web-$VER.tar.gz -C /var/www/canterlot/build
sed 's/chat\.example\.com/chat.your.domain/' canterlot-csp-$VER.txt
```

Строку передайте Caddy в переменной окружения `CANTERLOT_CSP`. Блок сайта возьмите из §4.2 и [docs/http.md](docs/http.md#caddy): порядок директив внутри `route` важен, чистка `Remote-*`, `X-Forwarded-*`, `X-Canterlot-Proxy` и `X-Canterlot-Client-Ip` идёт до проксирования. Хэши скриптов в CSP привязаны к версии, поэтому при обновлении PWA строку CSP тоже обновляйте.

**4. Машина с воркером.** На хабе выпустите параметры машины:

```bash
sudo -u canterlot canterlot-hub machine add --config /etc/canterlot/hub.toml --name workstation
```

Команда печатает `machine_id`, `installation_id`, `restore_generation` и `token`. Токен показывается один раз: сохраните его на машине в `~/.config/canterlot/token` с правами `0600`, туда же положите `ca.pem` своего УЦ. Затем на машине воркера:

```bash
sudo tar -xzf canterlot-worker-$VER-x86_64-linux-gnu.tar.gz -C /usr/local/bin
```

`~/.config/canterlot/worker.toml` заполните по §6.2 и [docs/configuration.md](docs/configuration.md): адрес хаба `https://<хаб>:7443`, три UUID, `allowed_workdir_roots` и таблицы нужных агентов (`[claude_code]`, `[codex]`, `[opencode]`, `[antigravity]`). Таблица `[machine]` задаёт ёмкость очереди тяжёлых задач (`lease_slots`) и где сторож ищет кэши сборки (`hotspot_roots`). Пользовательская служба — из §6.4, в ней обязателен `KillMode=process`: с ним идущие сессии переживают перезапуск воркера.

```bash
loginctl enable-linger "$USER"
systemctl --user enable --now canterlot-worker
```

**Обновление.** Скачайте новую версию тем же способом. Сначала замените бинарник хаба и перезапустите его, затем PWA с новым CSP, затем бинарник воркера: `systemctl --user restart canterlot-worker`. Идущие сессии при этом не прерываются. Откатить хаб на версию со старой схемой базы нельзя, поэтому перед обновлением сделайте дамп.

## Документация

- [Разработка](docs/development.md) — тулчейн, структура репозитория, проверки, тесты и обновление контракта.
- [Конфигурация](docs/configuration.md) — `hub.toml` и `worker.toml`, ключи, пуши Gotify, пороги машин и лимитов, версии CLI, запуск, отзыв и ротация машин.
- [HTTP и вход](docs/http.md) — вход владельца, сеансы, CSRF, Caddy и WSS.
- [PWA](docs/pwa.md) — модули клиента, статическая публикация (CSP и `wss://<origin-host>`), установка и обновление.
- [Развёртывание](docs/deploy/pilot.md) — пилотный контур: Proxmox LXC, PostgreSQL, Caddy, Authelia.
- [Контракт API](docs/design/api.md) — маршруты, ошибки и события; машинные схемы в `docs/api/`.
- [Проектирование](docs/design/README.md) — автоматы состояний, порядок блокировок, идемпотентность и восстановление.
- [Версионирование](docs/versioning.md) — правила бампа и проверка версии.

## Версии

Текущая версия — 1.31.0. История изменений для человека — [CHANGES.rst](CHANGES.rst), правила бампа — [docs/versioning.md](docs/versioning.md).

Архивы выпуска публикует `scripts/publish-release.sh vX.Y.Z` из сборки, сделанной при выкатке, без повторной компиляции.

## Лицензии

Canterlot распространяется по лицензии MIT, текст — [LICENSE](LICENSE).

Сторонние зависимости и их лицензии — [docs/licenses/THIRD_PARTY.md](docs/licenses/THIRD_PARTY.md).
