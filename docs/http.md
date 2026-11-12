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
- Без сеанса доступны только `GET /api/v1/auth/methods`, `POST /api/v1/auth/password`, `POST /api/v1/auth/recovery` (точный метод и путь), `GET /healthz` и `GET /readyz`. Всё остальное, включая WSS, медиа и вложения, требует сеанса.

Пути `/api/v1/auth`:

| Метод и путь | Сеанс | Что делает |
| --- | --- | --- |
| `GET /methods` | нет | `{password, passkey}`: задан ли пароль, есть ли ключи доступа |
| `POST /password` | нет | `{login, password, device?}` → сеанс и cookie; `401 invalid_credentials`; `429 too_many_attempts` с `details.retry_after_seconds` и `Retry-After` |
| `POST /recovery` | нет | `{token, device?}` → сеанс по ссылке восстановления |
| `GET /sessions` | да | `{current_id, idle_timeout_days, fresh_until, sessions}`: живые сеансы, текущий первым |
| `PATCH /sessions/{id}` | да | `{name}`: имя до 64 символов, пустое снимает |
| `DELETE /sessions/{id}` | да | свой id — выход; чужой требует свежего сеанса и уносит каскадом сеансы, вход которых он подтвердил |
| `POST /sessions/revoke-others` | свежий | завершить все остальные сеансы |
| `POST /logout` | да | завершить текущий сеанс, стереть cookie |
| `PATCH /settings` | да | `{idle_timeout_days}`: 7, 30, 90, 180 или 365 |
| `PUT /password` | да | `{current_password?, new_password}`: смена несёт текущий пароль, первый пароль требует свежего сеанса; успех завершает остальные сеансы |
| `POST /reauth/password` | да | `{password}` → сеанс свеж 10 минут; неверный пароль — `403 wrong_password` |

`device` — подсказки клиента для подписи устройства (объект до 2 КиБ, только для показа). Свежий сеанс — пароль, ключ доступа или ссылка восстановления за последние 10 минут; иначе действие управления доступом отвечает `403 reauth_required`. `GET /api/v1/me` возвращает `owner_id`, `login` и `session_id`.

Защита от перебора: корзины по IPv4, по /64 для IPv6 и одна для неизвестного адреса; пять неудач за 15 минут запирают корзину на минуту, каждая следующая удваивает срок до часа, 15 минут без неудач сбрасывают счёт. Больше 30 неудач за 10 минут запирают вход по паролю на 10 минут для всех без cookie известного устройства. Неудачи — неверный логин или пароль, неверный текущий пароль, неверный пароль повторного входа, неизвестный, использованный или истёкший токен восстановления. Проверка argon2 идёт за семафором на два разрешения; запрос, не получивший разрешения за 5 секунд, получает `429` без счёта неудачи.

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
