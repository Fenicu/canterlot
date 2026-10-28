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
|  |  - Локальный журнал управления (CJ) и кэш токена машины (0600)              |  |
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

Хаб и база данных PostgreSQL изолированы в выделенном непривилегированном контейнере Proxmox LXC на базе **Debian 13 (trixie)** ([docs/design/pilot.md#2-размещение](../design/pilot.md#2-размещение) §2).

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
  sudo -u postgres psql -c "CREATE DATABASE canterlot OWNER canterlot;"
  printf 'postgres://canterlot:%s@127.0.0.1:5432/canterlot\n' "$DB_PASS" > /etc/canterlot/db.url
  chown canterlot:canterlot /etc/canterlot/db.url
)
```

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

Ниже приведён эталонный файл конфигурации со всеми полями, валидируемыми структурой `HubConfig` ([crates/canterlot-hub/src/config.rs:14](../../crates/canterlot-hub/src/config.rs#L14)):

```toml
# Корневые параметры хаба
database_url_file = "/etc/canterlot/db.url"         # crates/canterlot-hub/src/config.rs:15
grpc_listen = "10.0.0.10:7443"                      # crates/canterlot-hub/src/config.rs:16 (обязательное поле, без значения по умолчанию)
tls_cert = "/etc/canterlot/tls/hub.crt"             # crates/canterlot-hub/src/config.rs:17
tls_key = "/etc/canterlot/tls/hub.key"              # crates/canterlot-hub/src/config.rs:18
manifest = "/etc/canterlot/manifest/installation.toml" # crates/canterlot-hub/src/config.rs:19

[timers]                                            # crates/canterlot-hub/src/config.rs:38
handshake = "10s"                                   # crates/canterlot-hub/src/config.rs:40
# degraded_after (15s, config.rs:59), unavailable_after (30s, config.rs:60)
# и command_stored_receipt (5s, config.rs:63) опущены — действуют значения по умолчанию из кода
http2_keepalive_interval = "30s"                    # crates/canterlot-hub/src/config.rs:46
http2_keepalive_timeout = "10s"                     # crates/canterlot-hub/src/config.rs:48
dispatch_sweep = "5s"                               # crates/canterlot-hub/src/config.rs:52

[http]                                              # crates/canterlot-hub/src/config.rs:141
listen = "10.0.0.10:8080"                           # crates/canterlot-hub/src/config.rs:142 (привязка к IP хаба в LXC; default в коде: 127.0.0.1:8080, config.rs:162)
trusted_proxies = ["10.0.0.2/32"]                   # crates/canterlot-hub/src/config.rs:143 (IP отдельного хоста Caddy /32)
owner_login = "owner"                               # crates/canterlot-hub/src/config.rs:144 (логин в Authelia)
user_header = "Remote-User"                         # crates/canterlot-hub/src/config.rs:145 (default Remote-User)
allowed_origins = ["https://chat.example.com"]      # crates/canterlot-hub/src/config.rs:146
# max_json_body_bytes опущен — действует значение по умолчанию 524288 (512 KiB), crates/canterlot-hub/src/config.rs:167
header_read_timeout = "10s"                         # crates/canterlot-hub/src/config.rs:149
body_read_timeout = "30s"                           # crates/canterlot-hub/src/config.rs:151
max_connections = 256                               # crates/canterlot-hub/src/config.rs:152
proxy_secret_file = "/etc/canterlot/proxy.secret"   # crates/canterlot-hub/src/config.rs:154
logout_url = "https://auth.example.com/logout"      # crates/canterlot-hub/src/config.rs:155
multi_process = false                               # crates/canterlot-hub/src/config.rs:156 (true включает LISTEN/NOTIFY)

[http.ws]                                           # crates/canterlot-hub/src/config.rs:289
max_connection_age = "30m"                          # crates/canterlot-hub/src/config.rs:291 (принудительный реконнект для повторной аутентификации)
max_subscriptions = 16                              # crates/canterlot-hub/src/config.rs:292
queue_messages = 64                                 # crates/canterlot-hub/src/config.rs:293 (диапазон 2..=256)
queue_bytes = 65536                                 # crates/canterlot-hub/src/config.rs:294 (диапазон 4096..=1048576)
delta_window = "25ms"                               # crates/canterlot-hub/src/config.rs:296 (не более 25ms)
poll_interval = "2s"                                # crates/canterlot-hub/src/config.rs:298
stall_timeout = "15s"                               # crates/canterlot-hub/src/config.rs:300

[limits]                                            # crates/canterlot-hub/src/config.rs:319
weekly_reserve_pct = 95                             # crates/canterlot-hub/src/config.rs:320 (порог включения «кранча», 0..=100)
snapshot_min_interval = "10m"                       # crates/canterlot-hub/src/config.rs:322
history_days = 90                                   # crates/canterlot-hub/src/config.rs:323
default_resume_text = "Лимиты восстановлены, продолжай с того места, где остановился." # crates/canterlot-hub/src/config.rs:324
reset_grace = "20s"                                 # crates/canterlot-hub/src/config.rs:326
verify_backoff = ["30s", "1m", "2m", "5m"]          # crates/canterlot-hub/src/config.rs:328
poll_min_interval = "5m"                            # crates/canterlot-hub/src/config.rs:330
unknown_reset_retry = "30m"                         # crates/canterlot-hub/src/config.rs:332
reserve_exit_hysteresis_permille = 20               # crates/canterlot-hub/src/config.rs:335
warn_pcts = [80, 90, 95]                            # crates/canterlot-hub/src/config.rs:336
stale_after = "35m"                                 # crates/canterlot-hub/src/config.rs:338

[limits.poll_floor]                                 # crates/canterlot-hub/src/config.rs:334
claude = "30m"                                      # crates/canterlot-hub/src/config.rs:377
codex = "5m"                                        # crates/canterlot-hub/src/config.rs:378

[activity]                                          # crates/canterlot-hub/src/config.rs:396
stalled_generating = "5m"                           # crates/canterlot-hub/src/config.rs:398
stalled_tool = "30m"                                # crates/canterlot-hub/src/config.rs:400
tick = "15s"                                        # crates/canterlot-hub/src/config.rs:402
debounce = "250ms"                                  # crates/canterlot-hub/src/config.rs:404

[scheduler]                                         # crates/canterlot-hub/src/config.rs:420
max_pending_per_conversation = 50                   # crates/canterlot-hub/src/config.rs:421
retry_queue_full = "60s"                            # crates/canterlot-hub/src/config.rs:423
max_attempts = 20                                   # crates/canterlot-hub/src/config.rs:424
idle_poll = "1s"                                    # crates/canterlot-hub/src/config.rs:426

[attachments]                                       # crates/canterlot-hub/src/config.rs:442
blob_dir = "/var/lib/canterlot/attachments"         # crates/canterlot-hub/src/config.rs:443
max_concurrent_uploads = 4                          # crates/canterlot-hub/src/config.rs:444
gc_grace = "24h"                                    # crates/canterlot-hub/src/config.rs:446
gc_interval = "1h"                                  # crates/canterlot-hub/src/config.rs:448

[machines]                                          # crates/canterlot-hub/src/config.rs:464
disk_warn_pct = 85                                  # crates/canterlot-hub/src/config.rs:465
metrics_stale_after = "90s"                         # crates/canterlot-hub/src/config.rs:467
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

Если Caddy запущен в контейнере Docker, файл остаётся на хосте с правами `0600`, владельцем назначается UID, от которого работает процесс Caddy в контейнере, а в контейнер файл монтируется только для чтения по пути, указанному в директиве `{file.…}` Caddyfile (README использует `/run/secrets/canterlot-proxy`). Путь в Caddyfile и путь монтирования должны совпадать.

### 2.6. Инициализация хаба (`canterlot-hub init`)

Команда инициализации ([crates/canterlot-hub/src/main.rs:30](../../crates/canterlot-hub/src/main.rs#L30)) выполняется **ровно один раз**:
```bash
mkdir -p /etc/canterlot/manifest /var/lib/canterlot/attachments
chown -R canterlot:canterlot /etc/canterlot /var/lib/canterlot

sudo -u canterlot canterlot-hub init --config /etc/canterlot/hub.toml
```

Команда:
1. Применяет миграции базы данных sqlx.
2. Создаёт учётную запись владельца `owner_login`.
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

Caddy выполняет функции внешнего TLS-терминатора, сервера статических файлов PWA, шлюза SSO через Authelia и обратного прокси к хабу ([docs/adr/0004-browser-auth-forward-auth.md](../adr/0004-browser-auth-forward-auth.md), [README.md](../../README.md#http-вход-csrf-и-caddy)).

### 4.1. Сборка фронтенда PWA и генерация CSP

Перед запуском PWA собирается в статические файлы:
```bash
cd web
npm ci
npm run build
```

Скрипт `web/scripts/print-csp.mjs` вычисляет SHA-256 хеши инлайн-скриптов из `web/build/200.html` и генерирует заголовок Content-Security-Policy ([README.md:445](../../README.md#L445)):
```bash
export CANTERLOT_CSP="$(node scripts/print-csp.mjs https://chat.example.com)"
```
Переменная `CANTERLOT_CSP` передаётся в окружение службы Caddy и подставляется директивой `header Content-Security-Policy "{$CANTERLOT_CSP}"`.

### 4.2. Конфигурация Caddyfile

Конфигурация сайта строго воспроизводит канонический блок из [README.md:447-480](../../README.md#L447-L480):

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

        # 4. Проксирование API, WebSocket и публичного healthcheck на хаб
        @api path /api/* /healthz
        handle @api {
            reverse_proxy 10.0.0.10:8080 {
                header_up X-Canterlot-Proxy {file./etc/caddy/secrets/canterlot-proxy}
            }
        }

        # 5. Обработка Web Share Target при неактивном Service Worker
        @share {
            method POST
            path /_share
        }
        handle @share {
            redir /share?error=sw_inactive 303
        }

        # 6. Раздача статики PWA (service-worker и manifest без кэширования)
        handle {
            @fresh path /service-worker.js /manifest.webmanifest
            header @fresh Cache-Control "no-cache"
            root * /var/www/canterlot/build
            try_files {path} /200.html
            file_server
        }
    }
}
```

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

Полный набор полей, валидируемый структурой `WorkerConfig` ([crates/canterlot-worker/src/config.rs:13](../../crates/canterlot-worker/src/config.rs#L13)):

```toml
# Корневые параметры соединения и инсталляции
hub_url = "https://hub.example.internal:7443"        # crates/canterlot-worker/src/config.rs:14
ca_cert = "/home/user/.config/canterlot/ca.pem"      # crates/canterlot-worker/src/config.rs:15
token_file = "/home/user/.config/canterlot/token"    # crates/canterlot-worker/src/config.rs:16 (права 0600)
machine_id = "019a1b10-0000-7000-8000-000000000001" # crates/canterlot-worker/src/config.rs:17
installation_id = "019a1b10-0000-7000-8000-000000000002" # crates/canterlot-worker/src/config.rs:18
restore_generation = "019a1b10-0000-7000-8000-000000000003" # crates/canterlot-worker/src/config.rs:19
data_dir = "/home/user/.local/state/canterlot-worker" # crates/canterlot-worker/src/config.rs:20 (права 0700)

[timers]                                            # crates/canterlot-worker/src/config.rs:56
handshake = "10s"                                   # crates/canterlot-worker/src/config.rs:58
http2_keepalive_interval = "30s"                    # crates/canterlot-worker/src/config.rs:60
http2_keepalive_timeout = "10s"                     # crates/canterlot-worker/src/config.rs:62
heartbeat = "5s"                                    # crates/canterlot-worker/src/config.rs:64
heartbeat_ack = "5s"                                # crates/canterlot-worker/src/config.rs:66
range_ack = "10s"                                   # crates/canterlot-worker/src/config.rs:68
backoff_min = "250ms"                               # crates/canterlot-worker/src/config.rs:70
backoff_max = "30s"                                 # crates/canterlot-worker/src/config.rs:72
backoff_reset_after = "30s"                         # crates/canterlot-worker/src/config.rs:74

[execution]                                         # crates/canterlot-worker/src/config.rs:35
# Белый список путей, в которых разрешено запускать сессии агентов
allowed_workdir_roots = ["/home/user/myprojects", "/home/user/work"] # crates/canterlot-worker/src/config.rs:36
supervisor_exe = "/usr/local/bin/canterlot-worker"  # crates/canterlot-worker/src/config.rs:37 (current_exe по умолчанию)
spawn_timeout = "10s"                               # crates/canterlot-worker/src/config.rs:39

[execution.supervisor]                              # crates/canterlot-worker/src/config.rs:79
attach_wait = "30s"                                 # crates/canterlot-worker/src/config.rs:81
ready_deadline = "30s"                              # crates/canterlot-worker/src/config.rs:83
transmit_deadline = "10s"                           # crates/canterlot-worker/src/config.rs:85
stop_grace = "5s"                                   # crates/canterlot-worker/src/config.rs:87
stop_retry = "2s"                                   # crates/canterlot-worker/src/config.rs:89

[machine]                                           # crates/canterlot-worker/src/config.rs:272
metrics_interval = "30s"                            # crates/canterlot-worker/src/config.rs:274

[attachments]                                       # crates/canterlot-worker/src/config.rs:296
fetch_parallelism = 2                               # crates/canterlot-worker/src/config.rs:297 (1 или 2)
fetch_retry = "2s"                                  # crates/canterlot-worker/src/config.rs:299
fetch_attempts = 5                                  # crates/canterlot-worker/src/config.rs:300
fetch_backoff_cap = "30s"                           # crates/canterlot-worker/src/config.rs:302

[[instruction_profiles]]                            # crates/canterlot-worker/src/config.rs:287
name = "executor"                                   # crates/canterlot-worker/src/config.rs:288
title = "Исполнитель"                               # crates/canterlot-worker/src/config.rs:289
description = "Профиль без персоны (нейтральный агент)" # crates/canterlot-worker/src/config.rs:290
harnesses = ["claude_code", "codex"]                # crates/canterlot-worker/src/config.rs:291
```

### 6.3. Права доступа к файлам и каталогам

- `token_file`: обязательно права `0600` (`chmod 600 ~/.config/canterlot/token`).
- `data_dir`: создаётся воркером автоматически с правами `0700`.
- Все каталоги исполнений `data_dir/executions/<id>` имеют права `0700`, а сокеты IPC и `spec.toml` — `0600`.

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
2. Внутри новой сессии процесс сопровождения запускает дочерний CLI харнесса (`claude` или `codex`).
3. При перезапуске воркера (`systemctl --user restart canterlot-worker`):
   - Главный процесс воркера корректно завершается.
   - Процессы сопровождения и CLI продолжают выполняться без сбоев.
   - Весь вывод CLI и промежуточные события буферизуются процессом сопровождения в локальный сессионный журнал на диске: `data_dir/executions/<execution_id>/journal.db` (`SJ`).
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
   cp target/release/canterlot-hub /usr/local/bin/canterlot-hub
   systemctl restart canterlot-hub
   ```
   *Хаб перезапускается первым*. На старте команда `serve` автоматически выполняет накат новых миграций базы данных PostgreSQL под advisory lock `sqlx`.
2. **Caddy (Web Server)**:
   При изменениях в PWA:
   - Обновите файлы сборки: `/var/www/canterlot/build/`.
   - Запустите скрипт генерации CSP: `export CANTERLOT_CSP="$(node web/scripts/print-csp.mjs https://chat.example.com)"`.
   - Перезагрузите сервис Caddy с обновлённой переменной окружения: `systemctl restart caddy` (или `caddy reload`).
3. **Воркер (Рабочая VM)**:
   ```bash
   cp target/release/canterlot-worker /usr/local/bin/canterlot-worker
   systemctl --user restart canterlot-worker
   ```
   Воркер перезапускается последним.

### 7.3. Что выживает при обновлении

- **Процессы сопровождения и агентские CLI**: благодаря `KillMode=process`, вызову `setsid` и сохранению сессионных журналов `SJ` работа агентов продолжается без пауз.
- **Очереди сообщений и отложенные вводы**: хранятся в PostgreSQL хаба, планировщик подхватывает их сразу после старта хаба.
- **Вложения**: незавершённые загрузки файлов возобновляются с сохранённого байтового смещения (`Upload-Offset`).
- **Согласование протокола (`pilot_v1`)**: при реконнекте воркер и хаб заново согласуют возможности в handshake. Если хаб старше воркера, воркер придерживает новые факты; откат хаба на версию ниже уже задействованных возможностей не поддерживается ([README.md](../../README.md#обновление-контракта-пилота)).

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
1. При последующем подключении воркера к хабу расхождение курсоров фиксируется как неустранимая ошибка рассинхронизации: воркер возвращает ошибку `WorkerError::StateDivergence` ([crates/canterlot-worker/src/session.rs:836-840](../../crates/canterlot-worker/src/session.rs#L836-L840)).
2. Процесс воркера немедленно завершает работу с кодом выхода 1 ([crates/canterlot-worker/src/main.rs:98-100](../../crates/canterlot-worker/src/main.rs#L98-L100)).

**Штатные пути восстановления в пилоте**:
- **Синхронный откат**: одновременное восстановление LXC хаба и VM воркера из парных снапшотов Proxmox VE, снятых в один и тот же момент времени при остановленных сервисах.
- **Сброс локального состояния воркера**: администратор хаба выполняет ротацию токена со сбросом локального состояния через CLI ([crates/canterlot-hub/src/main.rs:48-56](../../crates/canterlot-hub/src/main.rs#L48-L56)):
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
  - Публичная проверка сертификата и шлюза:
    ```bash
    curl -fsS https://chat.example.com/healthz
    # Ожидается: {"status":"ok"}
    ```
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
