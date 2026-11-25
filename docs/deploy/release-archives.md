# Развёртывание из готовых архивов

В [выпусках на GitHub](https://github.com/Fenicu/canterlot/releases) лежат собранные хаб, воркер и PWA. Хаб и воркер собраны для x86_64 под glibc 2.41 и новее (Debian 13, Ubuntu 25.04 и новее). Подробности каждого шага — в [pilot.md](pilot.md), ключи конфигурации — в [конфигурации](../configuration.md). Собрать всё из исходников можно по [development.md](../development.md).

Понадобятся:

- PostgreSQL 18 с базой в UTF-8;
- Caddy (TLS, статика PWA и обратный прокси к хабу); SSO не нужен, вход проверяет сам хаб;
- на рабочих машинах — systemd и CLI агентов, которыми будете пользоваться: `claude`, `codex`, `opencode`.

Поддерживаемые версии CLI (более новая запускается, но воркер пишет диагностику `untested_cli_version`):

| Агент | CLI | Минимум | Проверены |
|---|---|---|---|
| Claude Code | `claude` | 2.1.311 | 2.1.311, 2.1.332 |
| Codex | `codex` | 0.164.0 | 0.164.0, 0.167.0, 0.169.0, 0.172.0 |
| OpenCode | `opencode` | 2.1.4 | 2.1.4, 2.2.5, 2.2.6, 2.4.1 (ветка 1.x не поддерживается) |

Первоисточник таблицы — разделы адаптеров в [конфигурации](../configuration.md).

## 1. Скачать и проверить

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

## 2. Хаб

Установите бинарник и создайте системного пользователя:

```bash
sudo tar -xzf canterlot-hub-$VER-x86_64-linux-gnu.tar.gz -C /usr/local/bin
sudo useradd --system --home-dir /var/lib/canterlot --shell /usr/sbin/nologin canterlot
```

Дальше по [pilot.md](pilot.md):

- роль и база PostgreSQL, строка подключения в `/etc/canterlot/db.url` (§2.2);
- `/etc/canterlot/hub.toml` с таблицей `[http]`: `trusted_proxies` — адрес Caddy, `owner_login` — логин для страницы входа, `allowed_origins` — адрес PWA (§2.4);
- секрет прокси, общий для хаба и Caddy (§2.5);
- сертификат gRPC от своего УЦ, имя хоста хаба в SAN (§3);
- по желанию — пуши на телефон через Gotify: таблица `[notify.gotify]` с `url` сервера и `token_file` с ключом приложения; события включаются ключами `waiting`, `turn_finished` и `failures`, а `turn_finished` шлётся на итог запроса владельца, а не на каждый служебный ход ([конфигурация](../configuration.md)).

Затем создайте каталоги из §2.6, один раз инициализируйте хаб и включите службу из §2.7:

```bash
sudo -u canterlot canterlot-hub init --config /etc/canterlot/hub.toml
sudo systemctl enable --now canterlot-hub
```

`init` применяет миграции и создаёт владельца. При обновлении хаб сам применяет новые миграции на старте.

### Вход

Пароля сразу после установки нет: первый вход — одноразовая ссылка восстановления, выпущенная на хосте хаба.

```bash
sudo -u canterlot canterlot-hub auth recovery-link --config /etc/canterlot/hub.toml          # печатает ссылку
sudo -u canterlot canterlot-hub auth recovery-link --config /etc/canterlot/hub.toml --push   # шлёт её только в Gotify
```

Ссылка живёт 10 минут (`--minutes`, до 60) и открывается один раз. После входа задайте пароль или добавьте ключ доступа в «Настройки → Сеансы и вход». Там же список сеансов с устройством, адресом и местом, завершение любого из них, срок бездействия (по умолчанию 90 дней) и «Сканировать код» для входа по QR на новом устройстве. Те же действия на хосте: `auth set-password` (из TTY без эха или `--stdin`, `--revoke-sessions` завершает все сеансы), `auth sessions`, `auth revoke-all`. Подробности — в [http.md](../http.md) и [ADR-0005](../adr/0005-owner-sign-in.md).

Место сеанса хаб определяет сам по локальной базе DB-IP City Lite (CC BY 4.0); скачайте её и обновляйте раз в месяц:

```bash
sudo -u canterlot canterlot-hub geoip update --config /etc/canterlot/hub.toml   # в [auth.geoip].path, прежний файл остаётся при ошибке
```

### Общая память: разовый импорт файлов памяти Claude

Порядок целиком — в [pilot.md](pilot.md), §13:

```bash
canterlot-worker memory import-plan --root ~/.claude/projects --out manifest.toml   # 1. план; владелец просматривает manifest.toml (2)
canterlot-worker memory import-bundle --manifest manifest.toml --out bundle.json    # 3. пакет; отказ при расхождении хэша
sudo -u canterlot canterlot-hub memory import --config /etc/canterlot/hub.toml --bundle bundle.json --dry-run   # 4. сухой прогон
sudo -u canterlot canterlot-hub memory import --config /etc/canterlot/hub.toml --bundle bundle.json             # 5. применение
```

## 3. PWA и Caddy

Разверните статику и подставьте свой адрес PWA в CSP:

```bash
sudo mkdir -p /var/www/canterlot/build
sudo tar -xzf canterlot-web-$VER.tar.gz -C /var/www/canterlot/build
sed 's/chat\.example\.com/chat.your.domain/' canterlot-csp-$VER.txt
```

Строку передайте Caddy в переменной окружения `CANTERLOT_CSP`. Блок сайта возьмите из §4.2 [pilot.md](pilot.md) и [http.md](../http.md#caddy): порядок директив внутри `route` важен, чистка `Remote-*`, `X-Forwarded-*`, `X-Canterlot-Proxy` и `X-Canterlot-Client-Ip` идёт до проксирования. Хэши скриптов в CSP привязаны к версии, поэтому при обновлении PWA строку CSP тоже обновляйте.

## 4. Машина с воркером

На хабе выпустите параметры машины:

```bash
sudo -u canterlot canterlot-hub machine add --config /etc/canterlot/hub.toml --name workstation
```

Команда печатает `machine_id`, `installation_id`, `restore_generation` и `token`. Токен показывается один раз: сохраните его на машине в `~/.config/canterlot/token` с правами `0600`, туда же положите `ca.pem` своего УЦ. Затем на машине воркера:

```bash
sudo tar -xzf canterlot-worker-$VER-x86_64-linux-gnu.tar.gz -C /usr/local/bin
```

`~/.config/canterlot/worker.toml` заполните по §6.2 [pilot.md](pilot.md) и [конфигурации](../configuration.md): адрес хаба `https://<хаб>:7443`, три UUID, `allowed_workdir_roots` и таблицы нужных агентов (`[claude_code]`, `[codex]`, `[opencode]`). Таблица `[machine]` задаёт ёмкость очереди тяжёлых задач (`lease_slots`) и где сторож ищет кэши сборки (`hotspot_roots`). Таблица `[memory]` задаёт рабочие корни общей памяти (`work_roots`, по умолчанию `["~/work"]`). Таблица `[jobs]` настраивает задачи на месте, команды оболочки по просьбе хаба (`jobs_inplace_v1`): `max_running` (4), `log_max_mib` (16), `state_retention` (`"3d"`). Пользовательская служба — из §6.4, в ней обязателен `KillMode=process`: с ним идущие сессии переживают перезапуск воркера.

```bash
loginctl enable-linger "$USER"
systemctl --user enable --now canterlot-worker
```

### Раннеры

Раннер принимает только задачи, без сессий. Машину с воркером помечают несущей `canterlot-hub machine set-carrier --machine-id <uuid>`, раннер создают `canterlot-hub machine add --name runner-a --role runner --parent <uuid несущей> --root-dir /srv/runner --rank 20 --labels build-host`. Хаб не запускает на раннере сессии, а токен раннера выдаёт только своей несущей; подробности — в [конфигурации](../configuration.md).

`job_run` с `source` {`repo`, `rev`} и `machine: "auto"` отдаёт выбор места хабу: задача идёт на свободный раннер несущей (больший ранг, нужные `labels`) или на саму несущую, иначе ждёт в `waiting_runner`. Несущая готовит ровно этот коммит (`staging`) и после запуска забирает `outputs` (`collecting`); итог приходит один раз после сбора (очистку исходников он не ждёт). Названная машина обязана нести все `labels` задачи; ждущая раннер задача заканчивается через `[jobs].waiting_timeout` (24 часа) как `canceled` с причиной `no_runner`.

Ранг, метки и корень меняет `canterlot-hub machine set --machine-id <uuid> [--rank N] [--labels a,b] [--root-dir P]`. Инструмент `runners_list` показывает раннеры и занятость. Задачу владельца без разговора (с исходниками на нужном коммите и сбором результатов) отправляет на несущей `canterlot-worker job run --title gate --source @HEAD --output result --wait -- ./run.sh`: выход с кодом задачи, каталог результатов в `~/.local/state/canterlot-worker/job-outputs/<id>`. Устройство — в [design/runners.md](../design/runners.md), облачные окна раннеров в Yandex Cloud — в [конфигурации](../configuration.md) и §7 того же документа.

## 5. Вторая машина: оркестратор здесь, разработка там

Оркестратор (Claude Code или Codex) остаётся на основной машине, а делегаты, сборки, запуски и выкатка идут на второй через её воркер. Между машинами нет ssh: воркер второй машины сам подключается к хабу, ему нужен только исходящий доступ к порту воркеров хаба. Код остаётся на второй машине; через хаб идут ленты сессий и вывод команд, которые запросил оркестратор.

Выпустите машину тем же `machine add --name machine-b` и поставьте воркер, как в п. 4. В `worker.toml` перечислите только агентов, которыми canterlot должен пользоваться. Агент без своей таблицы не виден и не запускается, его лимиты не опрашиваются; без `[claude_code]` установленный там Claude Code игнорируется целиком, включая его истории в `~/.claude`. Если подписка у агента там другая, дайте ей свою метку `account`, чтобы её лимиты не смешивались с лимитами основной машины:

```toml
allowed_workdir_roots = ["/srv/projects"]

[opencode]
account = "b-opencode"

[memory]
work_roots = ["/srv/projects/work"]
```

`[memory] work_roots` задайте явно: без него рабочим считается каталог `work` в домашнем каталоге пользователя воркера. Домен памяти каталога на второй машине должен совпадать с доменом оркестратора, поэтому оркестратора рабочего проекта запускайте на основной машине тоже из рабочего каталога. Лимиты OpenCode там опрашивает CodexBar второй машины; без него остаются только показания живых сессий.

На карточке машины в PWA включите «Принимать делегатов и команды с других машин»: по умолчанию машина работу с других машин не берёт, выключение отказывает ещё не начатым запускам и задачам, начатые доходят до конца. Дальше оркестратор работает так:

- `session_spawn` с `machine: "machine-b"` и `workdir` на второй машине запускает там делегата, в том числе в отдельной рабочей копии (`isolated`); список агентов, лимиты и свободное место берутся со второй машины;
- `job_run` с `machine`, `workdir` и `shell` выполняет команду на месте в каталоге второй машины (диф, слияние, гейт, выкатка); короткая отвечает в вызове, длинная присылает итог сообщением. Вызывать его может только сессия в режиме bypass;
- `session_accept` убирает рабочую копию делегата на второй машине.

Воркер второй машины обновляйте вручную после каждого выпуска: замените бинарник и перезапустите службу, версия видна на экране «Машины». Пока он старый, новые возможности на нём недоступны, а хаб отвечает `capability_missing`. Бинарник выпуска собран на Debian 13 и требует glibc не старше её; на более старой системе соберите воркер на самой машине (`cargo build --release -p canterlot-worker`).

## Обновление

Скачайте новую версию тем же способом. Сначала замените бинарник хаба и перезапустите его, затем PWA с новым CSP, затем бинарник воркера: `systemctl --user restart canterlot-worker`. Идущие сессии при этом не прерываются. Откатить хаб на версию со старой схемой базы нельзя, поэтому перед обновлением сделайте дамп.
