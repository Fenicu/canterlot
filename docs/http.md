# HTTP: вход, CSRF и Caddy

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

Управление очередью доступно через `POST /api/v1/inputs/{id}/cancel`, `POST /api/v1/conversations/{id}/interrupt`, `POST /api/v1/conversations/{id}/stop` и `POST /api/v1/conversations/{id}/queue/continue`. Решение по запросу агента принимает `POST /api/v1/conversations/{id}/requests/{request_id}/decision`: первое сохранённое решение побеждает, формы тела и коды ошибок описаны в [контракте API](design/api.md#очередь-настройки-запуск-и-повтор).

`POST /api/v1/conversations/{id}/inputs` ставит текст и готовые `attachment_ids` в очередь; `retry_of` ссылается на неподтверждённый ввод той же сессии. `PATCH /api/v1/conversations/{id}/settings` сохраняет изменения для следующего ввода; они становятся эффективными после подтверждения воркера. `POST /api/v1/conversations/{id}/executions` запускает исполнение, а `POST /api/v1/conversations/{id}/resume` продолжает сессию с последним native ID и при запросе снимает паузу очереди в той же транзакции.

Чтения: `GET /api/v1/snapshot/conversations`, `GET /api/v1/snapshot/machines`, `GET /api/v1/conversations/{id}/snapshot`, `/history`, `/blocks/{block_id}`, `/runs/{run_id}`, `/delegation`. Снимок и его курсор читаются в одной `REPEATABLE READ` транзакции; `GET /api/v1/events?scope=&after=&limit=` восстанавливает хвост с сохранением номеров внутренних событий через `hidden`. При устаревшем или опережающем курсоре нужен новый снимок. Снимок разговора отдаёт в `open_requests` все незавершённые запросы (`open`, `decision_saved`, `decision_transmitting`); при переходе решения к передаче агенту хаб выпускает событие `request_transmitting`. Подробные поля и ограничения: `docs/design/api.md`; схема событий: `docs/api/events.schema.json`. Отдельных таблиц конфигурации у чтений нет: действуют `[http]` и пороги `[machines]`.
