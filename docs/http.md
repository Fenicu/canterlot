# HTTP: вход, CSRF и Caddy

Хаб слушает обычный HTTP во внутренней сети; TLS завершает Caddy. Владельца хаб проверяет сам: страница входа в PWA, пароль, ссылка восстановления, сеансы браузера в cookie (ADR-0005). Authelia и любой другой узел для входа не нужны.

## Транспорт

- Запрос принимается, только если TCP-адрес собеседника входит в `trusted_proxies`; адрес берётся из сокета, `X-Forwarded-*` не читается никогда.
- При заданном `proxy_secret_file` запрос несёт ровно один `X-Canterlot-Proxy` с тем же значением. Секрет нужен, когда у веб-хоста общий адрес для всех контейнеров (NAT): один адрес доверенного прокси ещё не доказывает, что запрос прошёл через Caddy. Создайте его без перевода строки: `umask 077; openssl rand -hex 32 | tr -d '\n' > proxy-secret` (только видимые ASCII-символы, не менее 32).
- Адрес клиента хаб берёт из одного разборчивого `X-Canterlot-Client-Ip`, и только при настроенном и совпавшем секрете. Без секрета или без заголовка адрес неизвестен: все попытки пароля попадают в одну корзину перебора, список сеансов показывает «IP неизвестен», журнал раз в 10 минут пишет `client_ip_missing`.

Нарушение транспорта даёт `401 unauthenticated` на любом пути, кроме `GET /healthz` и `GET /readyz`.

## Вход и сеансы

- Личность даёт только cookie `__Host-canterlot_session=<токен>; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=<срок бездействия>`. Читается только это имя; два значения — `401 unauthenticated`. Префикс `__Host-` не даёт соседним приложениям `*.example` подбросить свою cookie. Chromium и Firefox принимают её и на `http://localhost`, и на `http://127.0.0.1`.
- Хаб ищет сеанс в базе по SHA-256 токена на каждом запросе, без кэша. Отозванный сеанс (и через 30 дней его надгробие, ещё 400 дней) даёт `401 session_revoked` с `details.reason`, истёкший по бездействию — `401 session_expired`, неизвестный — `401 unauthenticated`; все три стирают cookie (`Max-Age=0`). Ошибка базы при поиске сеанса — `503 db_unavailable`, а не 401.
- `last_seen_at` и последний адрес пишутся не чаще раза в минуту или сразу при смене адреса; тогда же ответ продлевает cookie.
- Cookie известного устройства `__Host-canterlot_device` (400 дней) ставится при каждом входе и остаётся после выхода. Она ничего не удостоверяет: только выводит попытки пароля из-под общей блокировки и даёт им свою корзину.
- Без сеанса доступны только `GET /api/v1/auth/methods`, `POST /api/v1/auth/password`, `POST /api/v1/auth/recovery`, `POST /api/v1/auth/passkey/start`, `POST /api/v1/auth/passkey/finish`, `POST /api/v1/auth/qr/start`, `POST /api/v1/auth/qr/poll` (точный метод и путь), `GET /healthz` и `GET /readyz`. Всё остальное, включая WSS, медиа и вложения, требует сеанса.

Пути `/api/v1/auth`:

| Метод и путь | Сеанс | Что делает |
| --- | --- | --- |
| `GET /methods` | нет | `{password, passkey}`: задан ли пароль, есть ли ключи доступа |
| `POST /password` | нет | `{login, password, device?}` → сеанс и cookie; `401 invalid_credentials`; `429 too_many_attempts` с `details.retry_after_seconds` и `Retry-After` |
| `POST /recovery` | нет | `{token, device?}` → сеанс по ссылке восстановления |
| `POST /passkey/start` | нет | `{}` → `{challenge_id, options}` для `navigator.credentials.get()`; `409 no_passkeys`, если ключей нет |
| `POST /passkey/finish` | нет | `{challenge_id, credential, device?}` → сеанс и cookie; `401 invalid_credentials` |
| `POST /qr/start` | нет | `{device?}` → `{url, poll_token, expires_at}`: код на 60 секунд |
| `POST /qr/poll` | нет | `{poll_token, after?}` → `{state, session?}`; при `approved` — сеанс и cookie |
| `POST /qr/inspect` | да | `{secret}` → карточка запроса, код становится `scanned`; `410 qr_expired` |
| `POST /qr/approve` | да | `{secret}` → `{state: "approved"}`; свежести не требует |
| `POST /qr/deny` | да | `{secret}` → `{state: "denied"}` |
| `GET /sessions` | да | `{current_id, idle_timeout_days, fresh_until, sessions}`: живые сеансы, текущий первым |
| `PATCH /sessions/{id}` | да | `{name}`: имя до 64 символов, пустое снимает |
| `DELETE /sessions/{id}` | да | свой id — выход; чужой требует свежего сеанса и уносит каскадом сеансы, вход которых он подтвердил |
| `POST /sessions/revoke-others` | свежий | завершить все остальные сеансы |
| `POST /logout` | да | завершить текущий сеанс, стереть cookie |
| `PATCH /settings` | да | `{idle_timeout_days}`: 7, 30, 90, 180 или 365 |
| `PUT /password` | да | `{current_password?, new_password}`: смена несёт текущий пароль, первый пароль требует свежего сеанса; успех завершает остальные сеансы |
| `POST /reauth/password` | да | `{password}` → сеанс свеж 10 минут; неверный пароль — `403 wrong_password` |
| `POST /reauth/passkey/start` | да | `{}` → `{challenge_id, options}` |
| `POST /reauth/passkey/finish` | да | `{challenge_id, credential}` → сеанс свеж 10 минут; неудача — `403 passkey_rejected` |
| `GET /passkeys` | да | `{passkeys: [{id, name, created_at, last_used_at, backed_up}]}` |
| `POST /passkeys/register/start` | свежий | `{}` → `{challenge_id, options}` для `navigator.credentials.create()` |
| `POST /passkeys/register/finish` | да | `{challenge_id, credential, name?}` → ключ; без имени — подпись устройства; `403 passkey_rejected` |
| `PATCH /passkeys/{id}` | да | `{name}`: 1..64 символа |
| `DELETE /passkeys/{id}` | свежий | удалить ключ; ответ — оставшиеся ключи |

`device` — подсказки клиента для подписи устройства (объект до 2 КиБ, только для показа): `model`, `platform`, `platformVersion`, `fullVersionList` (`[{brand, version}]`) из `navigator.userAgentData.getHighEntropyValues()` и `standalone: true`, когда PWA открыта как приложение (`display-mode: standalone`). Свежий сеанс — пароль, ключ доступа или ссылка восстановления за последние 10 минут; иначе действие управления доступом отвечает `403 reauth_required`. `GET /api/v1/me` возвращает `owner_id`, `login` и `session_id`.

### Подпись устройства и место

Подпись (`label`) выводится при чтении из user agent и подсказок клиента, подсказки главнее: «браузер версия · система [версия] · модель», в конце « · приложение» для установленной PWA. Браузеры: Firefox, Chrome, Safari, Edge (в том числе их версии для iOS); системы: Windows (по подсказкам 10 или 11), macOS, Linux, Android, iPhone и iPad вместо iOS. Примеры: «Firefox 159 · Linux», «Chrome 142 · Android 16 · Pixel 9», «Safari 19 · iPhone · приложение». Нераспознанные части пропускаются, ничего не распознано — `null`. Подпись заявлена клиентом; сырой `user_agent` тоже есть в виде сеанса.

Место (`location`) ищется на хабе в локальной базе DB-IP (абзац «Место по IP» в [configuration.md](configuration.md)) один раз, когда адрес сеанса записывается впервые, и хранится на строке: `{city, region, country, country_code, local}`. Названия русские, если есть в базе, иначе английские. Частные, петлевые, link-local и CGNAT адреса дают `{local: true}` без остальных полей («локальная сеть»); без файла базы место `null`. Когда у живого сеанса меняется страна последнего адреса, уходит пуш «Сеанс «…» используется из новой страны: …», не больше одного на сеанс и страну; страна входа уведомления не даёт.

### Ключи доступа

WebAuthn через `webauthn-rs`: проверяющая сторона — `[auth].rp_id` и `[auth].origin` (только этот origin), имя «canterlot». Поток без discoverable: вход перечисляет все ключи владельца в `allowCredentials`, проверка пользователя обязательна, conditional mediation нет. Регистрация исключает уже добавленные ключи и просит `residentKey: preferred`. `options` — JSON вида `{publicKey: …}` с двоичными полями в base64url (подходит для `PublicKeyCredential.parseCreationOptionsFromJSON` / `parseRequestOptionsFromJSON`), `credential` — `PublicKeyCredential.toJSON()`. Состояние обряда хранится в базе 5 минут и забирается один раз любой попыткой finish; регистрация и повторный вход привязаны к начавшему их сеансу. После каждого входа сохраняются счётчик и признак резервной копии ключа и `last_used_at`. Начало входа ограничено 10 разами в минуту с корзины и 50 живыми обрядами всего. Неудачный finish входа или повторного входа считается неудачей перебора в корзине клиента; общая блокировка входа по паролю ключей не касается. Если WebAuthn не принимает origin (IP-адрес), ключи отвечают `409 passkeys_unavailable`, а `/methods` — `passkey: false`.

### Вход по QR

1. Новое устройство: `POST /qr/start` → `url` = `{origin}/qr#<secret>` (его показывает QR) и `poll_token` (остаётся на устройстве). Код живёт 60 секунд; страница начинает новый каждые 60 секунд. Не больше 10 запусков в минуту с корзины адресов и 20 ожидающих кодов всего, сверх — `429 too_many_attempts` сразу.
2. Оно же опрашивает `POST /qr/poll` с `after` — состоянием, которое уже показывает (по умолчанию `pending`). Ответ приходит, как только состояние станет другим, или через 25 секунд с тем же состоянием. Хаб перепроверяет строку каждые 500 мс и по уведомлению шины `auth`, поэтому подтверждение в другом процессе тоже будит опрос. Одновременно ждут не больше 8 опросов в процессе, девятый — `429`.
3. Устройство с сеансом открывает ссылку (страница `/qr` сразу убирает фрагмент из адреса): `POST /qr/inspect` переводит код в `scanned` и возвращает карточку `{created_at, expires_at, age_seconds, ip, location, label, user_agent, other_country}`. Адрес, место и возраст кода видит хаб; `label` и `user_agent` заявлены клиентом. `other_country` — страна запроса известна и отличается от страны текущего сеанса. Затем `approve` или `deny`.
4. Опрос, первым увидевший `approved`, в одной транзакции переводит строку в `delivered` и создаёт сеанс (`method: qr`, `approved_by` — подтвердивший сеанс, `authenticated_at` пуст, то есть сеанс несвеж). Ответ несёт `{state: "approved", session}` и cookie сеанса и устройства. Любой другой опрос того же кода видит `expired`. Сеанс выдаётся не позже минуты после одобрения и только пока подтвердивший сеанс жив; завершение подтвердившего уносит такой сеанс каскадом.

Состояния опроса: `pending`, `scanned`, `approved`, `denied`, `expired`. `expired` значит и «срок вышел», и «код не найден», и «сеанс уже выдан»: устройство начинает заново. Секрет кода и токен опроса хешируются с разными префиксами и не взаимозаменяемы.

Защита от перебора: корзины по IPv4, по /64 для IPv6 и одна для неизвестного адреса; пять неудач за 15 минут запирают корзину на минуту, каждая следующая удваивает срок до часа, 15 минут без неудач сбрасывают счёт. Больше 30 неудач за 10 минут запирают вход по паролю на 10 минут для всех без cookie известного устройства. Неудачи — неверный логин или пароль, неверный текущий пароль, неверный пароль повторного входа, неудачное завершение входа или повторного входа ключом доступа, неизвестный, использованный или истёкший токен восстановления. Проверка argon2 идёт за семафором на два разрешения; запрос, не получивший разрешения за 5 секунд, получает `429` без счёта неудачи.

Первый вход и вход без устройств — ссылка восстановления на хосте хаба:

```bash
sudo -u canterlot canterlot-hub auth recovery-link --config /etc/canterlot/hub.toml --push
```

Без `--push` команда печатает `https://<origin>/recover#<токен>`, с `--push` отправляет ссылку только в Gotify владельца и печатает `sent`. Ссылка живёт 10 минут (`--minutes`, до 60), открывается один раз, новая отменяет прежнюю. Прочие команды: `auth set-password` (дважды из TTY без эха или `--stdin`; `--revoke-sessions` завершает все сеансы), `auth sessions` (живые сеансы), `auth revoke-all` (завершить все).

## CSRF

Изменяющий запрос несёт `Origin` из `allowed_origins` и `X-Canterlot-Client: 1`, в том числе публичные пути входа; запрос с `Upgrade` (WSS) несёт разрешённый `Origin`. Заголовков CORS хаб не отправляет, `OPTIONS` получает `405`. Каждый изменяющий запрос вне `/api/v1/auth` несёт `Idempotency-Key`.

`GET /healthz` (живость) и `GET /readyz` (база отвечает, запись установки совпадает с манифестом) не требуют ничего. Лимит JSON-тела задаёт `max_json_body_bytes`. Строка журнала запросов содержит только метод, шаблон маршрута, статус, задержку и `correlation_id`; заголовки, query и тела в журнал не попадают.

## Caddy

Caddy раздаёт статику PWA без входа (оболочка должна загрузиться, чтобы показать страницу входа; данных в сборке нет) и проксирует `/api/*` вместе с WSS к хабу. Присланные клиентом `Remote-*`, `X-Forwarded-*`, `X-Canterlot-Proxy` и `X-Canterlot-Client-Ip` вычищаются навсегда, затем к запросу к хабу добавляются секрет и адрес клиента. Директивы внутри `route` выполняются в порядке записи.

```caddyfile
chat.example {
    route {
        request_header -Remote-*
        request_header -X-Forwarded-*
        request_header -X-Canterlot-Proxy
        request_header -X-Canterlot-Client-Ip

        @api path /api/* /healthz /readyz
        handle @api {
            reverse_proxy hub:8080 {
                header_up X-Canterlot-Proxy {file./run/secrets/canterlot-proxy}
                header_up X-Canterlot-Client-Ip {remote_host}
            }
        }
        handle {
            root * /var/www/canterlot/build
            try_files {path} /200.html
            file_server
        }
    }
}
```

`{remote_host}` — адрес непосредственного собеседника Caddy. Если перед Caddy стоит ещё один прокси или CDN, нужен `{client_ip}` вместе с `trusted_proxies` в самом Caddy. Адрес Caddy (или сеть контейнера) укажите в `[http].trusted_proxies`.

Заголовок личности старого прокси (`[http].user_header`, по умолчанию `Remote-User`) хаб больше не читает. Если такой заголовок дошёл до хаба от доверенного прокси, ответ несёт `X-Canterlot-Proxy-Leak: 1`, а журнал раз в 10 минут пишет `identity_header_forwarded`. Чистка `Remote-*` в Caddy остаётся навсегда: перед откатом хаба на версию без собственного входа (1.29 и раньше) в Caddy сначала возвращается блок входа, иначе старый хаб принял бы поддельный `Remote-User`.

Приёмка развёртывания, один раз и после каждого изменения Caddy: `scripts/check-proxy.sh` (без сеанса `GET /api/v1/me` даёт JSON `401 unauthenticated` хаба, поддельные `Remote-User`, `X-Canterlot-Proxy`, `X-Canterlot-Client-Ip` и `X-Forwarded-For` не помогают и не дают `X-Canterlot-Proxy-Leak`; положительная проверка входит по одноразовой ссылке восстановления во временный jar и выходит). Подробности — в [развёртывании](deploy/pilot.md).

## WSS

WSS-соединение живёт не дольше `[http.ws].max_connection_age`, после чего хаб закрывает его кодом 4401 с причиной `reauth`, а клиент переподключается и снова проходит проверку сеанса. Каждый сокет перепроверяет свой сеанс в базе по уведомлению о сеансах (шина `auth`, в нескольких процессах через `pg_notify`), при переподключении слушателя, при потере уведомлений и раз в `[http.ws].session_check_interval` (60 с); отозванный или истёкший сеанс закрывается кодом 4401 с кадром `{"op":"close","reason":"revoked"}`, обычно в пределах двух секунд.

WSS `GET /api/v1/ws` использует подпротокол `canterlot.v1`; кадры определены как `ClientOp` / `ServerOp` в `docs/api/events.schema.json`, правила восстановления описаны в `docs/design/api.md`. В `[http.ws]`: до 16 подписок, очередь 2–256 сообщений и 4096–1048576 байт, с резервом одного слота и 1024 байт для управления; `delta_window` не больше 25 мс. Merge учитывает полный JSON-кадр и бюджет данных; невместимое одиночное событие передаётся как `oversized` со ссылкой на HTTP-чтение. Catch-up ждёт ёмкости, `lagged` возникает только после `stall_timeout` без прогресса при заполненной очереди. Shutdown закрывает WSS кодом 1001 и дожидается pump.

Контракт API: `docs/api/openapi.json` (отдаётся как `GET /api/v1/openapi.json`) генерируется командой `canterlot-hub openapi`; `scripts/gen-openapi.sh` обновляет файл, а тест `openapi_is_served_and_committed_file_matches_generated` падает, пока файл устарел. Таблица кодов ошибок: `docs/design/api.md`.

Управление очередью доступно через `POST /api/v1/inputs/{id}/cancel`, `POST /api/v1/conversations/{id}/interrupt`, `POST /api/v1/conversations/{id}/stop` и `POST /api/v1/conversations/{id}/queue/continue`. Решение по запросу агента принимает `POST /api/v1/conversations/{id}/requests/{request_id}/decision`: первое сохранённое решение побеждает, формы тела и коды ошибок описаны в [контракте API](design/api.md#очередь-настройки-запуск-и-повтор).

`POST /api/v1/conversations/{id}/inputs` ставит текст и готовые `attachment_ids` в очередь; `retry_of` ссылается на неподтверждённый ввод той же сессии. `PATCH /api/v1/conversations/{id}/settings` сохраняет изменения для следующего ввода; они становятся эффективными после подтверждения воркера. `POST /api/v1/conversations/{id}/executions` запускает исполнение, а `POST /api/v1/conversations/{id}/resume` продолжает сессию с последним native ID и при запросе снимает паузу очереди в той же транзакции.

Чтения: `GET /api/v1/snapshot/conversations`, `GET /api/v1/snapshot/machines`, `GET /api/v1/conversations/{id}/snapshot`, `/history`, `/blocks/{block_id}`, `/runs/{run_id}`, `/delegation`. Снимок и его курсор читаются в одной `REPEATABLE READ` транзакции; `GET /api/v1/events?scope=&after=&limit=` восстанавливает хвост с сохранением номеров внутренних событий через `hidden`. При устаревшем или опережающем курсоре нужен новый снимок. Снимок разговора отдаёт в `open_requests` все незавершённые запросы (`open`, `decision_saved`, `decision_transmitting`); при переходе решения к передаче агенту хаб выпускает событие `request_transmitting`. Подробные поля и ограничения: `docs/design/api.md`; схема событий: `docs/api/events.schema.json`. Отдельных таблиц конфигурации у чтений нет: действуют `[http]` и пороги `[machines]`.
