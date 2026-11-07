# Инструкция по развёртыванию пилота Canterlot

Настоящее руководство описывает архитектуру и шаги развёртывания пилотного контура Canterlot: хаба с базой данных PostgreSQL в изолированном контейнере Proxmox LXC, воркера на рабочей виртуальной машине владельца и единой точки входа на обратном прокси Caddy с аутентификацией через Authelia.

Все параметры конфигурации и команды CLI сопоставлены с исходным кодом проекта (`crates/canterlot-hub`, `crates/canterlot-worker`, `web`, `docs/design`).

---

## 1. Схема архитектуры и используемые адреса

### 1.1. Таблица сетевых параметров и заполнителей

В руководстве используются следующие типовые значения (заменяются на фактические параметры инфраструктуры):

| Параметр | Значение в примере | Назначение |
|---|---|---|
| Адрес хаба (LXC) | `10.0.0.10` | Внутренний IP-адрес контейнера Proxmox LXC с хабом и PostgreSQL |
| Адрес хоста Caddy | `10.0.0.2` | Внутренний IP-адрес отдельного сервера Web/Edge с Caddy и Authelia |
| Адрес машины воркера | `10.0.0.20` | Внутренний IP-адрес рабочей VM владельца с воркером и агентами |
| Публичное имя сайта | `chat.example.com` | Внешний HTTPS-домен для доступа к PWA и API |
| Внутреннее DNS-имя хаба | `hub.example.internal` | Доменное имя хаба для gRPC TLS (указано в сертификате SAN) |

### 1.2. Архитектурная схема

Размещение компонентов строго следует решениям [pilot.md](../design/pilot.md#2-размещение) (§2) и [ADR-0004](../adr/0004-browser-auth-forward-auth.md):

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
|  |  3. route: зачистка семейств заголовков Remote-*, X-Forwarded-*, Proxy     |  |
|  |  4. forward_auth: запрос к Authelia (проверка SSO сессии по cookie)        |  |
|  |  5. reverse_proxy: проксирование @api (/api/* и /healthz) на порт хаба    |  |
|  |     + инъекция заголовка секрета X-Canterlot-Proxy                          |  |
|  +-----------------------------------------------------------------------------+  |
|                         |                          |                              |
|           HTTP (auth)   v                          | HTTP / WSS                   |
|       +---------------------+                      |                              |
|       | Authelia (SSO / 2FA)|                      |                              |
|       +---------------------+                      |                              |
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
owner_login = "owner"                               # логин в Authelia
user_header = "Remote-User"                         # default Remote-User
allowed_origins = ["https://chat.example.com"]
# max_json_body_bytes опущен — действует значение по умолчанию 524288 (512 KiB)
header_read_timeout = "10s"
body_read_timeout = "30s"
max_connections = 256
proxy_secret_file = "/etc/canterlot/proxy.secret"
logout_url = "https://auth.example.com/logout"
multi_process = false                               # true включает LISTEN/NOTIFY

[http.ws]
max_connection_age = "30m"                          # принудительный реконнект для повторной аутентификации
max_subscriptions = 16
queue_messages = 64                                 # уменьшено против умолчания 256; диапазон 2..=256
queue_bytes = 65536                                 # уменьшено против умолчания 1048576; диапазон 4096..=1048576
delta_window = "25ms"                               # не более 25ms
poll_interval = "2s"
stall_timeout = "15s"

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
2. Вставляет строку владельца в `owners` со свежим UUID (`installation.rs:62-65`); логин `owner_login` задаётся только в HTTP-конфиге и сопоставляется позже с заголовком идентичности при входе ([crates/canterlot-hub/src/http/auth.rs:132](../../crates/canterlot-hub/src/http/auth.rs#L132)).
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

Caddy выполняет функции внешнего TLS-терминатора, сервера статических файлов PWA, шлюза SSO через Authelia и обратного прокси к хабу ([docs/adr/0004-browser-auth-forward-auth.md](../adr/0004-browser-auth-forward-auth.md), [docs/http.md](../http.md#http-вход-csrf-и-caddy)).

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
> Блок `route { ... }` критически необходим. В стандартном порядке директив Caddy удаление заголовков (`request_header -...`) выполнилось бы **после** `forward_auth`, удалив только что проставленный заголовок аутентификации. Внутри `route` порядок выполнения строго соответствует порядку строк в конфиге!
>
> Директивы `request_header -Remote-*`, `request_header -X-Forwarded-*` и `request_header -X-Canterlot-Proxy` зачищают **семейства префиксов**, блокируя любые попытки клиента подделать заголовки пользователя (`Remote-User`, `Remote-Groups`, `Remote-Name`, `Remote-Email`), параметры прокси (`X-Forwarded-For`, `X-Forwarded-Proto`, `X-Forwarded-Host`, `X-Forwarded-Uri`) или секрет прокси (`X-Canterlot-Proxy`).

```caddyfile
chat.example.com {
    route {
        # 1. Зачистка семейств заголовков клиента ДО передачи в Authelia
        request_header -Remote-*
        request_header -X-Forwarded-*
        request_header -X-Canterlot-Proxy

        # 2. Проверка сессии владельца через Authelia forward-auth
        forward_auth authelia:9091 {
            uri /api/authz/forward-auth
            copy_headers Remote-User Remote-Groups Remote-Name Remote-Email
        }

        # 3. Политика безопасности контента (значение из переменной CANTERLOT_CSP)
        header Content-Security-Policy "{$CANTERLOT_CSP}"

        # 4. Проксирование API, WebSocket и healthcheck на хаб (за forward_auth, как и всё остальное)
        @api path /api/* /healthz
        handle @api {
            # Сжатие ответов хаба; WebSocket-апгрейды Caddy не сжимает.
            encode zstd gzip
            reverse_proxy 10.0.0.10:8080 {
                header_up X-Canterlot-Proxy {file./etc/caddy/secrets/canterlot-proxy}
            }
        }

        # 5. Переходный ответ для старых установок с Web Share Target
        @share {
            method POST
            path /_share
        }
        handle @share {
            redir * /share?error=sw_inactive 303
        }

        # 6. Раздача статики PWA: manifest без кэширования
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

`file_server { precompressed zstd br gzip }` отдаёт заранее сжатые соседние файлы `<name>.br`, `<name>.zst` и `<name>.gz`, которые создаёт `npm run build`; каталог статики переносится на сервер целиком, вместе с ними. `encode zstd gzip` внутри `@api` сжимает ответы хаба, но не трогает WebSocket-апгрейды.

В `redir` матчер `*` обязателен: единственный аргумент, начинающийся с `/`, Caddy читает как матчер пути, а не как адрес перехода (`redir /share?error=sw_inactive 303` даёт `302` с заголовком `Location: 303`).

### 4.3. Приёмка прокси скриптом `check-proxy.sh`

После каждого изменения конфигурации Caddy или Authelia обязателен запуск тестового скрипта ([scripts/check-proxy.sh](../../scripts/check-proxy.sh)):
```bash
bash scripts/check-proxy.sh https://chat.example.com /path/to/cookies.txt
```
Где `/path/to/cookies.txt` — куки-файл curl с активной сессией владельца. Скрипт гарантирует:
- Неавторизованный запрос отклоняется Authelia (3xx/401/403) и не доходит до хаба.
- Поддельные заголовки `Remote-User: attacker` и `X-Canterlot-Proxy: forged` отбрасываются прокси, а хаб видит только реальный логин владельца.

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

[execution.supervisor]
attach_wait = "30s"
ready_deadline = "30s"
transmit_deadline = "10s"
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
- [ ] **7. Caddy и Authelia**: блок `chat.example.com` зачищает семейства заголовков `Remote-*`, `X-Forwarded-*` и `X-Canterlot-Proxy`, выполняет `forward_auth`, выставляет `Content-Security-Policy` и проксирует `@api` (`/api/*` и `/healthz`) на `10.0.0.10:8080`.
- [ ] **8. Разрешение имени на воркере**: в `/etc/hosts` воркера прописана строка `10.0.0.10 hub.example.internal`, команда `getent hosts hub.example.internal` возвращает IP хаба.
- [ ] **9. Сопряжение машины**: выполнена команда `canterlot-hub machine add`, полученный токен сохранён в `~/.config/canterlot/token` с правами `0600`, UUID вписаны в `worker.toml`.
- [ ] **10. Права хранилища воркера**: каталог `~/.local/state/canterlot-worker` создан с правами `0700`.
- [ ] **11. Службы systemd**:
  - Хаб: `systemctl status canterlot-hub` активен (active/running).
  - Воркер: `systemctl --user status canterlot-worker` активен (active/running), директива `KillMode=process` установлена.
  - Linger: выполнен `loginctl enable-linger $USER`.
- [ ] **12. Тестирование прокси**: запуск `bash scripts/check-proxy.sh https://chat.example.com cookies.txt` завершается успешно (код возврата 0).
- [ ] **13. Сквозная доступность и приёмка**:
  - Публичная проверка сертификата и шлюза (через Authelia, нужен jar с сессией, как для `check-proxy.sh`):
    ```bash
    curl -fsS -b cookies.txt https://chat.example.com/healthz
    # Ожидается: {"status":"ok"}
    ```
    Без сессии Authelia ответ будет переходом на страницу входа: `/healthz` закрыт `forward_auth`, как и весь хост.
  - Локальная проверка готовности внутри LXC хаба:
    ```bash
    curl -fsS http://10.0.0.10:8080/readyz
    # Ожидается: {"status":"ok"}
    ```
  - В браузере открывается `https://chat.example.com`, после входа через Authelia загружается PWA.
  - В журнале хаба (`journalctl -u canterlot-hub`) фиксируется подключение воркера с сообщением: `session accepted` (атрибуты `kind = "session_accepted"`, `machine_id = ...`).

---

## 10. Ограничения и функциональность `TODO`

Следующие возможности зафиксированы в архитектуре, но не входят в текущую кодовую базу пилота:

1. **TODO: Пакетный установщик**: сборка deb/rpm пакетов отсутствует. Компоненты компилируются через Cargo и устанавливаются вручную.
2. **TODO: Автоматическая ротация TLS gRPC**: ротация сертификатов внутреннего центра сертификации и хаба выполняется вручную через OpenSSL.
3. **TODO: Автоматическое обновление CSP**: заголовок CSP генерируется утилитой `print-csp.mjs` и передаётся в переменную `CANTERLOT_CSP` при обновлении фронтенда.
4. **TODO: Автоматическое восстановление R7**: при рассинхронизации состояния воркер аварийно останавливается; автоматическая сверка фактов запланирована на этап 4.
5. **TODO: Собственный экран входа Canterlot**: в пилоте аутентификация полностью делегирована связке Caddy + Authelia; встроенный логин Canterlot разрабатывается для последующих этапов.

---

## 11. Выкат скриптом `scripts/deploy`

Штатный выкат production — только этот набор. Скрипты пишутся и тестируются один раз, дальше запускаются; ручные шаги §7 не используются. Сборку делает `scripts/build-release.sh` (контейнер Debian 13 на сборочной машине), а `scripts/deploy` только проверяет готовый dist, раскладывает его, выполняет один scripted cutover и проверяет результат. Своего `build.sh` у набора нет.

Идентичность релиза — 12 hex-символов коммита; dist лежит в `.superpowers/deploy/dist/<sha12>`. Все хосты, пути и юниты — в `scripts/deploy/deploy.conf` (секретов там нет). Значения токенов, `credential`, манифеста, спек и `spec.toml` не читаются и не печатаются; `docker inspect` — только с `--format`, `printenv` — только для `CANTERLOT_CSP`/`GODEBUG`.

### 11.1. Порядок

```bash
SHA=$(git rev-parse --short=12 HEAD)                  # после сборки тега
scripts/build-release.sh vX.Y.Z                       # dist в .superpowers/deploy/dist
scripts/deploy/stage.sh "$SHA" --source "$PWD" --dist .superpowers/deploy/dist
scripts/deploy/cutover.sh "$SHA" --source "$PWD" --dist .superpowers/deploy/dist --dry-run
# dry-run печатает CONFIRM=<hash>; боевой прогон — только с ним:
scripts/deploy/cutover.sh --execute "$SHA" --confirm <hash> --source "$PWD" --dist .superpowers/deploy/dist
scripts/deploy/verify.sh "$SHA" --dist .superpowers/deploy/dist
scripts/deploy/retain.sh --apply --keep "$SHA" --dist .superpowers/deploy/dist
```

`preflight.sh`, `cutover.sh` и `verify.sh` принимают `--dry-run`. Боевой `cutover.sh` **отказывает** без явного `--execute <release-id>` и верного `--confirm <token>`: токен печатает dry-run, он детерминирован для пары «release-id + текущие UTC-сутки». Позиционный SHA в боевом режиме не принимается. `--shadow` (только вместе с `--dry-run`) разрешает проверять уже развёрнутый релиз — например, «выкатить то, что уже стоит»; равенство цели и развёрнутого релиза тогда не провал.

`source` любого скрипта набора ничего не исполняет: в начале стоит `[[ "${BASH_SOURCE[0]}" == "$0" ]] || return 0` (урок 1.16.1, когда `source` cutover-скрипта реально остановил воркер). Помощники живут в `lib.sh` и тестируются отдельно.

### 11.2. Проверка dist

`target-check.sh` проверяет цель только чтением:

- `SHA256SUMS` сходится, `RELEASE` содержит `sha=` и `source=` (полный 40-hex коммит, начинающийся с release id);
- `--version` хаба и воркера (и синтетического воркера) запускается и версии согласованы;
- `ldd` хаба/воркера/синтетического воркера не содержит `libssl`;
- максимальная версия символа GLIBC не выше `GLIBC_MAX` (по умолчанию 2.41, прод Debian 13);
- `canterlot-hub openapi` побайтно равен `docs/api/openapi.json` из чекаута `--source` (это исправляет старую ошибку `stage.sh`, который сравнивал openapi со своим чекаутом инструмента);
- `csp.txt` выведен из финального `web/200.html` того же dist, `_app/version.json` равен релизу, состав корня web взят из dist;
- залитые каталоги на хабе, веб-хосте и воркере совпадают с dist по каждому файлу; после ручного слияния старых immutable-чанков допустимы только лишние файлы под `_app/immutable/`;
- миграции цели против базы: база не должна быть впереди, число ожидающих миграций печатается.

`stage.sh` копирует только tar-конвейерами, проверяет полный пофайловый состав и точный корень stagedir, публикует атомарным `mv -T` и не трогает `current`/`build`. Для openapi ему обязателен `--source` того чекаута, из которого собран dist.

### 11.3. Инвариант живой сессии

`preflight.sh` снимает для каждого живого исполнения PID и стартовый тик супервизора, CLI и всех настоящих релеев (`mcp-relay` + exec id из `--socket .../executions/<id>/mcp.sock`), а PID воркера — отдельным множеством. Во время cutover проверяется инвариант, а не равенство множеств: **каждый записанный процесс обязан быть жив с тем же тиком**; новые транзиентные дети (вызовы инструментов, второй CLI на мгновение) допустимы и только печатаются. Так инцидент 1.17.0 (лишний CLI-ребёнок против ровно 15 строк) больше не откатывает воркер, а исчезновение записанного PID по-прежнему жёсткая ошибка.

Остановка юнита не доказывает отсутствие воркера: `find_control_worker` ищет exe/argv[0] с basename `canterlot-worker`, пропускает только точные подкоманды `supervise`/`mcp-relay`, не матчит подстроку в пути (инцидент 1.16.0 с обёрткой в пути, содержащем `canterlot-worker`).

### 11.4. Один scripted cutover

Порядок один и тот же, окно остановки не ждёт человека:

1. ворота чтения (KillMode=`process`, живые исполнения, нет открытых stop/interrupt/steer и компактификаций, миграции, нет `vzdump` и блокировки CT, `readyz`, вне окна 03:30); инвариант сессий;
2. стоп воркера; проверка, что управляющих воркеров не осталось; инвариант сессий без воркера;
3. свежий `vzdump` и `pg_dump` под именем с меткой прогона; дамп проверяется **до** flip хаба: `pg_restore --list` (TOC содержит `_sqlx_migrations`) и полное чтение `pg_restore -f /dev/null`;
4. flip хаба (`cd /opt/canterlot`, `ln -s` + `mv -T`), один рестарт, `readyz`, миграции, процесс хаба из нового релиза;
5. flip PWA, `canterlot.env.<sha>`, один `docker compose up -d caddy`, проверка `version.json` и CSP;
6. flip воркера и один старт; ожидание **голого** `state="live"` в журнале юнита (поле `state=`, а не `time="…"` после него — инцидент 1.16.0), без `[ … ] && break` под `set -e` (инцидент 1.17.0); печатается измеренный простой воркера;
7. `verify.sh`: два чтения, версии, миграции, возможности (`claude_histories_v1`, `path_probe_v1`) и три харнесса (`claude_code`, `codex`, `opencode`), CSP, precompressed `Content-Encoding` (`br`/`zstd`/`gzip`) одноразовым loopback-Caddy внутри контейнера, счётчики ошибок журналов обеих сторон, курсоры SJ/CJ, коды сайтов;
8. retention (`retain.sh --apply`) под своими `/proc`-guard'ами; цель и предыдущие релизы передаются `--keep` и не удаляются.

Политика отката не меняется: **до** flip хаба любая неудача или сигнал — только рестарт воркера на его прежнем релизе (хаб и база нетронуты); **с** flip хаба — СТОП и доклад с точным состоянием и ручными командами восстановления, воркер остаётся остановленным. Автоматического отката базы нет никогда; ручной откат — только `rollback-db.sh` с проверенным дампом. PWA-ошибки мягкие: записать, вернуть предыдущий build, продолжить.

В конце печатается готовый шаблон раздела для `.superpowers/deploy/pilot-deploy-log.md` с фактическими версией, временем простоя и результатами проверок. Скрипт ничего не коммитит.

### 11.5. Retention

`retain.sh` держит 3 свежих релиза на компонент плюс каждый релиз, из которого живёт процесс (`/proc/<pid>/exe`), плюс всё, что передано `--keep`; локальный dist — так же. Без `--apply` только печатает. Удаление — только буквальными абсолютными путями, каждый проверен непосредственно перед удалением: не `current`/`build`, не живой процесс, не зарезервирован идущей операцией; финальная сверка и удаление — одной удалённой командой под тем же `.deploy.lock`, что у flip и recreate.

### 11.6. Тесты

`scripts/deploy/tests/run.sh` — shellcheck, синтаксис python, затем hermetic-набор с подменёнными ssh/systemctl/docker/curl/psql/pg_dump/pg_restore/pvesh и настоящими процессами сессий. Отдельно покрыты инциденты: обёртка с `canterlot-worker` в пути не матчится; строки `time="…"` не удовлетворяют ожиданию live; `source` ничего не запускает; новый транзиентный CLI-ребёнок не ломает инвариант, а исчезнувший записанный PID ломает; ожидание live не обрывается `set -e`; боевой прогон без верного `--confirm` отказывает. Dry-run доказуемо ничего не пишет (снимок всех файлов до/после и дозор).
