# Сборочный образ выпуска Canterlot: Debian 13 (trixie), glibc 2.41 — та же
# платформа, что у прода (хаб LXC и воркер VM). Собирает только бинарники и PWA:
# исходники монтируются, в образе их нет.
#
# База закреплена по digest (regctl image digest debian:trixie), Rust — по каналу
# из rust-toolchain.toml, Node — по версии и sha256 из nodejs.org/dist.
# Тег образа считает scripts/build-release.sh: hash Dockerfile + rust-toolchain.toml.
FROM debian:trixie@sha256:9c6e381421d0f48d1b8133e573f33c185ee16cbf505eda9c4a8a4791b25ea8a6

ARG DEBIAN_FRONTEND=noninteractive
# protobuf-compiler нужен: crates/canterlot-proto/build.rs вызывает tonic-prost-build,
# а prost-build не приносит protoc с собой. binutils — objdump для проверки GLIBC.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        binutils \
        build-essential \
        ca-certificates \
        curl \
        git \
        jq \
        pkg-config \
        protobuf-compiler \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

# Rust ставится в /usr/local и принадлежит root: в контейнере сборка идёт от
# uid хоста (--user), поэтому /usr/local/rustup и /usr/local/cargo только читаются.
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

# HOME произвольного uid: /home/canterlot открыт на запись, чтобы npm и git не
# падали на первом же обращении. Кэши (cargo, target, npm) монтируются снаружи.
RUN mkdir -p /home/canterlot && chmod 0777 /home/canterlot
ENV HOME=/home/canterlot \
    LANG=C.UTF-8 \
    PATH=/usr/local/bin:/usr/local/sbin:/usr/local/cargo/bin:/usr/sbin:/usr/bin:/sbin:/bin

WORKDIR /src
