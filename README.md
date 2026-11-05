# Canterlot

<p align="center">

[![Forgejo](https://shields.fenicu.com/badge/Forgejo-Fenicu%2Fcanterlot-brightgreen?logo=forgejo)](https://git.fenicu.com/Fenicu/canterlot)
![версия](https://shields.fenicu.com/badge/%D0%B2%D0%B5%D1%80%D1%81%D0%B8%D1%8F-1.11.0-00f4ee)
[![история](https://shields.fenicu.com/badge/%D0%B8%D1%81%D1%82%D0%BE%D1%80%D0%B8%D1%8F-CHANGES.rst-6c757d)](CHANGES.rst)
[![лицензия](https://shields.fenicu.com/badge/%D0%BB%D0%B8%D1%86%D0%B5%D0%BD%D0%B7%D0%B8%D1%8F-MIT-blue)](LICENSE)
![агенты](https://shields.fenicu.com/badge/%D0%B0%D0%B3%D0%B5%D0%BD%D1%82%D1%8B-Claude%20Code%20%C2%B7%20Codex%20%C2%B7%20OpenCode%20%C2%B7%20Antigravity-1f5457)
![Rust](https://shields.fenicu.com/badge/rust-1.99.1-orange?logo=rust)
![SvelteKit](https://shields.fenicu.com/badge/SvelteKit-3-FF3E00?logo=svelte&logoColor=white)
![PostgreSQL](https://shields.fenicu.com/badge/PostgreSQL-18-4169E1?logo=postgresql&logoColor=white)
![Node](https://shields.fenicu.com/badge/node-24-339933?logo=node.js&logoColor=white)

</p>

Canterlot — self-hosted хаб для агентов программирования. Claude Code, Codex, OpenCode и Antigravity работают на машинах владельца, а сессии открываются с телефона через PWA: лента, разрешения и вопросы, очередь сообщений, лимиты и настройки. Antigravity работает на Google-аккаунте владельца и использует его каталог `~/.gemini`: настройки, MCP-серверы и история разговоров общие с Antigravity владельца.

Хаб написан на Rust и хранит состояние в PostgreSQL. На каждой рабочей машине запущен Rust-воркер, хаб и воркеры общаются по gRPC поверх TLS. Клиент — статическая сборка SvelteKit; её раздаёт обратный прокси, он же закрывает вход (Caddy и Authelia). Отдельного сервера приложений у клиента нет.

Что можно делать из интерфейса:

- смотреть ленту сессии: ответы, вызовы инструментов, субагентов и служебные строки;
- отвечать на разрешения и вопросы агента карточками; ответ проходит этапы «отправка → сохранено → передано → применено»;
- вести очередь сообщений: «Отправить сейчас» вливает сообщение в идущий ход, а ждущее сообщение можно изменить до отправки;
- убирать сессию в корзину на 24 часа, возвращать её одним нажатием или удалять окончательно;
- продолжать черновик с любого устройства: текст и готовые вложения хранятся на хабе, удаление и отправка сразу видны на телефоне и компьютере;
- прерывать текущий ход, не закрывая сессию: контекст и процесс остаются, следующее сообщение продолжает работу;
- видеть заполнение контекста и сжимать его, в том числе между ходами;
- запускать делегированные сессии через MCP-инструменты: отдельная рабочая копия, вложенный список, приём результата;
- давать агенту будильники: по времени, по состоянию других сессий (в том числе одним сообщением за всю пачку и со сторожем по сроку), по сбросу лимита провайдера и по освобождению машины; в чате они видны плашками;
- включать автопинок делегата: хаб сам подталкивает исполнителя, бросившего ход без маркера `STATUS: DONE`, и будит оркестратора один раз, когда толчки кончились; состояние, счётчики и последние сигналы видны в чате, владелец ставит правило на паузу или снимает его, а текст толчка редактируется во вкладке «Текст толчка» экрана «Инструкции»;
- следить за лимитами провайдеров по пятичасовому, недельному и месячному окнам и за скоростью ответа; настраивать пороги жёлтого и красного предупреждений и недельный резерв — изменения действуют сразу, без перезапуска хаба;
- видеть в сведениях о сессии расход токенов за всю сессию и всех её делегатов, по моделям и с кэшем;
- вставлять скриншоты и файлы из буфера или перетаскиванием; агент может отдать файл в ленту;
- видеть свободное место на машинах и тихое предупреждение в списке сессий, когда его меньше `[machines] disk_warn_free_gb = 30` ГБ (критический уровень — `disk_critical_free_gb = 10`);
- видеть версию в настройках, окно «Что нового» и историю изменений.

## Скриншоты

Телефон и рабочий стол: темы Аквамарин, Стандартная и VS Code.

<table align="center">
  <tr>
    <td align="center"><a href="https://git.fenicu.com/Fenicu/canterlot/src/branch/main/docs/screenshots/sessions-dark.png"><img src="docs/screenshots/sessions-dark.png" width="240" alt="Список сессий с лимитами и группами"></a><br><sub>Сессии, лимиты и группы</sub></td>
    <td align="center"><a href="https://git.fenicu.com/Fenicu/canterlot/src/branch/main/docs/screenshots/chat-dark.png"><img src="docs/screenshots/chat-dark.png" width="240" alt="Чат с кодом, инструментами и заполнением контекста"></a><br><sub>Чат, код и инструменты</sub></td>
    <td align="center"><a href="https://git.fenicu.com/Fenicu/canterlot/src/branch/main/docs/screenshots/permission-light.png"><img src="docs/screenshots/permission-light.png" width="240" alt="Карточка разрешения на команду"></a><br><sub>Разрешения агента</sub></td>
  </tr>
  <tr>
    <td align="center" valign="top"><a href="https://git.fenicu.com/Fenicu/canterlot/src/branch/main/docs/screenshots/question-dark.png"><img src="docs/screenshots/question-dark.png" width="240" alt="Карточка вопроса с вариантами ответа"></a><br><sub>Вопросы с вариантами</sub></td>
    <td align="center" valign="top" colspan="2"><a href="https://git.fenicu.com/Fenicu/canterlot/src/branch/main/docs/screenshots/limits-light.png"><img src="docs/screenshots/limits-light.png" width="200" alt="История лимитов и скорость моделей"></a><br><sub>История лимитов и скорость</sub></td>
  </tr>
  <tr>
    <td align="center" colspan="3"><a href="https://git.fenicu.com/Fenicu/canterlot/src/branch/main/docs/screenshots/desktop.png"><img src="docs/screenshots/desktop.png" width="760" alt="Рабочий стол: список сессий и чат рядом"></a><br><sub>Рабочий стол: список и чат рядом</sub></td>
  </tr>
</table>

## Развёртывание из готовых архивов

В [выпусках на Forgejo](https://git.fenicu.com/Fenicu/canterlot/releases) лежат собранные хаб, воркер и PWA. Хаб и воркер собраны для x86_64 под glibc 2.41 и новее (Debian 13, Ubuntu 25.04 и новее). Подробности каждого шага — в [docs/deploy/pilot.md](docs/deploy/pilot.md), ключи конфигурации — в [docs/configuration.md](docs/configuration.md). Собрать всё из исходников можно по [docs/development.md](docs/development.md).

Понадобятся:

- PostgreSQL 18 с базой в UTF-8;
- Caddy и SSO с forward-auth, который передаёт логин в заголовке `Remote-User` (например, Authelia);
- на рабочих машинах — systemd и CLI агентов, которыми будете пользоваться: `claude`, `codex`, `opencode`, `agy`.

**1. Скачать и проверить.**

```bash
VER=$(curl -fsS https://git.fenicu.com/api/v1/repos/Fenicu/canterlot/releases/latest | jq -r .tag_name | sed 's/^v//')
base=https://git.fenicu.com/Fenicu/canterlot/releases/download/v$VER
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
- `/etc/canterlot/hub.toml` с таблицей `[http]`: `trusted_proxies` — адрес Caddy, `owner_login` — ваш логин в SSO, `allowed_origins` — адрес PWA (§2.4);
- секрет прокси, общий для хаба и Caddy (§2.5);
- сертификат gRPC от своего УЦ, имя хоста хаба в SAN (§3).

Затем создайте каталоги из §2.6, один раз инициализируйте хаб и включите службу из §2.7:

```bash
sudo -u canterlot canterlot-hub init --config /etc/canterlot/hub.toml
sudo systemctl enable --now canterlot-hub
```

`init` применяет миграции и создаёт владельца. При обновлении хаб сам применяет новые миграции на старте.

**3. PWA и Caddy.** Разверните статику и подставьте свой адрес PWA в CSP:

```bash
sudo mkdir -p /var/www/canterlot/build
sudo tar -xzf canterlot-web-$VER.tar.gz -C /var/www/canterlot/build
sed 's/chat\.example\.com/chat.your.domain/' canterlot-csp-$VER.txt
```

Строку передайте Caddy в переменной окружения `CANTERLOT_CSP`. Блок сайта возьмите из §4.2: порядок директив внутри `route` важен, он не даёт подделать заголовки входа. Хэши скриптов в CSP привязаны к версии, поэтому при обновлении PWA строку CSP тоже обновляйте.

**4. Машина с воркером.** На хабе выпустите параметры машины:

```bash
sudo -u canterlot canterlot-hub machine add --config /etc/canterlot/hub.toml --name workstation
```

Команда печатает `machine_id`, `installation_id`, `restore_generation` и `token`. Токен показывается один раз: сохраните его на машине в `~/.config/canterlot/token` с правами `0600`, туда же положите `ca.pem` своего УЦ. Затем на машине воркера:

```bash
sudo tar -xzf canterlot-worker-$VER-x86_64-linux-gnu.tar.gz -C /usr/local/bin
```

`~/.config/canterlot/worker.toml` заполните по §6.2 и [docs/configuration.md](docs/configuration.md): адрес хаба `https://<хаб>:7443`, три UUID, `allowed_workdir_roots` и таблицы нужных агентов. Пользовательская служба — из §6.4, в ней обязателен `KillMode=process`: с ним идущие сессии переживают перезапуск воркера.

```bash
loginctl enable-linger "$USER"
systemctl --user enable --now canterlot-worker
```

**Обновление.** Скачайте новую версию тем же способом. Сначала замените бинарник хаба и перезапустите его, затем PWA с новым CSP, затем бинарник воркера: `systemctl --user restart canterlot-worker`. Идущие сессии при этом не прерываются. Откатить хаб на версию со старой схемой базы нельзя, поэтому перед обновлением сделайте дамп.

## Документация

- [Разработка](docs/development.md) — тулчейн, структура репозитория, проверки, тесты и обновление контракта.
- [Конфигурация](docs/configuration.md) — `hub.toml` и `worker.toml`, ключи, запуск, отзыв и ротация машин.
- [HTTP и вход](docs/http.md) — заголовки прокси, CSRF, Caddy и WSS.
- [PWA](docs/pwa.md) — модули клиента, статическая публикация (CSP и `wss://<origin-host>`), установка и обновление.
- [Развёртывание](docs/deploy/pilot.md) — пилотный контур: Proxmox LXC, PostgreSQL, Caddy, Authelia.
- [Контракт API](docs/design/api.md) — маршруты, ошибки и события; машинные схемы в `docs/api/`.
- [Проектирование](docs/design/README.md) — автоматы состояний, порядок блокировок, идемпотентность и восстановление.
- [Версионирование](docs/versioning.md) — правила бампа и проверка версии.

## Версии

Текущая версия — 1.11.0. История изменений для человека — [CHANGES.rst](CHANGES.rst), правила бампа — [docs/versioning.md](docs/versioning.md).

Архивы выпуска публикует `scripts/publish-release.sh vX.Y.Z` из сборки, сделанной при выкатке, без повторной компиляции.

## Лицензии

Canterlot распространяется по лицензии MIT, текст — [LICENSE](LICENSE).

Сторонние зависимости и их лицензии — [docs/licenses/THIRD_PARTY.md](docs/licenses/THIRD_PARTY.md).
