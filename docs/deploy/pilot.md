# Инструкция по развёртыванию пилота Canterlot

Настоящее руководство описывает архитектуру и шаги развёртывания пилотного контура Canterlot: хаба с базой данных PostgreSQL в изолированном контейнере Proxmox LXC, воркера на рабочей виртуальной машине владельца и единой точки входа на обратном прокси Caddy. Владельца проверяет сам хаб: страница входа PWA, пароль или ключ доступа, сеанс в cookie ([ADR-0005](../adr/0005-owner-sign-in.md)); Authelia и вход на прокси не нужны.

Все параметры конфигурации и команды CLI сопоставлены с исходным кодом проекта (`crates/canterlot-hub`, `crates/canterlot-worker`, `web`, `docs/design`).

---

## 1. Схема архитектуры и используемые адреса

### 1.1. Таблица сетевых параметров и заполнителей

В руководстве используются следующие типовые значения (заменяются на фактические параметры инфраструктуры):

| Параметр | Значение в примере | Назначение |
|---|---|---|
| Адрес хаба (LXC) | `10.0.0.10` | Внутренний IP-адрес контейнера Proxmox LXC с хабом и PostgreSQL |
| Адрес хоста Caddy | `10.0.0.2` | Внутренний IP-адрес отдельного сервера Web/Edge с Caddy |
| Адрес машины воркера | `10.0.0.20` | Внутренний IP-адрес рабочей VM владельца с воркером и агентами |
| Публичное имя сайта | `chat.example.com` | Внешний HTTPS-домен для доступа к PWA и API |
| Внутреннее DNS-имя хаба | `hub.example.internal` | Доменное имя хаба для gRPC TLS (указано в сертификате SAN) |

### 1.2. Архитектурная схема

Размещение компонентов строго следует решениям [pilot.md](../design/pilot.md#2-размещение) (§2) и [ADR-0005](../adr/0005-owner-sign-in.md):

```
+-----------------------------------------------------------------------------------+
| Владелец: Браузер (Firefox Desktop / Chrome Samsung S25 Ultra)                    |
+-----------------------------------------------------------------------------------+
                                    | HTTPS / WSS
                                    v
+-----------------------------------------------------------------------------------+
| Сервер Web / Edge (Внешняя сеть / DMZ, IP: 10.0.0.2)                              |
|                                                                                   |
|  +-----------------------------------------------------------------------------+  |
|  | Caddy (Обратный прокси и раздача статики PWA)                                |  |
|  |                                                                             |  |
|  |  1. file_server: отдача SPA сборки web/build/ (try_files {path} /200.html)   |  |
|  |  2. Content-Security-Policy (хэши инлайн-скриптов из CANTERLOT_CSP)          |  |
|  |  3. route: зачистка Remote-*, X-Forwarded-*, X-Canterlot-Proxy,             |  |
|  |     X-Canterlot-Client-Ip (навсегда: правило отката ADR-0005)               |  |
|  |  4. входа на прокси нет: сеанс владельца проверяет хаб по cookie            |  |
|  |  5. reverse_proxy: проксирование @api (/api/* и /healthz) на порт хаба      |  |
|  |     + X-Canterlot-Proxy (секрет) и X-Canterlot-Client-Ip (адрес клиента)    |  |
|  +-----------------------------------------------------------------------------+  |
|                                                    | HTTP / WSS                   |
+----------------------------------------------------+------------------------------+
                                                     |
                         Внутренняя сеть (LAN / VLAN)|
                                                     v
+-----------------------------------------------------------------------------------+
| Proxmox LXC: Среда хаба Canterlot (IP: 10.0.0.10)                                 |
|                                                                                   |
|  +-----------------------------------------------------------------------------+  |
|  | nftables: порт 8080 только с 10.0.0.2, порт 7443 только с 10.0.0.20         |  |
|  +-----------------------------------------------------------------------------+  |
|  | canterlot-hub (systemd system service: canterlot-hub.service)               |  |
|  |                                                                             |  |
|  |  - HTTP API: 10.0.0.10:8080 (только для Caddy, проверка IP и секрета)       |  |
|  |  - gRPC TLS: 10.0.0.10:7443 (серверный TLS с SAN, bearer-токены машин)      |  |
|  |  - Вложения: локальный каталог data/attachments (staging / blobs)            |  |
|  +-----------------------------------------------------------------------------+  |
|           |                                       |                               |
|           v Unix сокет / TCP loopback             v Независимый том               |
|  +-----------------------------+     +-----------------------------------------+  |
|  | PostgreSQL 18 (Debian 13)   |     | Внешний манифест инсталляции:           |  |
|  | (БД canterlot, миграции SQL)|     | /etc/canterlot/manifest/installation.toml|  |
|  +-----------------------------+     | (ВНЕ набора снапшотов базы данных)      |  |
|                                      +-----------------------------------------+  |
+-----------------------------------------------------------------------------------+
                                                     ^
                         gRPC over TLS (Серверный TLS| без входящих портов у воркера)
                                                     |
+-----------------------------------------------------------------------------------+
| Proxmox VM: Рабочая станция владельца (IP: 10.0.0.20)                             |
|                                                                                   |
|  +-----------------------------------------------------------------------------+  |
|  | canterlot-worker (systemd user service: canterlot-worker.service)           |  |
|  |                                                                             |  |
|  |  - KillMode=process (сохранение сопровождающих процессов при рестарте)      |  |
|  |  - data_dir: ~/.local/state/canterlot-worker (права 0700)                   |  |
|  |  - Журнал управления (CJ) (0600); токен в token_file (0600)                 |  |
|  +-----------------------------------------------------------------------------+  |
|           |                                       |                               |
|           v Управление процессом                  v Управление процессом          |
|  +-----------------------------------+   +------------------------------------+   |
|  | canterlot-worker supervise        |   | canterlot-worker supervise         |   |
|  | (Отдельная сессия ОС via setsid)  |   | (Отдельная сессия ОС via setsid)   |   |
|  |  - Локальный журнал сессии (SJ)   |   |  - Локальный журнал сессии (SJ)    |   |
|  |  - Дочерний процесс: claude CLI   |   |  - Дочерний процесс: codex CLI     |   |
|  |  - Буферизация вывода при рестарте|   |  - Буферизация вывода при рестарте |   |
|  +-----------------------------------+   +------------------------------------+   |
+-----------------------------------------------------------------------------------+
```

---

## 2. Хаб в Proxmox LXC

Хаб и база данных PostgreSQL изолированы в выделенном контейнере Proxmox LXC ([docs/design/pilot.md#2-размещение](../design/pilot.md#2-размещение) §2). Подложка **Debian 13 (trixie)** и PostgreSQL 18 — выбор этого руководства, повторяющий тестовый контур проекта ([scripts/test-db.sh:21](../../scripts/test-db.sh#L21) использует образ `postgres:18.6-trixie`); контейнер рекомендуется создавать непривилегированным.

### 2.1. Пакеты ОС и установка PostgreSQL 18

Тестовый контур проекта ориентирован на PostgreSQL 18 ([scripts/test-db.sh:21](../../scripts/test-db.sh#L21) использует образ `postgres:18.6-trixie`). Внутри LXC настраивается официальный репозиторий PGDG (PostgreSQL Global Development Group):

```bash
apt-get update && apt-get install -y curl ca-certificates gnupg

# 1. Подключение ключа и репозитория PGDG
install -d /etc/apt/keyrings
curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc | gpg --dearmor -o /etc/apt/keyrings/postgresql.gpg
echo "deb [signed-by=/etc/apt/keyrings/postgresql.gpg] http://apt.postgresql.org/pub/repos/apt trixie-pgdg main" > /etc/apt/sources.list.d/pgdg.list

# 2. Установка пакетов хаба, PostgreSQL 18 и межсетевого экрана
apt-get update && apt-get install -y \
    postgresql-18 \
    openssl \
    ca-certificates \
    curl \
    jq \
    nftables
```

Бинарный файл `canterlot-hub` размещается в `/usr/local/bin/canterlot-hub` с правами `0755`.  
> **TODO**: В настоящее время централизованный deb-пакет отсутствует; бинарный файл собирается из исходников через `cargo build --release -p canterlot-hub` и копируется вручную.

### 2.2. Системный пользователь ОС и настройка PostgreSQL

Служба хаба выполняется от выделенной непривилегированной учётной записи ОС. Создаём системного пользователя:
```bash
useradd --system --home-dir /var/lib/canterlot --shell /usr/sbin/nologin canterlot
```

Затем создаём роль и базу данных в PostgreSQL, а строку подключения сохраняем в файл с правами `0600`. Пароль генерируется случайным (шестнадцатеричные символы не требуют экранирования в URL), передаётся в `psql` через стандартный ввод и не появляется ни в аргументах команд (`ps`), ни в журнале командного интерпретатора (`~/.bash_history`):
```bash
mkdir -p /etc/canterlot
( umask 077
  DB_PASS=$(openssl rand -hex 24)
  sudo -u postgres psql -c "CREATE ROLE canterlot LOGIN;"
  printf "ALTER ROLE canterlot PASSWORD '%s';\n" "$DB_PASS" | sudo -u postgres psql -q
  sudo -u postgres psql -c "CREATE DATABASE canterlot OWNER canterlot ENCODING 'UTF8' TEMPLATE template0;"
  printf 'postgres://canterlot:%s@127.0.0.1:5432/canterlot\n' "$DB_PASS" > /etc/canterlot/db.url
  chown canterlot:canterlot /etc/canterlot/db.url
)
```

> [!IMPORTANT]
> База должна быть в кодировке UTF-8: `sudo -u postgres psql -d canterlot -Atc "SHOW server_encoding;"` печатает `UTF8`. Миграции используют ICU-сортировку `und-x-icu` (уникальный индекс имён групп, поиск сессий по названию), а в базе `SQL_ASCII` накат останавливается с ошибкой `collation "und-x-icu" for encoding "SQL_ASCII" does not exist`. Кластер, созданный пакетом PostgreSQL в минимальном контейнере без заданной локали, может получить кодировку `SQL_ASCII`, и `CREATE DATABASE` без `ENCODING` унаследует её из `template1`, поэтому кодировка и `template0` в команде выше заданы явно. Кодировку существующей базы не сменить: удалите пустую базу и создайте её заново с `ENCODING 'UTF8'`.

### 2.3. Межсетевой экран nftables на LXC хаба

Порт HTTP API (:8080) предназначен исключительно для обратного прокси Caddy, а порт gRPC (:7443) — только для рабочей машины воркера ([docs/design/pilot.md#2-размещение](../design/pilot.md#2-размещение) §2). На уровне ядра LXC сетевой трафик ограничивается через `nftables`.

Создаём конфигурацию `/etc/nftables.conf`:
```bash
cat <<'EOF' > /etc/nftables.conf
#!/usr/sbin/nft -f

flush ruleset

table inet filter {
    chain input {
        type filter hook input priority filter; policy accept;

        # Разрешаем локальный loopback и уже установленные сессии
        iif "lo" accept
        ct state established,related accept

        # HTTP API (порт 8080) принимается ТОЛЬКО с адреса хоста Caddy (10.0.0.2)
        ip saddr 10.0.0.2 tcp dport 8080 accept
        tcp dport 8080 drop

        # gRPC TLS (порт 7443) принимается ТОЛЬКО с адреса машины воркера (10.0.0.20)
        ip saddr 10.0.0.20 tcp dport 7443 accept
        tcp dport 7443 drop
    }
}
EOF

systemctl enable --now nftables
```

**Проверка изоляции портов**: выполните с машины воркера (`10.0.0.20`) или любого другого узла, отличного от Caddy:
```bash
# Порт 8080 обязан сбрасываться правилом drop
curl --connect-timeout 2 http://10.0.0.10:8080/readyz
# Ожидается: Operation timed out (пакеты отброшены)
```

### 2.4. Конфигурация `hub.toml`

Конфигурационный файл хаба располагается по пути `/etc/canterlot/hub.toml` (права `0600`, владелец `canterlot`). Все относительные пути внутри файла вычисляются относительно каталога, в котором лежит `hub.toml`.

Ниже приведён эталонный файл конфигурации со всеми таблицами, которые принимает структура `HubConfig` ([crates/canterlot-hub/src/config.rs](../../crates/canterlot-hub/src/config.rs)). Ссылок на строки кода здесь нет: они устаревают с каждой правкой, значения по умолчанию сверяйте с файлом:

```toml
# Корневые параметры хаба
database_url_file = "/etc/canterlot/db.url"
grpc_listen = "10.0.0.10:7443"                      # обязательное поле, без значения по умолчанию
tls_cert = "/etc/canterlot/tls/hub.crt"
tls_key = "/etc/canterlot/tls/hub.key"
manifest = "/etc/canterlot/manifest/installation.toml"

[timers]
handshake = "10s"
# degraded_after (15s), unavailable_after (30s)
# и command_stored_receipt (5s) опущены — действуют значения по умолчанию из кода
http2_keepalive_interval = "30s"
http2_keepalive_timeout = "10s"
dispatch_sweep = "5s"

[http]
listen = "10.0.0.10:8080"                           # привязка к IP хаба в LXC; default в коде: 127.0.0.1:8080
trusted_proxies = ["10.0.0.2/32"]                   # IP отдельного хоста Caddy /32
owner_login = "owner"                               # логин владельца для входа по паролю
# user_header и logout_url устарели: хаб их принимает и не использует (предупреждение
# config_key_deprecated при старте); дошедший от прокси Remote-User только помечается
# заголовком ответа X-Canterlot-Proxy-Leak. Из нового файла их лучше убрать.
allowed_origins = ["https://chat.example.com"]
# max_json_body_bytes опущен — действует значение по умолчанию 524288 (512 KiB)
header_read_timeout = "10s"
body_read_timeout = "30s"
max_connections = 256
proxy_secret_file = "/etc/canterlot/proxy.secret"    # без секрета адрес клиента (X-Canterlot-Client-Ip) не принимается
multi_process = false                               # true включает LISTEN/NOTIFY

[http.ws]
max_connection_age = "30m"                          # принудительный реконнект для повторной аутентификации
max_subscriptions = 16
queue_messages = 64                                 # уменьшено против умолчания 256; диапазон 2..=256
queue_bytes = 65536                                 # уменьшено против умолчания 1048576; диапазон 4096..=1048576
delta_window = "25ms"                               # не более 25ms
poll_interval = "2s"
stall_timeout = "15s"

[auth]
# origin = "https://chat.example.com"               # по умолчанию первый из allowed_origins: ссылки восстановления, WebAuthn
# rp_id = "chat.example.com"                        # по умолчанию хост origin

[auth.geoip]                                        # с релиза, где есть canterlot-hub geoip update
path = "/var/lib/canterlot/geoip/dbip-city-lite.mmdb" # база мест входа; выкат узнаёт путь у хаба (canterlot-hub geoip path)

[limits]
weekly_reserve_pct = 95                             # умолчание резерва как расход: 100 - значение = остаток, который владелец оставляет себе; действующее значение меняется в настройках PWA
snapshot_min_interval = "10m"
history_days = 90
default_resume_text = "Лимиты восстановлены, продолжай с того места, где остановился."
reset_grace = "20s"
verify_backoff = ["30s", "1m", "2m", "5m"]
poll_min_interval = "5m"
unknown_reset_retry = "30m"
reserve_exit_hysteresis_permille = 20
warn_pcts = [80, 90, 95]                            # устарело: принимается для старых файлов, больше не используется
stale_after = "35m"
unverified_wake_after = "20m"

[limits.poll_floor]
claude = "30m"
codex = "5m"

[activity]
stalled_generating = "30m"
stalled_tool = "30m"
tick = "15s"
debounce = "250ms"
idle_quiet = "5s"                                   # сколько сессия стоит в idle без событий, чтобы считаться устоявшейся (межсессионные итоги)

[scheduler]
max_pending_per_conversation = 50
retry_queue_full = "60s"
max_attempts = 20
idle_poll = "1s"

[attachments]
blob_dir = "/var/lib/canterlot/attachments"
max_concurrent_uploads = 4
max_media_bytes_per_machine = 0                 # общий объём медиа одной машины, байт; 0 выключен, включайте после оценки накопленного
gc_grace = "24h"
gc_interval = "1h"

[machines]
disk_warn_free_gb = 30
disk_critical_free_gb = 10
disk_warn_pct = 85                                 # устарело
metrics_stale_after = "90s"

[session_tools]                                     # пределы межсессионных инструментов, таблица необязательна
max_spawn_depth = 3
max_chain_depth = 8
pair_sends = 10
pair_window = "10m"
sender_sends = 30
sender_window = "10m"
parent_spawns = 10
spawn_window = "1h"
max_working_children = 6
spawn_stagger = "20s"
```

### 2.5. Секрет прокси и перенос на сервер Caddy

Секрет прокси создаётся отдельной командой (32 случайных байта в виде 64 шестнадцатеричных символов, без перевода строки):
```bash
( umask 077
  openssl rand -hex 32 | tr -d '\n' > /etc/canterlot/proxy.secret
  chmod 0600 /etc/canterlot/proxy.secret
  chown canterlot:canterlot /etc/canterlot/proxy.secret
)
```

**Перенос секрета на сервер Caddy (10.0.0.2)**:  
Секрет обязан быть перенесён на сервер Caddy без отображения его содержимого в терминале или сохранении в истории командной строки:
```bash
ssh root@10.0.0.2 "( umask 077; mkdir -p /etc/caddy/secrets; cat > /etc/caddy/secrets/canterlot-proxy && chown caddy:caddy /etc/caddy/secrets/canterlot-proxy && chmod 0600 /etc/caddy/secrets/canterlot-proxy )" < /etc/canterlot/proxy.secret
```
Файл `/etc/caddy/secrets/canterlot-proxy` на сервере Caddy содержит байт-в-байт тот же секрет, защищён правами `0600` и доступен исключительно пользователю `caddy`.

Если Caddy запущен в контейнере Docker, файл остаётся на хосте с правами `0600`, владельцем назначается UID, от которого работает процесс Caddy в контейнере, а в контейнер файл монтируется только для чтения по пути, указанному в директиве `{file.…}` Caddyfile (docs/http.md использует `/run/secrets/canterlot-proxy`). Путь в Caddyfile и путь монтирования должны совпадать.

### 2.6. Инициализация хаба (`canterlot-hub init`)

Команда инициализации ([crates/canterlot-hub/src/main.rs:30](../../crates/canterlot-hub/src/main.rs#L30)) выполняется **ровно один раз**:
```bash
mkdir -p /etc/canterlot/manifest /var/lib/canterlot/attachments
chown -R canterlot:canterlot /etc/canterlot /var/lib/canterlot

sudo -u canterlot canterlot-hub init --config /etc/canterlot/hub.toml
```

Команда:
1. Применяет миграции базы данных sqlx.
2. Вставляет строку владельца в `owners` со свежим UUID (`installation.rs:62-65`); логин `owner_login` задаётся только в HTTP-конфиге и служит логином входа по паролю. Пароля после `init` нет: первый вход — ссылка восстановления (§4.4).
3. Генерирует начальные UUID для `installation_id` и `restore_generation`.
4. Записывает внешний манифест `/etc/canterlot/manifest/installation.toml`.
5. Повторный запуск завершается ошибкой, если манифест или таблица инсталляции уже существуют.

### 2.7. Служба systemd для хаба

Создаётся системный юнит `/etc/systemd/system/canterlot-hub.service`:
```ini
[Unit]
Description=Canterlot Control Plane Hub
After=network.target postgresql.service
Wants=postgresql.service

[Service]
Type=simple
User=canterlot
Group=canterlot
WorkingDirectory=/etc/canterlot
ExecStart=/usr/local/bin/canterlot-hub serve --config /etc/canterlot/hub.toml
Restart=always
RestartSec=5s
LimitNOFILE=65536

# Песочница systemd
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/canterlot /etc/canterlot/manifest
ReadOnlyPaths=/etc/canterlot

[Install]
WantedBy=multi-user.target
```

Активация и запуск:
```bash
systemctl daemon-reload
systemctl enable --now canterlot-hub
```

При старте `canterlot-hub serve` ([crates/canterlot-hub/src/main.rs:35](../../crates/canterlot-hub/src/main.rs#L35)) проверяет манифест, сверяет его с БД, применяет недостающие миграции под advisory lock, устанавливает panic hook и запускает gRPC и HTTP серверы.

---

## 3. Внутренний УЦ и TLS для gRPC

Канал связи между воркером и хабом защищён **серверным TLS** от внутреннего центра сертификации (УЦ) ([docs/design/pilot.md#2-размещение](../design/pilot.md#2-размещение) §2). Воркер проверяет сертификат хаба по доверенному корневому сертификату `ca.crt` и сверяет Subject Alternative Name (SAN). Для аутентификации воркер использует долгоживущий bearer-токен машины в gRPC-метаданных ([crates/canterlot-worker/src/session.rs:638-645](../../crates/canterlot-worker/src/session.rs#L638-L645)). Клиентские сертификаты в схеме пилота не применяются (это серверный TLS, а не mutual TLS).

> **TODO**: Автоматическая ротация сертификатов внутренним демоном (например, cert-manager / step-ca) не входит в состав пилота. Сертификаты генерируются через OpenSSL вручную на длительный срок.

### 3.1. Генерация корневого УЦ

На доверенном узле администрирования:
```bash
# 1. Приватный ключ УЦ (максимально защищённый)
openssl genrsa -out ca.key 4096
chmod 0600 ca.key

# 2. Самоподписанный сертификат УЦ на 10 лет
openssl req -x509 -new -nodes -key ca.key -sha256 -days 3650 \
    -out ca.crt -subj "/CN=Canterlot Pilot Internal CA"
```

### 3.2. Выпуск сертификата хаба с SAN

Сертификат хаба обязан содержать как DNS-имя, так и внутренний IP-адрес LXC:
```bash
# 1. Ключ сервера хаба
openssl genrsa -out hub.key 2048
chmod 0600 hub.key

# 2. Запрос на сертификат (CSR)
openssl req -new -key hub.key -out hub.csr -subj "/CN=hub.example.internal"

# 3. Файл расширений с SAN
cat <<'EOF' > hub.ext
authorityKeyIdentifier=keyid,issuer
basicConstraints=CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = @alt_names

[alt_names]
DNS.1 = hub.example.internal
IP.1 = 10.0.0.10
EOF

# 4. Подпись сертификата корневым УЦ
openssl x509 -req -in hub.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
    -out hub.crt -days 825 -sha256 -extfile hub.ext
```

### 3.3. Размещение сертификатов

1. **На хабе**:  
   Копируем `hub.crt` в `/etc/canterlot/tls/hub.crt`, а `hub.key` в `/etc/canterlot/tls/hub.key` (права `0600`, владелец `canterlot`). Пути указываются в `tls_cert` и `tls_key` файла `hub.toml`.
2. **На рабочей машине воркера**:  
   Копируем `ca.crt` в `~/.config/canterlot/ca.pem`. Путь к этому файлу указывается в поле `ca_cert` конфигурации воркера.

---

## 4. Блок сайта Caddy

Caddy выполняет функции внешнего TLS-терминатора, сервера статических файлов PWA и обратного прокси к хабу. Входа на прокси нет: статика PWA публична (без неё не показать страницу входа, данных в сборке нет), всё под `/api` хаб пускает только с сеансом ([ADR-0005](../adr/0005-owner-sign-in.md), [docs/http.md](../http.md#caddy)).

### 4.1. Сборка фронтенда PWA и генерация CSP

Перед запуском PWA собирается в статические файлы:
```bash
cd web
npm ci
npm run build
```

Скрипт `web/scripts/print-csp.mjs` вычисляет SHA-256 хеши инлайн-скриптов из `web/build/200.html` и генерирует заголовок Content-Security-Policy ([docs/pwa.md](../pwa.md#статическая-публикация)):
```bash
export CANTERLOT_CSP="$(node scripts/print-csp.mjs https://chat.example.com)"
```
Переменная `CANTERLOT_CSP` передаётся в окружение службы Caddy и подставляется директивой `header Content-Security-Policy "{$CANTERLOT_CSP}"`.

### 4.2. Конфигурация Caddyfile

Конфигурация сайта строго воспроизводит канонический блок из [docs/pwa.md](../pwa.md#статическая-публикация):

> [!IMPORTANT]
> Внутри `route` директивы выполняются в порядке записи: присланные клиентом заголовки вычищаются до проксирования, а `header_up` добавляет к запросу к хабу свои значения.
>
> Директивы `request_header -Remote-*`, `request_header -X-Forwarded-*`, `request_header -X-Canterlot-Proxy` и `request_header -X-Canterlot-Client-Ip` зачищают **семейства префиксов**: заголовки пользователя старого прокси (`Remote-User`, `Remote-Groups`, `Remote-Name`, `Remote-Email`), параметры прокси (`X-Forwarded-For`, `X-Forwarded-Proto`, `X-Forwarded-Host`, `X-Forwarded-Uri`), секрет прокси и адрес клиента. Чистка `Remote-*` остаётся навсегда, хотя новый хаб `Remote-User` не читает: хаб 1.29 и старше верит ему от прокси с секретом, и откат без неё дал бы вход любому, кто пришлёт логин владельца (правило отката в §11.4).

```caddyfile
chat.example.com {
    route {
        # 1. Зачистка заголовков клиента (навсегда)
        request_header -Remote-*
        request_header -X-Forwarded-*
        request_header -X-Canterlot-Proxy
        request_header -X-Canterlot-Client-Ip

        # 2. Политика безопасности контента (значение из переменной CANTERLOT_CSP)
        header Content-Security-Policy "{$CANTERLOT_CSP}"

        # 3. Проксирование API, WebSocket и healthcheck на хаб; сеанс проверяет хаб
        @api path /api/* /healthz
        handle @api {
            # Сжатие ответов хаба; WebSocket-апгрейды Caddy не сжимает.
            encode zstd gzip
            reverse_proxy 10.0.0.10:8080 {
                header_up X-Canterlot-Proxy {file./etc/caddy/secrets/canterlot-proxy}
                header_up X-Canterlot-Client-Ip {remote_host}
            }
        }

        # 4. Переходный ответ для старых установок с Web Share Target
        @share {
            method POST
            path /_share
        }
        handle @share {
            redir * /share?error=sw_inactive 303
        }

        # 5. Раздача статики PWA без входа: manifest без кэширования
        handle {
            @fresh path /manifest.webmanifest
            header @fresh Cache-Control "no-cache"
            root * /var/www/canterlot/build
            try_files {path} /200.html
            file_server {
                precompressed zstd br gzip
            }
        }
    }
}
```

`{remote_host}` — адрес непосредственного собеседника Caddy (TCP-пир), его клиент подделать не может; так же написано в [docs/http.md](../http.md#caddy) и [docs/pwa.md](../pwa.md). `{client_ip}` здесь не годится: если в глобальных опциях Caddy есть `servers { trusted_proxies … }` (в docker-установках часто `static private_ranges`), Caddy вычисляет `{client_ip}` из `X-Forwarded-For` ещё до `request_header -X-Forwarded-*`, и клиент из доверенной сети (LAN, hairpin NAT, userland-proxy docker) выбирает себе любой адрес: корзину перебора, IP в списке сеансов и пуш о новой стране. Хаб этого не видит — заголовок приходит от Caddy вместе с секретом. `{client_ip}` уместен только когда перед Caddy стоит свой доверенный CDN или прокси и глобальный `trusted_proxies` перечисляет ровно его адреса. Без `header_up X-Canterlot-Client-Ip` (или без секрета прокси) хаб не знает адреса клиента: все попытки пароля попадают в одну корзину перебора, а список сеансов показывает «IP неизвестен».

`file_server { precompressed zstd br gzip }` отдаёт заранее сжатые соседние файлы `<name>.br`, `<name>.zst` и `<name>.gz`, которые создаёт `npm run build`; каталог статики переносится на сервер целиком, вместе с ними. `encode zstd gzip` внутри `@api` сжимает ответы хаба, но не трогает WebSocket-апгрейды.

В `redir` матчер `*` обязателен: единственный аргумент, начинающийся с `/`, Caddy читает как матчер пути, а не как адрес перехода (`redir /share?error=sw_inactive 303` даёт `302` с заголовком `Location: 303`).

### 4.3. Приёмка прокси скриптом `check-proxy.sh`

После каждого изменения Caddy обязателен запуск [scripts/check-proxy.sh](../../scripts/check-proxy.sh). Cookie владельца на диске ему не нужна:
```bash
EXPECTED_LOGIN=owner bash scripts/check-proxy.sh https://chat.example.com              # без сеанса
EXPECTED_LOGIN=owner bash scripts/check-proxy.sh https://chat.example.com --positive   # плюс вход и выход
```
Без флага скрипт проверяет:
- `GET /api/v1/me` без сеанса отвечает JSON `401 unauthenticated` самого хаба; `401` с `WWW-Authenticate` (вход Caddy), переход `3xx` или HTML — провал;
- поддельный `Remote-User` с логином владельца (отдельно и вместе с `X-Canterlot-Proxy`, `X-Canterlot-Client-Ip`, `X-Forwarded-For`) по-прежнему даёт `401`, и ни один ответ не несёт `X-Canterlot-Proxy-Leak`: так проверяется, что Caddy вычищает `Remote-*` (без этого откат хаба на 1.29 стал бы обходом входа);
- `GET /` отдаёт `200` и HTML (оболочка PWA публична).

`--positive` добавляет вход: по ssh `HUB_SSH` (умолчания хоста и путей — из `scripts/deploy/deploy.conf`, как у выката) на хосте хаба выпускается ссылка восстановления на 5 минут, скрипт входит по ней во временный jar в собственном каталоге `mktemp`, проверяет `/api/v1/me` (`owner_id`, `login`, `session_id`) и по `GET /api/v1/auth/sessions`, что у текущего сеанса есть адрес клиента (пустой адрес — нет `header_up X-Canterlot-Client-Ip` или не совпал секрет прокси), выходит (`POST /api/v1/auth/logout`) и удаляет jar с каталогом. Ссылка остаётся только в переменной скрипта и нигде не печатается. Вход создаёт сеанс, поэтому владельцу приходит push о входе; новая ссылка отменяет прежнюю неиспользованную, так что `--positive` запускается, когда владелец уже вошёл.

### 4.4. Вход владельца

Вход проверяет сам хаб ([docs/http.md](../http.md#вход-и-сеансы), [ADR-0005](../adr/0005-owner-sign-in.md)): страница входа PWA, пароль `owner_login` или ключ доступа, сеанс в cookie `__Host-canterlot_session`. Первый вход и вход без своих устройств — ссылка восстановления на хосте хаба:
```bash
sudo -u canterlot canterlot-hub auth recovery-link --config /etc/canterlot/hub.toml --push
```
С `--push` ссылка уходит только в Gotify владельца, а команда печатает `sent`. Без `--push` ссылка печатается в терминал: так её запускает только сам владелец в своём ssh. Агент или делегат ссылку не печатает и не пересылает: она не должна попасть ни в один транскрипт и журнал. Ссылка живёт 10 минут (`--minutes`, до 60), открывается один раз, новая отменяет прежнюю; после входа сразу задайте пароль или ключ доступа. Остальные команды: `auth sessions` (живые сеансы), `auth revoke-all` (завершить все), `auth set-password`.

Место входа в списке сеансов берётся из базы DB-IP City Lite (`[auth.geoip].path`). Её скачивает `canterlot-hub geoip update --config /etc/canterlot/hub.toml`; выкат (§11.4) запускает команду сам, когда файла нет или он старше 35 дней. Без базы место просто пустое.

---

## 5. Сопряжение машины (Pairing)

Сопряжение воркера на рабочей VM с центральным хабом выполняется через CLI-команду `canterlot-hub machine add` ([crates/canterlot-hub/src/main.rs:42](../../crates/canterlot-hub/src/main.rs#L42)).

### 5.1. Добавление машины

Выполните на хабе:
```bash
sudo -u canterlot canterlot-hub machine add \
    --config /etc/canterlot/hub.toml \
    --name workstation
```

Команда возвращает в stdout ровно четыре строки:
```text
machine_id=019a1b10-0000-7000-8000-000000000001
installation_id=019a1b10-0000-7000-8000-000000000002
restore_generation=019a1b10-0000-7000-8000-000000000003
token=dGhpcy1pcy1hLXRlc3QtdG9rZW4...
```

### 5.2. Перенос параметров на воркер

1. Значения `machine_id`, `installation_id` и `restore_generation` вписываются в файл `worker.toml`.
2. Значение `token` сохраняется в отдельный защищённый файл `token_file` на машине воркера:
   ```bash
   mkdir -p ~/.config/canterlot
   echo -n "dGhpcy1pcy1hLXRlc3QtdG9rZW4..." > ~/.config/canterlot/token
   chmod 0600 ~/.config/canterlot/token
   ```
   > [!IMPORTANT]
   > Токен выводится только один раз. Хаб сохраняет в PostgreSQL исключительно SHA-256 хеш токена. Прочитать токен повторно невозможно.

### 5.3. Ротация и отзыв токена машины

- **Ротация токена** ([crates/canterlot-hub/src/main.rs:48](../../crates/canterlot-hub/src/main.rs#L48)):
  ```bash
  canterlot-hub machine rotate-token \
      --config /etc/canterlot/hub.toml \
      --machine-id 019a1b10-0000-7000-8000-000000000001
  ```
  Выводит новый `token=...`. Старый сеанс gRPC разрывается. Новый токен записывается в `token_file`, после чего воркер перезапускается.  
  Если локальное состояние на воркере было намеренно удалено, используется флаг `--reset-local-state`. Для разблокировки отозванной машины используется флаг `--reactivate`.

- **Отзыв машины** ([crates/canterlot-hub/src/main.rs:58](../../crates/canterlot-hub/src/main.rs#L58)):
  ```bash
  canterlot-hub machine revoke \
      --config /etc/canterlot/hub.toml \
      --machine-id 019a1b10-0000-7000-8000-000000000001
  ```
  Немедленно закрывает соединение воркера с ошибкой `Unauthenticated` и блокирует приём любых команд и фактов.

---

## 6. Воркер на рабочей VM владельца

Воркер исполняется непосредственно на рабочей машине/виртуальной машине владельца от имени обычной пользовательской учётной записи (не root!).

### 6.1. Разрешение сетевого имени хаба

Чтобы воркер успешно подключался к хабу по защищённому имени из сертификата SAN ([crates/canterlot-worker/src/session.rs:643-645](../../crates/canterlot-worker/src/session.rs#L643-L645)), на рабочей машине настраивается разрешение DNS или добавляется статическая запись в `/etc/hosts`:
```bash
echo "10.0.0.10 hub.example.internal" | sudo tee -a /etc/hosts
```
Проверка разрешения имени:
```bash
getent hosts hub.example.internal
# Ожидается вывод: 10.0.0.10 hub.example.internal
```

### 6.2. Конфигурация `worker.toml`

Конфигурационный файл располагается по пути `~/.config/canterlot/worker.toml`.

Полный набор полей, которые принимает структура `WorkerConfig` ([crates/canterlot-worker/src/config.rs](../../crates/canterlot-worker/src/config.rs)), кроме таблиц адаптеров (о них ниже):

```toml
# Корневые параметры соединения и инсталляции
hub_url = "https://hub.example.internal:7443"
ca_cert = "/home/user/.config/canterlot/ca.pem"
token_file = "/home/user/.config/canterlot/token"    # права 0600
machine_id = "019a1b10-0000-7000-8000-000000000001"
installation_id = "019a1b10-0000-7000-8000-000000000002"
restore_generation = "019a1b10-0000-7000-8000-000000000003"
data_dir = "/home/user/.local/state/canterlot-worker" # права 0700

[timers]
handshake = "10s"
http2_keepalive_interval = "30s"
http2_keepalive_timeout = "10s"
heartbeat = "5s"
heartbeat_ack = "5s"
range_ack = "10s"
backoff_min = "250ms"
backoff_max = "30s"
backoff_reset_after = "30s"

[execution]
# Корни рабочих каталогов; ["/"] разрешает любой абсолютный путь, как решил владелец.
allowed_workdir_roots = ["/"]
supervisor_exe = "/usr/local/bin/canterlot-worker"  # current_exe по умолчанию
spawn_timeout = "10s"
build_jobs = 6                                      # CARGO_BUILD_JOBS для сессий, если он ещё не задан
# Серверы MCP владельца, которые получают делегаты (D24); только на чтение, nanokvm никогда.
delegate_mcp_servers = ["context7"]

[execution.supervisor]
attach_wait = "30s"
ready_deadline = "30s"
transmit_deadline = "10s"
cli_version_probe = "5s"
stop_grace = "5s"
stop_retry = "2s"

[machine]
metrics_interval = "30s"
# codexbar_path = "/home/user/.local/bin/codexbar"  # без значения: PATH, затем ~/.local/bin
limits_poll_interval = "15m"                        # меньше 15 минут поднимается до 15

[attachments]
fetch_parallelism = 2                               # 1 или 2
fetch_retry = "2s"
fetch_attempts = 5
fetch_backoff_cap = "30s"
```

Таблицы адаптеров `[claude_code]`, `[codex]` (в том числе `[codex].home_dir`) и необязательная `[opencode]` (без неё OpenCode не показывается и не запускается; вход в отдельную базу canterlot описан в docs/configuration.md) показаны в примере `worker.toml` в [docs/configuration.md](../configuration.md). Раздел `[[instruction_profiles]]` из конфигурации воркера удалён: с ним воркер не запустится, потому что пресеты инструкций хранит хаб и редактируются они в PWA ([docs/design/pilot.md#4-создание-сессии-и-пресеты-инструкций](../design/pilot.md#4-создание-сессии-и-пресеты-инструкций)).

### 6.3. Права доступа к файлам и каталогам

- `token_file`: обязательно права `0600` (`chmod 600 ~/.config/canterlot/token`).
- `data_dir`: создаётся воркером автоматически с правами `0700`.
- Все каталоги исполнений `data_dir/executions/<id>` имеют права `0700`, а сокеты IPC и `spec.toml` — `0600`; файл `instructions.md` с текстом пресета внутри каталога исполнения — `0600`.
- `CODEX_HOME` canterlot (`[codex].home_dir`, по умолчанию `data_dir/codex-home`) нужен пресетам, кроме «Как у владельца»: воркер сам создаёт его с правами `0700`, наполняет символическими ссылками на записи `~/.codex` и собственным `AGENTS.md`, а путь внутри `/tmp`, `/var/tmp` или домашнего каталога Codex владельца отвергает.

### 6.4. Пользовательский юнит systemd и выживание процессов

Воркер настраивается как пользовательская служба systemd (`systemd --user`).  
Файл: `~/.config/systemd/user/canterlot-worker.service`:

```ini
[Unit]
Description=Canterlot Execution Worker
After=default.target

[Service]
Type=simple
WorkingDirectory=%h
ExecStart=/usr/local/bin/canterlot-worker run --config %h/.config/canterlot/worker.toml
Restart=always
RestartSec=3s

# КРИТИЧЕСКИ ВАЖНО ДЛЯ ВЫЖИВАНИЯ ИСПОЛНЕНИЙ:
KillMode=process

# Защита от переполнения дескрипторов
LimitNOFILE=65536

[Install]
WantedBy=default.target
```

#### Механизм `KillMode=process` и выживание процессов сопровождения

По умолчанию systemd при остановке или перезапуске службы уничтожает всю группу cgroup (`KillMode=control-group`), посылая SIGKILL всем потомкам процесса.  
Директива `KillMode=process` предписывает systemd посылать сигнал SIGTERM **исключительно** основному процессу воркера (`Worker`).

1. При запуске каждого агентского исполнения воркер стартует изолированный процесс сопровождения `canterlot-worker supervise` ([crates/canterlot-worker/src/supervisor/run.rs:218-223](../../crates/canterlot-worker/src/supervisor/run.rs#L218-L223)), который сразу создаёт новую сессию операционной системы через `setsid`.
2. Внутри новой сессии процесс сопровождения запускает дочерний CLI харнесса (`claude`, `codex` или `opencode serve`).
3. При перезапуске воркера (`systemctl --user restart canterlot-worker`):
   - Главный процесс воркера корректно завершается.
   - Процессы сопровождения и CLI продолжают выполняться без сбоев.
   - Весь вывод CLI и промежуточные события буферизуются процессом сопровождения в локальный сессионный журнал на диске: `data_dir/executions/<execution_id>/sj/` (`SJ`, [crates/canterlot-worker/src/supervisor/sj.rs:33-38](../../crates/canterlot-worker/src/supervisor/sj.rs#L33-L38)).
4. При старте нового экземпляра воркер сканирует каталог `data_dir/executions/`, находит живые процессы сопровождения, подключается к их IPC Unix-сокетам (`attach`), сверяет состояние и передаёт накопленные факты в хаб. Агентское исполнение не прерывается.

### 6.5. Включение linger для пользователя

Чтобы пользовательская служба systemd не завершалась при выходе владельца из SSH-сессии, на машине воркера включается linger:
```bash
loginctl enable-linger $USER
```

Активация и запуск:
```bash
systemctl --user daemon-reload
systemctl --user enable --now canterlot-worker
```

---

## 7. Порядок обновлений

Обновление компонентов Canterlot выполняется без прерывания текущих агентских сессий с соблюдением строгого порядка.

### 7.1. Сборка артефактов

```bash
# 1. Сборка бинарников Rust
source ~/.cargo/env
cargo build --release -p canterlot-hub -p canterlot-worker

# 2. Сборка PWA
cd web && npm ci && npm run build && cd ..
```

### 7.2. Очерёдность перезапуска

1. **Хаб (LXC)**:
   ```bash
   install -m 0755 target/release/canterlot-hub /usr/local/bin/canterlot-hub.new
   mv -f /usr/local/bin/canterlot-hub.new /usr/local/bin/canterlot-hub
   systemctl restart canterlot-hub
   ```
   Бинарник кладётся рядом под новым именем и подменяется атомарным `rename` (`mv` в пределах одной файловой системы): работающий процесс держит старый inode, а путь сразу указывает на новый файл. `cp` поверх запущенного бинарника падает с `Text file busy`, а `install` прямо в целевой путь на мгновение оставляет его пустым. Раскладка по каталогам `releases/<sha>` со ссылкой `current` устроена так же: новый релиз кладётся рядом, ссылка меняется атомарным `rename`.
   *Хаб перезапускается первым*. На старте команда `serve` автоматически выполняет накат новых миграций базы данных PostgreSQL под advisory lock `sqlx`.
2. **Caddy (Web Server)**:
   При изменениях в PWA:
   - Обновите файлы сборки: `/var/www/canterlot/build/`.
   - Запустите скрипт генерации CSP: `export CANTERLOT_CSP="$(node web/scripts/print-csp.mjs https://chat.example.com)"`.
   - Перезагрузите сервис Caddy с обновлённой переменной окружения: `systemctl restart caddy` (или `caddy reload`).
3. **Воркер (Рабочая VM)**:
   ```bash
   install -m 0755 target/release/canterlot-worker /usr/local/bin/canterlot-worker.new
   mv -f /usr/local/bin/canterlot-worker.new /usr/local/bin/canterlot-worker
   systemctl --user restart canterlot-worker
   ```
   Воркер перезапускается последним. Процессы сопровождения запущены из того же файла (`supervisor_exe`) и переживают перезапуск, поэтому `cp` здесь падает при любой живой сессии. После подмены они дорабатывают на старом бинарнике, новые исполнения стартуют на новом.

### 7.3. Что выживает при обновлении

- **Процессы сопровождения и агентские CLI**: благодаря `KillMode=process`, вызову `setsid` и сохранению сессионных журналов `SJ` работа агентов продолжается без пауз.
- **Очереди сообщений и отложенные вводы**: хранятся в PostgreSQL хаба, планировщик подхватывает их сразу после старта хаба.
- **Вложения**: незавершённые загрузки файлов возобновляются с сохранённого байтового смещения (`Upload-Offset`).
- **Согласование протокола (`pilot_v1`)**: при реконнекте воркер и хаб заново согласуют возможности в handshake. Если хаб старше воркера, воркер придерживает новые факты; откат хаба на версию ниже уже задействованных возможностей не поддерживается ([docs/development.md](../development.md#обновление-контракта-пилота)).

---

## 8. Резервное копирование и восстановление

В соответствии с решением владельца ([pilot.md](../design/pilot.md#11-отступления-от-тз-решения-владельца) §1.1, §2) приложение самостоятельно не создаёт резервные копии:
- Бэкапы выполняются **исключительно средствами гипервизора Proxmox VE** (регулярные согласованные снапшоты и дампы LXC контейнера хаба и VM воркера).

### 8.1. Правило изоляции внешнего манифеста

> [!CAUTION]
> Файл внешнего манифеста инсталляции `manifest` (`/etc/canterlot/manifest/installation.toml`) **КАТЕГОРИЧЕСКИ ЗАПРЕЩЕНО** помещать внутрь набора снапшотов базы данных PostgreSQL!
> Он должен находиться на отдельной точке монтирования (bind-mount хоста или независимый storage volume), которая **НЕ** откатывается при восстановлении базы из снапшота.

Манифест хранит:
- `installation_id`: неизменный UUID инсталляции.
- `restore_generation`: текущий маркер поколения восстановления.
- Флаг незавершённой процедуры восстановления `restore_in_progress`.

### 8.2. Рассинхронизация состояния (State Divergence) в пилоте

При откате базы данных хаба из снапшота Proxmox без одновременного отката воркера возникает ситуация рассинхронизации: база данных хаба откатывается назад во времени, а локальные журналы воркера (`CJ` и `SJ`) содержат факты и события, которых хаб больше не помнит.

**Поведение пилота**:
1. При последующем подключении воркера к хабу расхождение курсоров фиксируется как неустранимая ошибка рассинхронизации: воркер возвращает ошибку `WorkerError::StateDivergence` ([crates/canterlot-worker/src/session.rs](../../crates/canterlot-worker/src/session.rs)).
2. Процесс воркера немедленно завершает работу с кодом выхода 1 ([crates/canterlot-worker/src/main.rs](../../crates/canterlot-worker/src/main.rs)). С `Restart=always` systemd перезапускает воркер каждые 3 секунды, и каждый запуск снова завершается той же ошибкой: в `journalctl --user -u canterlot-worker` это повторяющаяся строка `hub and worker journal cursors diverged`. До восстановления остановите службу (`systemctl --user stop canterlot-worker`).

**Штатные пути восстановления в пилоте**:
- **Синхронный откат**: одновременное восстановление LXC хаба и VM воркера из парных снапшотов Proxmox VE, снятых в один и тот же момент времени при остановленных сервисах.
- **Сброс локального состояния воркера**: администратор хаба выполняет ротацию токена со сбросом локального состояния через CLI ([crates/canterlot-hub/src/main.rs](../../crates/canterlot-hub/src/main.rs)):
  ```bash
  sudo -u canterlot canterlot-hub machine rotate-token \
      --config /etc/canterlot/hub.toml \
      --machine-id 019a1b10-0000-7000-8000-000000000001 \
      --reset-local-state
  ```
  После этого на машине воркера очищается каталог `~/.local/state/canterlot-worker`, записывается новый токен и служба запускается с чистого состояния.

> [!NOTE]
> Автоматическая процедура управляемого восстановления с формированием нового `restore_generation`, передачей воркером отчёта о живых исполнениях и интерактивным подтверждением владельцем в UI (протокол R7, [docs/design/recovery.md](../../docs/design/recovery.md)) относится к **Этапу 4** и в пилотном контуре не поддерживается.

---

## 9. Чек-лист «перед первым запуском»

Перед открытием PWA в браузере выполните проверку каждого пункта:

- [ ] **1. Системный пользователь и PostgreSQL 18**: системный пользователь `canterlot` создан, служба `postgresql` активна, база `canterlot` создана, файл `/etc/canterlot/db.url` имеет права `0600` и доступен пользователю `canterlot`.
- [ ] **2. Манифест инсталляции**: каталог `/etc/canterlot/manifest` размещён на независимом томе вне снапшотов БД.
- [ ] **3. Первичная инициализация**: выполнена команда `canterlot-hub init --config /etc/canterlot/hub.toml`, создан файл `installation.toml`.
- [ ] **4. Межсетевой экран LXC**: `nftables` активен, порт 8080 доступен только с `10.0.0.2`, порт 7443 — только с `10.0.0.20`.
- [ ] **5. TLS-сертификаты**: сертификаты `ca.crt`, `hub.crt`, `hub.key` выпущены, SAN содержит имя `hub.example.internal` и IP `10.0.0.10`, файл `hub.key` защищён правами `0600`.
- [ ] **6. Секрет прокси**: файл `/etc/canterlot/proxy.secret` на хабе и `/etc/caddy/secrets/canterlot-proxy` на сервере Caddy содержат идентичный 32-байтный секрет и защищены правами `0600`.
- [ ] **7. Caddy**: блок `chat.example.com` зачищает семейства заголовков `Remote-*`, `X-Forwarded-*`, `X-Canterlot-Proxy` и `X-Canterlot-Client-Ip`, входа на прокси не спрашивает, выставляет `Content-Security-Policy` и проксирует `@api` (`/api/*` и `/healthz`) на `10.0.0.10:8080` с `header_up X-Canterlot-Proxy` и `header_up X-Canterlot-Client-Ip`.
- [ ] **8. Разрешение имени на воркере**: в `/etc/hosts` воркера прописана строка `10.0.0.10 hub.example.internal`, команда `getent hosts hub.example.internal` возвращает IP хаба.
- [ ] **9. Сопряжение машины**: выполнена команда `canterlot-hub machine add`, полученный токен сохранён в `~/.config/canterlot/token` с правами `0600`, UUID вписаны в `worker.toml`.
- [ ] **10. Права хранилища воркера**: каталог `~/.local/state/canterlot-worker` создан с правами `0700`.
- [ ] **11. Службы systemd**:
  - Хаб: `systemctl status canterlot-hub` активен (active/running).
  - Воркер: `systemctl --user status canterlot-worker` активен (active/running), директива `KillMode=process` установлена.
  - Linger: выполнен `loginctl enable-linger $USER`.
- [ ] **12. Первый вход**: `canterlot-hub auth recovery-link --push` (§4.4), владелец вошёл по ссылке и задал пароль или ключ доступа.
- [ ] **13. Тестирование прокси**: `bash scripts/check-proxy.sh https://chat.example.com` и затем с `--positive` (§4.3) завершаются с кодом 0.
- [ ] **14. Сквозная доступность и приёмка**:
  - Публичная проверка сертификата (`/healthz` сеанса не требует):
    ```bash
    curl -fsS https://chat.example.com/healthz
    # Ожидается: {"status":"ok"}
    ```
  - Локальная проверка готовности внутри LXC хаба:
    ```bash
    curl -fsS http://10.0.0.10:8080/readyz
    # Ожидается: {"status":"ok"}
    ```
  - В браузере открывается `https://chat.example.com`, страница входа PWA принимает пароль или ключ доступа владельца.
  - В журнале хаба (`journalctl -u canterlot-hub`) фиксируется подключение воркера с сообщением: `session accepted` (атрибуты `kind = "session_accepted"`, `machine_id = ...`).

---

## 10. Ограничения и функциональность `TODO`

Следующие возможности зафиксированы в архитектуре, но не входят в текущую кодовую базу пилота:

1. **TODO: Пакетный установщик**: сборка deb/rpm пакетов отсутствует. Компоненты компилируются через Cargo и устанавливаются вручную.
2. **TODO: Автоматическая ротация TLS gRPC**: ротация сертификатов внутреннего центра сертификации и хаба выполняется вручную через OpenSSL.
3. **TODO: Автоматическое обновление CSP**: заголовок CSP генерируется утилитой `print-csp.mjs` и передаётся в переменную `CANTERLOT_CSP` при обновлении фронтенда.
4. **TODO: Автоматическое восстановление R7**: при рассинхронизации состояния воркер аварийно останавливается; автоматическая сверка фактов запланирована на этап 4.

---

## 11. Выкат скриптом `scripts/deploy`

Штатный выкат production — только этот набор (решение владельца после 1.19.0: до этого каждый выкат писал свой cutover-скрипт, и пять выкатов подряд, 1.16.0–1.18.1, падали на его пути записи). Сборку делает `scripts/build-release.sh` (контейнер Debian 13 на сборочной машине), а `scripts/deploy` проверяет готовый dist, раскладывает его, репетирует и выполняет один scripted cutover, проверяет результат и чистит старые релизы. Своего `build.sh` у набора нет.

Идентичность релиза — 12 hex-символов коммита; dist лежит в `.superpowers/deploy/dist/<sha12>`. Все хосты, пути и юниты — в `scripts/deploy/deploy.conf` (секретов там нет), а умолчания там и в `build-release.sh`/`gate-remote.sh` — нейтральный пример: значения своего стенда кладут в `.superpowers/site.env` (путь переопределяет `CANTERLOT_SITE_ENV`), образец — `docs/deploy/site.env.example`; явная переменная окружения главнее файла. Значения токенов, `credential`, манифеста, спек и `spec.toml` не читаются и не печатаются; `docker inspect` — только с `--format`, `printenv` — только для `CANTERLOT_CSP`/`GODEBUG`.

### 11.1. Порядок

Боевой прогон — **только с токеном** от dry-run или репетиции того же плана, не старше 2 часов:

```bash
SHA=<12 hex релиза>; DIST=.superpowers/deploy/dist; SRC=<чекаут, из которого собран dist>
scripts/deploy/stage.sh "$SHA" --source "$SRC" --dist "$DIST"
scripts/deploy/cutover.sh "$SHA" --dry-run  --source "$SRC" --dist "$DIST"   # только чтение
scripts/deploy/cutover.sh "$SHA" --rehearse --source "$SRC" --dist "$DIST"   # весь путь записи, записи только печатаются
# оба печатают CONFIRM=<epoch>-<план>-<проверка>; боевой прогон — с ним, в течение 2 часов:
scripts/deploy/cutover.sh --execute "$SHA" --confirm <CONFIRM> --source "$SRC" --dist "$DIST"
```

`verify.sh` и `retain.sh --apply` боевой прогон вызывает сам. Пошаговый ранбук для делегата — §11.7.

- **`--dry-run`** — только чтение: `preflight` и `target-check` против цели, ворота чтения, проверка обслуживания, превью слияния чанков, `verify.sh --dry-run` против развёрнутой базы, печать плана.
- **`--rehearse`** — репетиция (1.19.0 делал её руками): **весь** путь записи против живой системы, но каждая пишущая команда проходит через единственную точку `run_write` и только печатается строкой `WOULD-WRITE #N (метка): команда`. Все чтения, ожидания, разборы и циклы выполняются по-настоящему: ворота, снимок PID/тиков, скан управляющего воркера (обязан найти ровно живой процесс юнита), полное чтение самого свежего существующего дампа, цикл `readyz`, PID/exe/`NRestarts` хаба, разбор журнала хаба, чтение env и `version.json`, CSP, ожидание старта воркера против настоящего юнита — с подмешанными пустым `MainPID`, `MainPID=0` и ENOENT у `/proc/<pid>/exe` на переходе exec, плюс реальные переходы exec собственных коротких детей — разбор `state="live"` из журнала, счёт слияния чанков, обслуживание, `verify.sh` (loopback-проба только печатается), план `retain.sh` без `--apply`. Локально пишутся только state-файл снимка в `/tmp` и временные файлы счётчиков.
- **`--execute <id> --confirm <токен>`** — боевой прогон. Позиционный SHA в этом режиме не принимается, `--shadow` запрещён.

**Токен** (`<epoch>-<план>-<проверка>`) годен 2 часа (`CONFIRM_TTL=7200`) и привязан к id релиза и к хэшу точного плана (`plan_text` в `lib.sh`: цель, развёрнутые hub/web/worker, хэши `SHA256SUMS`/`RELEASE`/`EXPECT`/`csp.txt` из dist, хосты, пути, юниты, хэш текста самого toolkit и число хранимых релизов). Боевой прогон пересчитывает план до любого действия и отказывает с понятной причиной: просрочен (когда выдан и сколько минут назад), выдан для другого релиза или изменён, план изменился (оба хэша). Токен печатается только при PASS; провалившиеся dry-run и репетиция токена не дают.

`--shadow` (с `--dry-run` или `--rehearse`) разрешает проверять уже развёрнутый релиз — «выкатить то, что уже стоит»; равенство цели и развёрнутого релиза тогда не провал.

`source` любого скрипта набора ничего не исполняет: в начале стоит `[[ "${BASH_SOURCE[0]}" == "$0" ]] || return 0` (урок 1.16.1, когда `source` cutover-скрипта реально остановил воркер). Библиотеки (`lib.sh`, `tests/lib-harness.sh`, `tests/cutover-fixture.sh`) не исполняемые, и это проверяет линтер.

### 11.2. Проверка dist

`target-check.sh` проверяет цель только чтением:

- `SHA256SUMS` сходится, `RELEASE` содержит `sha=` и `source=` (полный 40-hex коммит, начинающийся с release id);
- `--version` хаба и воркера (и синтетического воркера) запускается и версии согласованы;
- `ldd` хаба/воркера/синтетического воркера не содержит `libssl`;
- максимальная версия символа GLIBC не выше `GLIBC_MAX` (по умолчанию 2.41, прод Debian 13);
- `canterlot-hub openapi` побайтно равен `docs/api/openapi.json` из чекаута `--source`;
- `EXPECT` (если есть) равен выводу `canterlot-worker capabilities` боевого воркера этого dist;
- `csp.txt` выведен из финального `web/200.html` того же dist, `_app/version.json` равен релизу, состав корня web взят из dist;
- залитые каталоги на хабе, веб-хосте и воркере совпадают с dist по каждому файлу; после слияния старых immutable-чанков допустимы только лишние файлы под `_app/immutable/`;
- миграции цели против базы: база не должна быть впереди, число ожидающих миграций печатается.

**Что релиз ждёт от воркера, объявляет сам релиз.** `scripts/release-build/in-container.sh` пишет в dist файл `EXPECT` (входит в `SHA256SUMS`) из вывода `canterlot-worker capabilities`: `format=1`, `capabilities=` (список `OPTIONAL_CAPABILITIES` из `crates/canterlot-worker/src/session.rs`) и `harnesses=` (харнессы этой сборки). `verify.sh` требует от машины в хабе все объявленные возможности и те объявленные харнессы, что включены в `worker.toml` (таблица есть и `enabled` не `false`; читаются только имена таблиц и ключ `enabled`). Релиз, который намеренно убирает харнесс или возможность, правки toolkit не требует. Dist без `EXPECT` (релизы до 1.20) — прежний список (`claude_histories_v1`, `path_probe_v1`; `claude_code`, `codex`, `opencode`) с предупреждением `WARNING: no …/EXPECT`.

`stage.sh` копирует только tar-конвейерами, проверяет полный пофайловый состав и точный корень stagedir, публикует атомарным `mv -T` и не трогает `current`/`build`. Для openapi ему обязателен `--source` того чекаута, из которого собран dist.

### 11.3. Инвариант живой сессии

`preflight.sh` снимает для каждого живого исполнения PID и стартовый тик супервизора, CLI и всех настоящих релеев (`mcp-relay` + exec id из `--socket .../executions/<id>/mcp.sock`), а PID воркера — отдельным множеством. Во время cutover проверяется инвариант, а не равенство множеств: **каждый записанный процесс обязан быть жив с тем же тиком**; новые транзиентные дети (вызовы инструментов, второй CLI на мгновение) допустимы и только печатаются (инцидент 1.17.0).

Ворота 1 `preflight.sh` сверяют супервизоры из `/proc` со строками хаба. Живой строкой считается только `running`. Супервизор, чья строка находится в переходном состоянии (`created`, `starting`, `stopping`, `owner_lost`; см. `legal_transition` в `crates/canterlot-hub/src/executions.rs`), законен: делегат, запущенный за секунды до preflight, ещё в `starting` (dry-run 1.24.0 упал на этом ложно). Такие строки preflight ждёт до `SUPERVISOR_SETTLE_WAIT` секунд (по умолчанию 60, опрос каждые `SUPERVISOR_SETTLE_POLL`, по умолчанию 5), каждый раз перечитывая и хаб, и `/proc`. Если строка дошла до `running`, сессия проверяется как обычная и попадает в снимок тиков. Если строка всё ещё переходная, это SOFT-FAIL с названием состояния, и сессия в снимок не попадает. Супервизор без строки или с терминальной строкой (`stopped`, `failed`) — HARD-FAIL, без ожидания. Строка `running` без супервизора, как и раньше, HARD-FAIL. Неосвобождённая запись ownership такого супервизора отдельной ошибкой не считается. Cutover этим не ослаблен: `snapshot_drift` сверяет только записанные процессы, а супервизор, появившийся после снимка (новая сессия во время выката), и раньше был допустим и не проверялся. Сессия, оставшаяся в переходном состоянии, учитывается так же. `verify.sh` и проверка обслуживания сверяют только направление «строка `running` → процесс», лишние каталоги исполнений лишь печатают, поэтому переходные строки их не задевают.

Остановка юнита не доказывает отсутствие воркера: `find_control_worker` берёт кандидатов с basename `canterlot-worker` у exe или argv[0], пропускает только точные подкоманды `supervise`/`mcp-relay`/`lease`, не матчит подстроку в пути (инцидент 1.16.0). Из кандидатов прод-воркером считается процесс, у которого exe (с отрезанным ` (deleted)`: бинарь релиза удалён после старта) или абсолютный argv[0] лежит под корнем установки `WORKER_BASE` (`~/.local/opt/canterlot`, то есть `current` и `releases/<sha12>`), либо `run` с `--config`, указывающим на прод-конфиг `WORKER_CONFIG`, откуда бы ни был запущен бинарь; процесс с нечитаемым exe и относительным argv[0] тоже считается (классифицировать нельзя). Тестовые воркеры стек-тестов делегатов (`<worktree>/target/debug/canterlot-worker run --config <стек>/worker.toml` против одноразового хаба) выкат не блокируют (репетиция 1.23.2): скан пишет одну строку `control-worker scan: ignoring non-production canterlot-worker processes …` с pid и путём бинаря, без прочих аргументов. В отчёте перечисляются все найденные прод-воркеры через `; `, и репетиция требует ровно один — `MainPID` юнита. `preflight.sh` считает прод-сессиями только сопровождения с `--exec-dir`, равным прямому каталогу `WORKER_DATA/executions/<id>`: сопровождения тестовых воркеров из worktree под `WORKER_DATA` в сверку с хабом не попадают. Сканы `/proc` сопоставляют argv точно, без `pgrep -f` и без подстроки по склеенной командной строке.

### 11.4. Один scripted cutover

Порядок один и тот же, окно остановки не ждёт человека. Каждая запись идёт через `run_write` (в репетиции — печать), каждое чтение после записи сверяется с ожидаемым состоянием:

1. проверка токена; ворота чтения (KillMode=`process`, живые исполнения, нет открытых stop/interrupt и компактификаций, миграции, нет `vzdump` и блокировки CT, `readyz`, вне окна 03:30); инвариант сессий; резерв цели и предыдущих релизов на всех хостах;
2. стоп воркера; остановка подтверждается чтением (`ActiveState`), не паузой; управляющих воркеров не осталось; инвариант сессий без воркера;
3. свежий `vzdump` (вывод обязан содержать `Backup job finished successfully`, id архива разбирается и по форме `ct/<ctid>/<время>Z`) и `pg_dump -Fc -Z zstd:3` под именем с меткой прогона; дамп проверяется **до** flip хаба: `pg_restore --list` (TOC содержит `_sqlx_migrations`), полное чтение `pg_restore -f /dev/null` и размер не меньше `DUMP_MIN_BYTES` (64 МБ: zstd-дамп примерно на 13% меньше gzip);
4. flip хаба под host lock, отдельное чтение ссылки, один рестарт, `readyz`, миграции, `migrations_applied` в журнале с момента рестарта, PID/exe хаба только непустые и ненулевые (урок 1.19.0), `NRestarts=0`;
5. PWA: `canterlot.env.<sha>` из `csp.txt` (0600, чтение назад), flip `build`, один `docker compose up -d caddy` (id контейнера до/после печатается), `version.json` и CSP в контейнере;
6. flip воркера и один старт; защищённое ожидание `MainPID` с читаемым exe (`wait_worker_exe`: пустой `MainPID`, `MainPID=0` и ENOENT на переходе exec — не обрыв `set -e`, а следующий опрос; инцидент 1.18.1); exe из нового релиза; ожидание **голого** `state="live"` в журнале юнита (инциденты 1.16.0 и 1.19.0: разбор без конвейера с ранним выходом); `NRestarts=0`; инвариант сессий; печатается измеренный простой;
7. **слияние старых чанков PWA** после старта воркера и до любой чистки: `_app/immutable` до трёх предыдущих релизов веба копируется в новый релиз `cp -rn` под web lock (открытые вкладки догружают свои чанки); затем чтение: ни одного пропущенного относительного пути из каждого источника и каждый файл dist нового релиза байт в байт на месте (`sha256sum -c`);
8. **обслуживание**: у каждой живой сессии есть каталог исполнения и сокет `/run/user/<uid>/canterlot/<id>.sock`; на хостах нет `current.new`, `build.new` и `releases/.stage-*`; `canterlot.env.<web>` — 0600 и равен живому env. Нарушения — мягкие ошибки (в dry-run и репетиции — провал);
9. `verify.sh`: два чтения (пауза `VERIFY_READ_GAP`, 60 с), версии, миграции, соединение воркера в текущем запуске, **каталог машины после старта** — возможности и харнессы из `EXPECT` релиза (с ожиданием до `CATALOG_WAIT` секунд), CSP, precompressed `Content-Encoding` (`br`/`zstd`/`gzip`, мягкая ошибка) на первом по `LC_ALL=C` полностью предсжатом JS-ассете релиза, счётчики ошибок журналов (новый вид WARN/ERROR хаба против суточной базы — жёсткая ошибка, кроме видов WARN из `VERIFY_WARN_KINDS_OK`, по умолчанию `config_key_deprecated sign_in_global_lock client_ip_missing`; неудачные входы, отказы CSRF на публичных путях входа и пределы запусков QR и ключа доступа хаб пишет INFO и в счёт не идут), курсоры SJ/CJ, коды сайтов (`SITE_CODES`: `hub.example.com` отдаёт оболочку PWA с кодом 200, `auth.example.com` не проверяется — узел выключен);
10. **база GeoIP**: путь выкат спрашивает у выкатываемого хаба (`canterlot-hub geoip path --config /etc/canterlot/hub.toml` печатает действующий `[auth.geoip].path`, так что выкат и хаб смотрят в один файл); если файла на хабе нет или он старше `GEOIP_MAX_AGE_DAYS` (35) дней, `canterlot-hub geoip update` от имени `canterlot` через `run_write` (в репетиции печатается, dry-run только сообщает возраст); неудача — мягкая ошибка, места входа остаются пустыми;
11. retention (`retain.sh --apply`) под своими `/proc`-guard'ами; цель и предыдущие релизы передаются `--keep`;
12. чек-лист проверок со входом для владельца и шаблон раздела журнала выката.

**Loopback-Caddy** для precompressed (решение владельца: допустим в боевом прогоне): один одноразовый процесс `caddy run` внутри контейнера на `127.0.0.1:18080` для всех трёх кодировок, под `timeout` (`PROBE_LIFETIME`, 60 с), с ловушкой на `EXIT`/`INT`/`TERM`/`HUP`; занятый порт — отказ до старта; старт и останов (`probe: loopback caddy pid N started/stopped`) попадают в журнал, а отдельное чтение после пробы доказывает, что порт снова закрыт. Проверяется первый по `LC_ALL=C` `.js`-файл под `_app/immutable`, у которого в релизе есть все три соседа (`<файл>.br`, `<файл>.zst`, `<файл>.gz`); если такого файла нет, это `SOFT-FAIL`, как и любая другая неудача пробы. В репетиции проба только печатается, в dry-run — `SKIPPED`.

Политика отката: **до** flip хаба любая неудача или сигнал — только рестарт воркера на его прежнем релизе (хаб и база нетронуты). **С** flip хаба воркер гасится только пока он не доказанно жив на новом релизе (не дошёл до `state="live"`, exe не из нового релиза, `NRestarts` != 0, инвариант сессий нарушен); после этого (слияние чанков, обслуживание, проверка, retention) неудача или сигнал — СТОП-и-доклад с точным состоянием, а хаб, PWA и воркер остаются работать, retention пропускается. Причина: оркестратор и доклады живут на той же машине воркера, и его остановка глушит все отчёты (после 1.19.1 владелец поднимал воркер руками 25 минут, хотя релиз уже был проверен живым). Precompressed `Content-Encoding` — мягкая ошибка, как и остальные PWA-проверки. Автоматического отката базы нет никогда; ручной откат — только `rollback-db.sh` с проверенным дампом. PWA-ошибки мягкие: записать, вернуть предыдущий build, продолжить. Неудача репетиции ничего не восстанавливает (восстанавливать нечего) и пишет `REHEARSAL FAIL at phase …`.

**Откат хаба на релиз без собственного входа (1.29 и раньше): сначала блок входа в Caddy.** Такой хаб берёт владельца из `Remote-User` прокси с секретом, а Caddy после перехода на собственный вход ни логина, ни пароля не спрашивает. Поэтому до flip хаба назад (строка 1 таблицы отката в `STOP-AND-REPORT`) и до `rollback-db.sh --to <старый релиз>` в Caddy возвращается `basic_auth` или `forward_auth` (копия Caddyfile, `caddy validate`, reload), чистка `Remote-*` остаётся, затем `check-proxy.sh`. Это правило стоит строкой 0 в таблице отката `cutover.sh` и напоминанием в `rollback-db.sh`. Там же, до старта старого хаба:

- из `/etc/canterlot/hub.toml` убрать таблицу `[auth]` (вместе с `[auth.geoip]`) и ключ `session_check_interval` в `[http.ws]`: конфиг старого хаба `deny_unknown_fields`, с ними он не стартует;
- если старый хаб пойдёт на этой же базе (без `rollback-db.sh`), отменить ждущие пуши входа: старый хаб не читает строку пуша без разговора, и весь его проход доставки падает, пока они не устареют.
  ```bash
  ssh hub "runuser -u canterlot -- psql -h /var/run/postgresql -d canterlot -c \"update owner_pushes set state='cancelled',updated_at=clock_timestamp() where state='pending' and kind in ('sign_in','sign_in_failures','sign_in_lock','session_country')\""
  ```
  После `rollback-db.sh` с дампом до релиза этого не нужно: таких строк там нет.
- откат воркера ниже этого релиза: сначала убрать `[execution].delegate_mcp_servers` из `worker.toml` (тот же `deny_unknown_fields`).

### 11.5. Retention

Автоматически после проверенного cutover (решение владельца). `retain.sh` держит 3 свежих релиза на компонент плюс каждый релиз, из которого живёт процесс (`/proc/<pid>/exe`), плюс всё, что передано `--keep` (цель и предыдущие релизы каждого компонента); локальный dist — так же (3 свежих + `--keep`). Никогда не удаляются цель, цели `current`/`build`, релиз, который реально раздаёт Caddy, и зарезервированные живой операцией. Без `--apply` только печатает план. Удаление — только буквальными абсолютными путями: каждый путь печатается (`REMOVE <путь>`) и проверяется заново непосредственно перед удалением (форма пути, имя релиза, не `current`/`build`, ни один процесс не исполняет файл из каталога, нет резерва); финальная сверка и удаление — одной удалённой командой под тем же `.deploy.lock`, что у flip и recreate; каждое удаление идёт через `run_write`, а `removed` печатается только после чтения пути с того хоста, где он живёт. Маркер резерва хранит прогон, pid и время: резерв устарел, только когда прогон уже не работает и маркер старше 2 ч (срока токена плана), и тогда `retain.sh` под тем же lock снимает его строкой `stale reservation of <sha> from <when> released` и может удалить релиз. Свежий резерв и резерв живого прогона уважаются; оставленные после STOP-AND-REPORT резервы защищают цель и предыдущий релиз, пока следующий успешный прогон не снимет их как устаревшие.

### 11.6. Тесты

`scripts/deploy/tests/run.sh` — shellcheck, синтаксис python, статическая проверка `tests/pipe-lint.py`, юнит-проверки `lib.sh`, затем наборы **параллельно** (каждый в своём `FAKE_ROOT`; `RUN_JOBS=N` ограничивает число, `RUN_SUITES="…"` выбирает наборы; лог каждого набора — в каталоге, который печатает прогон). Hermetic-стенд с подменёнными ssh/systemctl/docker/curl/psql/pg_dump/pg_restore/pvesh/vzdump и настоящими процессами сессий.

`pipe-lint.py` ловит классы ошибок, которые уже роняли прод: `cmd | head|grep -q|sed …q|awk …exit` под `pipefail` (SIGPIPE 141, 1.19.0); незащищённое `VAR="$(…)"` в цикле ожидания под `set -e` (1.18.0, 1.18.1); `[ … ] && break|continue` и `[ … ] && cmd` последней командой тела цикла (1.17.0); `source` исполняемого файла или точки входа (1.16.1); `pgrep -f`, `ps | grep <литерал>` и подстроку `*canterlot-worker*` по командной строке (1.16.0); любую пишущую команду в скриптах выката мимо `run_write` (иначе репетиция её выполнила бы). Подавление — комментарий `# lint: <правило>-ok <причина>`; `test-lint.sh` проверяет, что каждое правило ловит образец и молчит на исправленной форме.

Наборы: `lint`, `token` (2 часа, чужой релиз, чужой план, порча, старый суточный токен), `rehearse` (ничего не пишет, печатает каждую запись, сценарии ожидания старта, токен репетиции открывает боевой прогон того же плана, изменённый план и просроченный токен — отказ, провал репетиции без токена, shadow), `cutover` и `cutover-signals` (боевой путь, слияние чанков, обслуживание, пустой/нулевой `MainPID` при старте, `NRestarts`, `vzdump` без строки успеха, маленький дамп, сигналы в каждой фазе), `verify` (`EXPECT`, отключённый харнесс, убранный харнесс, поздний каталог, loopback-проба: старт/стоп/порт), `dryrun` (токен только при PASS, `EXPECT` против бинаря), `preflight`, `stage`, `retain`, `rollback-db`, `guardrails`, `invariants`, `site-env` (файл стенда перекрывает умолчания, явная переменная окружения — файл), `geoip` (путь из `canterlot-hub geoip path`, нет файла, свежий файл не трогается, старше 35 дней, неудача, хаб без пути и нечитаемый хаб — мягкие, репетиция только печатает), `check-proxy` (подменённые сайт и CLI хаба: JSON-401 хаба принят, `401` и `302` от Caddy и `X-Canterlot-Proxy-Leak` — провал, вход и выход по ссылке, адрес клиента, jar удалён, ссылка не напечатана). Ожидания стенда событийные (супервизор ищется точным сканом `/proc`, а не `pgrep -f` с паузой), бинарь фейкового воркера собирается один раз на прогон.

### 11.7. Ранбук оператора (выкат следующего релиза)

Выполнять дословно, на машине воркера, из чекаута, где лежит toolkit. Подставить `SHA` (12 hex), `VER` (`X.Y.Z`), `SRC` (чекаут, из которого собран dist) и `DIST` (каталог с `dist/<SHA>`). Логи сохранять в `.superpowers/deploy/` основного чекаута.

1. **Dist.** `cd "$DIST/$SHA" && sha256sum --quiet -c SHA256SUMS && cat RELEASE EXPECT` — все строки OK, `source=` начинается с `SHA`, `EXPECT` содержит `format=1`. В журнал: число файлов и строку `capabilities=`.
2. **Stage.** `scripts/deploy/stage.sh "$SHA" --source "$SRC" --dist "$DIST" 2>&1 | tee .superpowers/deploy/stage-$VER.log` — конец `staged <SHA> everywhere; current/build untouched`.
3. **Dry-run.** `scripts/deploy/cutover.sh "$SHA" --dry-run --source "$SRC" --dist "$DIST" 2>&1 | tee .superpowers/deploy/dryrun-$VER.log` — конец `dry-run PASS`; проверить в логе `target check PASS`, `preflight PASS`, `verify PASS`, строки `maintenance:` и `chunk merge preview:`. Любой `SOFT-FAIL`/`HARD-FAIL` — стоп, разобраться, повторить.
4. **Репетиция.** `scripts/deploy/cutover.sh "$SHA" --rehearse --source "$SRC" --dist "$DIST" 2>&1 | tee .superpowers/deploy/rehearsal-$VER.log` — конец `rehearsal PASS: … N writes printed as WOULD-WRITE, none executed`. Проверить: строки `start-wait scenario` (пять штук и `real exec transitions`), `the control-worker scan finds exactly the live unit process`, `dump fully readable`, `verify PASS`; `current`/`build` на всех хостах не изменились.
5. **Боевой прогон** — в течение 2 часов после шага 3 или 4, без правок dist и toolkit между ними: скопировать строку `real run:` из конца лога и выполнить, добавив `2>&1 | tee .superpowers/deploy/cutover-$VER.log`. Если токен отвергнут (`expired` или `plan changed`) — повторить шаг 4. Конец: `cutover done: hub=SHA, worker=SHA, web=SHA, down N s, soft failures 0`.
6. **Если `STOP-AND-REPORT`** — ничего не делать сверх напечатанного: вставить блок отчёта в журнал и позвать владельца. Если воркер уже проверен живым на новом релизе, он, хаб и PWA остаются работать: остановка воркера заглушила бы доклады, которые идут с этой же машины, поэтому руками его не гасить. Ручные команды восстановления в отчёте берут те же host lock; откат базы — только `rollback-db.sh`.
7. **Журнал выката.** В `.superpowers/deploy/pilot-deploy-log.md` вставить шаблон раздела из конца лога cutover и заполнить: релиз и тег, токен (время выдачи, хэш плана) и пути трёх логов, строки `vzdump done`, `dump fully readable`, `hub on …`, `worker process … from release`, `worker state="live" … down time`, `chunk merge:`, `maintenance:`, `capabilities present`/`harness catalog has`, `precompressed …`, `verify PASS` (оба чтения), список `removed` из retention, все `SOFT-FAIL` как Deviations.
8. **Приёмка прокси** — `EXPECTED_LOGIN=<логин владельца> scripts/check-proxy.sh https://hub.example.com --positive` (§4.3): без сеанса JSON `401` хаба и вычищенные поддельные заголовки, затем вход по одноразовой ссылке во временный jar, адрес клиента в списке сеансов, выход. Владельцу придёт push о входе с этой машины; ссылка нигде не печатается.
9. **Владельцу** — чек-лист `owner checklist: authenticated checks` из конца лога (вход в PWA паролем или ключом доступа и баннер обновления, `GET /api/v1/config`, живая сессия, новые функции релиза) в раздел «Left for the owner».

### 11.8. Первый релиз с собственным входом

До этого релиза хаб брал владельца из `Remote-User` прокси, а перед `hub.example.com` стоит временный `basic_auth` в Caddy (узел Authelia выключен). Порядок первого выката (ADR-0005, спека входа §12), строго по шагам:

1. **Обычный выкат по §11.7 с `basic_auth` на месте.** Пока он стоит, `https://hub.example.com/` отвечает `401`, а новый хаб пишет новые виды WARN: `config_key_deprecated` (устаревшие `user_header` и `logout_url` в `hub.toml`), `client_ip_missing` (Caddy ещё не передаёт адрес клиента) и, если временный Caddy передаёт `Remote-User`, `identity_header_forwarded`. До dry-run на хосте хаба: в боевой `/etc/canterlot/hub.toml` добавить
   ```toml
   [auth.geoip]
   path = "/var/lib/canterlot/geoip/dbip-city-lite.mmdb"
   ```
   и создать каталог базы, принадлежащий служебному пользователю хаба: `install -d -o canterlot -g canterlot -m 0755 /var/lib/canterlot/geoip`. Без этой строки путь по умолчанию — `data/geoip/…` от каталога конфигурации, то есть `/etc/canterlot/data/geoip/`, куда `canterlot` писать не может (`geoip_dir_error`). Работающий старый хаб читает `hub.toml` только при старте, поэтому правка ему не мешает; но перезапускать старый хаб с ней нельзя (`[auth]` ему неизвестна, конфиг `deny_unknown_fields`), а при откате ниже этого релиза секция снимается (§11.4, правило отката на релиз без собственного входа). Гейт 3 preflight сверяет `hub.toml` с новыми структурами и секцию пропускает. Проверка: `runuser -u canterlot -- /opt/canterlot/releases/<sha>/bin/canterlot-hub geoip path --config /etc/canterlot/hub.toml` печатает `/var/lib/canterlot/geoip/dbip-city-lite.mmdb`. Затем для dry-run, репетиции и боевого прогона этого релиза в той же оболочке:
   ```bash
   export SITE_CODES="hub.example.com:401 git.example.com:200"
   export VERIFY_WARN_KINDS_OK="config_key_deprecated sign_in_global_lock client_ip_missing identity_header_forwarded"
   ```
   Переменные в план не входят, токен от них не меняется. Шаг geoip скачивает базу мест входа (файла ещё нет); его `SOFT-FAIL` выкат не останавливает, повтор — та же команда руками (`runuser -u canterlot -- /opt/canterlot/current/bin/canterlot-hub geoip update --config /etc/canterlot/hub.toml` на хабе). `check-proxy.sh` на этом шаге не запускается: `basic_auth` ещё отвечает вместо хаба.
2. **Ссылка входа владельцу — только через Gotify.** На хосте хаба:
   ```bash
   ssh hub 'runuser -u canterlot -- /opt/canterlot/current/bin/canterlot-hub auth recovery-link --config /etc/canterlot/hub.toml --push'
   ```
   Ожидаемый вывод — `sent`; в доклад идёт только «ссылка отправлена в Gotify». Если команда отвечает ошибкой (`notify_not_configured`, недоступен Gotify и т. п.) — стоп: ссылку без `--push` выкат не выпускает. Оркестратору передаётся точная команда, которую владелец выполнит сам в своём ssh, и ссылка не появляется ни в одном транскрипте и журнале:
   ```bash
   ssh hub 'runuser -u canterlot -- /opt/canterlot/current/bin/canterlot-hub auth recovery-link --config /etc/canterlot/hub.toml'
   ```
3. **Ждать подтверждения владельца**, что он вошёл по ссылке (PWA пока за `basic_auth`: сначала пароль Caddy, затем страница входа) и задал пароль или ключ доступа. Без подтверждения Caddy не трогать.
4. **Caddy — одна просмотренная правка.** На хосте web сначала посмотреть глобальные опции Caddyfile (блок `{ … }` в начале файла и импортируемые из него файлы): есть ли `servers { trusted_proxies … }`. Если есть, это не мешает правке ниже (она берёт `{remote_host}`, а не `{client_ip}`), но в доклад идёт строка с найденным значением; `{client_ip}` в этой правке не использовать ни в каком случае (§4.2). Затем копия Caddyfile рядом с меткой времени (`cp -p Caddyfile Caddyfile.bak-<YYYYMMDDTHHMMSSZ>`), затем один diff, который показывается целиком до применения:
   - убрать блок `basic_auth` сайта `hub.example.com` (и копирование `Remote-User` в запрос к хабу, если оно есть);
   - оставить чистку `request_header -Remote-*` и `request_header -X-Forwarded-*`, оставить или добавить `request_header -X-Canterlot-Proxy` и `request_header -X-Canterlot-Client-Ip`;
   - рядом с существующим `header_up X-Canterlot-Proxy {file.…}` добавить `header_up X-Canterlot-Client-Ip {remote_host}` (§4.2).

   Затем `docker exec caddy caddy validate --config <Caddyfile в контейнере> --adapter caddyfile` и только при успехе `docker exec caddy caddy reload --config <Caddyfile в контейнере> --adapter caddyfile`. Ошибка проверки — вернуть копию, Caddy не перезагружать.
5. **Приёмка.** Сначала без сеанса, затем с входом (владельцу придёт push о входе):
   ```bash
   EXPECTED_LOGIN=<логин владельца> scripts/check-proxy.sh https://hub.example.com
   EXPECTED_LOGIN=<логин владельца> scripts/check-proxy.sh https://hub.example.com --positive
   ```
   Провал любой проверки — вернуть копию Caddyfile (reload) и доложить; хаб при этом не откатывается. После приёмки `SITE_CODES` и `VERIFY_WARN_KINDS_OK` из оболочки убираются (умолчания уже ждут `hub.example.com:200`), а `user_header` и `logout_url` стоит убрать из `hub.toml`, чтобы `config_key_deprecated` не повторялся при каждом рестарте.
