# Canterlot

<p align="center">

[![Forgejo](https://shields.fenicu.com/badge/Forgejo-Fenicu%2Fcanterlot-brightgreen?logo=forgejo)](https://git.fenicu.com/Fenicu/canterlot)
![версия](https://shields.fenicu.com/badge/%D0%B2%D0%B5%D1%80%D1%81%D0%B8%D1%8F-1.10.1-00f4ee)
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
- прерывать текущий ход, не закрывая сессию: контекст и процесс остаются, следующее сообщение продолжает работу;
- видеть заполнение контекста и сжимать его, в том числе между ходами;
- запускать делегированные сессии через MCP-инструменты: отдельная рабочая копия, вложенный список, приём результата;
- давать агенту будильники: по времени, по состоянию других сессий (в том числе одним сообщением за всю пачку и со сторожем по сроку), по сбросу лимита провайдера и по освобождению машины; в чате они видны плашками;
- включать автопинок делегата: хаб сам подталкивает исполнителя, бросившего ход без маркера `STATUS: DONE`, и будит оркестратора один раз, когда толчки кончились;
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

Текущая версия — 1.10.1. История изменений для человека — [CHANGES.rst](CHANGES.rst), правила бампа — [docs/versioning.md](docs/versioning.md).

## Лицензии

Canterlot распространяется по лицензии MIT, текст — [LICENSE](LICENSE).

Сторонние зависимости и их лицензии — [docs/licenses/THIRD_PARTY.md](docs/licenses/THIRD_PARTY.md).
