# Образ полного прогона проверок Canterlot: scripts/check.sh --all и
# scripts/check-web.sh --stack в Debian 13 — той же платформе, что прод, эта VM
# и тестовая PostgreSQL. На сборочной машине (Arch) те же проверки падают на
# os-release, шрифтах и окружении, поэтому гейт идёт только в этом образе.
#
# База закреплена по digest (тот же, что у docker/release-build), Rust — по
# каналу из rust-toolchain.toml, Node — по версии и sha256 из nodejs.org/dist,
# PostgreSQL — той же мажорной версии, что тестовый сервер VM
# (scripts/test-db.sh поднимает postgres:18.6-trixie, Debian 18.6-1.pgdg13).
# Тег образа считает scripts/gate-remote.sh: hash этого файла, rust-toolchain.toml
# и версии Playwright из web/package-lock.json.
FROM debian:trixie@sha256:9c6e381421d0f48d1b8133e573f33c185ee16cbf505eda9c4a8a4791b25ea8a6

ARG DEBIAN_FRONTEND=noninteractive
# build-essential, protobuf-compiler и pkg-config — сборка Rust (prost-build
# зовёт protoc); cmake — transcribe.cpp для canterlot-speech (без CUDA);
# binutils — objdump, jq — разбор lock-файлов, strace — тесты долговечности
# каталогов; procps — ps/pgrep/pkill из scripts/reap-stray-tests.sh
# и e2e/stack/global-teardown.ts; openssl — самоподписанные сертификаты стенда;
# libnss-wrapper — passwd-запись для uid, который приходит снаружи (её требует
# initdb, а в контейнере пользователя нет).
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        binutils \
        build-essential \
        ca-certificates \
        cmake \
        curl \
        git \
        gnupg \
        jq \
        libnss-wrapper \
        locales \
        openssl \
        pkg-config \
        procps \
        protobuf-compiler \
        python3 \
        strace \
        tar \
        tzdata \
        util-linux \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

# Локаль en_US.UTF-8 — как у postgres:18.6-trixie (его ENV LANG): от неё зависит
# порядок строк в тестах лимитов и пресетов (кириллица сортируется не по байтам).
RUN localedef -i en_US -c -f UTF-8 -A /usr/share/locale/locale.alias en_US.UTF-8

# PostgreSQL 18: в trixie только 17, а тестовая база VM — 18.6 (PGDG). Нужны и
# сервер (in-container.sh поднимает свою базу на 127.0.0.1:55441), и клиент
# (psql/pg_isready зовут scripts/test-db.sh, e2e/stack/global-setup.ts и пробы).
RUN install -d /usr/share/postgresql-common/pgdg \
    && curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc \
        -o /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc \
    && printf 'deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] https://apt.postgresql.org/pub/repos/apt trixie-pgdg main\n' \
        >/etc/apt/sources.list.d/pgdg.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends postgresql-18 postgresql-client-18 \
    && rm -rf /var/lib/apt/lists/* \
    && /usr/lib/postgresql/18/bin/postgres --version

# Шрифты ровно те же, что на VM: тесты вёрстки (e2e/ui/desktop-layout, parity)
# сравнивают размеры текста, а не «похожесть».
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        fontconfig \
        fonts-dejavu-core \
        fonts-dejavu-mono \
        fonts-droid-fallback \
        fonts-freefont-otf \
        fonts-gfs-baskerville \
        fonts-gfs-porson \
        fonts-liberation \
        fonts-liberation-sans-narrow \
        fonts-lmodern \
        fonts-lyx \
        fonts-noto-color-emoji \
        fonts-noto-mono \
        fonts-opensymbol \
        fonts-urw-base35 \
    && fc-cache -f \
    && rm -rf /var/lib/apt/lists/*

# Rust ставится в /usr/local и принадлежит root: гейт идёт от uid хоста
# (--user), поэтому /usr/local/rustup и /usr/local/cargo только читаются.
# Прокси rustup кладутся в /usr/local/bin, чтобы CARGO_HOME на рантайме мог быть
# смонтированным кэшем и не прятал cargo/rustc.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    CARGO_TERM_COLOR=never
ARG RUST_VERSION=1.99.1
RUN export PATH="/usr/local/cargo/bin:${PATH}" \
    && curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path \
        --profile minimal --default-toolchain "${RUST_VERSION}" \
    && rustup component add --toolchain "${RUST_VERSION}" rustfmt clippy \
    && for bin in cargo rustc rustup rustdoc rustfmt cargo-clippy clippy-driver; do \
        ln -sf "/usr/local/cargo/bin/${bin}" "/usr/local/bin/${bin}"; \
    done \
    && chmod -R a+rX /usr/local/rustup /usr/local/cargo

# Node 24 из официального тарбола: версия и sha256 сверяются при сборке образа.
ARG NODE_VERSION=24.22.1
ARG NODE_SHA256=6c172a9ab6718f7b801fb5a7bd486af2ee5e32758086a04b84fcce2aa77095e6
RUN curl -fsSLO "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz" \
    && echo "${NODE_SHA256}  node-v${NODE_VERSION}-linux-x64.tar.xz" | sha256sum -c - \
    && mkdir -p /usr/local/node \
    && tar -xJf "node-v${NODE_VERSION}-linux-x64.tar.xz" -C /usr/local/node --strip-components=1 \
    && rm "node-v${NODE_VERSION}-linux-x64.tar.xz" \
    && for bin in node npm npx; do ln -sf "/usr/local/node/bin/${bin}" "/usr/local/bin/${bin}"; done

# cargo-nextest: scripts/check.sh ставит его через scripts/install-tools.sh, но в
# образе он закреплён по версии, чтобы прогон не тянул сеть и не менялся сам.
ARG NEXTEST_VERSION=0.9.150
RUN export PATH="/usr/local/cargo/bin:${PATH}" \
    && curl -fsSL "https://get.nexte.st/${NEXTEST_VERSION}/linux" \
        | tar zxf - -C /usr/local/cargo/bin \
    && test "$(cargo nextest --version | head -n1 | awk '{print $2}')" = "${NEXTEST_VERSION}" \
    && chmod -R a+rX /usr/local/cargo

# Playwright: браузеры пекутся в образ под версию из web/package-lock.json, а
# системные зависимости ставит install-deps. На рантайме браузеры лежат в
# смонтированном кэше /cache/playwright (in-container.sh копирует их туда, если
# кэш пуст), поэтому первый прогон не скачивает ничего.
ARG PLAYWRIGHT_VERSION=1.64.1
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
RUN npm install --global "playwright@${PLAYWRIGHT_VERSION}" \
    && ln -sf /usr/local/node/bin/playwright /usr/local/bin/playwright \
    && playwright install --with-deps chromium firefox \
    && npm cache clean --force \
    && chmod -R a+rX /opt/ms-playwright

# HOME произвольного uid: npm, git и тесты пишут сюда. ~/.cargo/env нужен
# scripts/check.sh (`source ~/.cargo/env`), ~/.gitconfig — тестам, которые
# коммитят (identity и доверие к рабочей копии на смонтированном каталоге).
RUN mkdir -p /home/canterlot/.cargo \
    && printf 'case ":$PATH:" in\n    *:/usr/local/cargo/bin:*) ;;\n    *) export PATH="/usr/local/cargo/bin:$PATH" ;;\nesac\n' \
        >/home/canterlot/.cargo/env \
    && printf '[user]\n\tname = e2e\n\temail = e2e@canterlot.invalid\n[safe]\n\tdirectory = *\n[init]\n\tdefaultBranch = main\n' \
        >/home/canterlot/.gitconfig \
    && chmod -R a+rwX /home/canterlot
ENV HOME=/home/canterlot \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    TZ=UTC \
    PATH=/usr/local/bin:/usr/local/sbin:/usr/local/cargo/bin:/usr/sbin:/usr/bin:/sbin:/bin

WORKDIR /src
