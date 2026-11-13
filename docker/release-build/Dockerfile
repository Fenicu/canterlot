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
# cmake собирает transcribe.cpp для canterlot-speech.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        binutils \
        build-essential \
        ca-certificates \
        cmake \
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

# CUDA 12.9.1 для canterlot-speech --features cuda: только то, что нужно для
# компиляции (nvcc, cudart с заглушкой libcuda.so, cublas, cccl). Репозиторий
# NVIDIA для debian12 не проходит проверку подписи в trixie, поэтому берутся
# архивы redist; пути и sha256 — из redistrib_12.9.1.json. CUDA 13 не
# поддерживает Volta (sm_70), поэтому именно 12.x.
ARG CUDA_REDIST=https://developer.download.nvidia.com/compute/cuda/redist
RUN set -eu; mkdir -p /usr/local/cuda-12.9 /tmp/cuda; cd /tmp/cuda; \
    for entry in \
        "cuda_nvcc/linux-x86_64/cuda_nvcc-linux-x86_64-12.9.86-archive.tar.xz 7a1a5b652e5ef85c82b721d10672fc9a2dbaab44e9bd3c65a69517bf53998c35" \
        "cuda_cudart/linux-x86_64/cuda_cudart-linux-x86_64-12.9.79-archive.tar.xz 1f6ad42d4f530b24bfa35894ccf6b7209d2354f59101fd62ec4a6192a184ce99" \
        "libcublas/linux-x86_64/libcublas-linux-x86_64-12.9.1.4-archive.tar.xz 546addc4a9d82b8f23aa9ba9274b6bc0429a63008a31c759884ac24880796057" \
        "cuda_cccl/linux-x86_64/cuda_cccl-linux-x86_64-12.9.27-archive.tar.xz 8b1a5095669e94f2f9afd7715533314d418179e9452be61e2fde4c82a3e542aa"; \
    do \
        set -- $entry; \
        curl -fsSL -o archive.tar.xz "${CUDA_REDIST}/$1"; \
        echo "$2  archive.tar.xz" | sha256sum -c -; \
        tar -xJf archive.tar.xz -C /usr/local/cuda-12.9 --strip-components=1; \
        rm archive.tar.xz; \
    done; \
    rm -f /usr/local/cuda-12.9/lib/libcublas_static.a /usr/local/cuda-12.9/lib/libcublasLt_static.a; \
    ln -s lib /usr/local/cuda-12.9/lib64; \
    /usr/local/cuda-12.9/bin/nvcc --version; \
    chmod -R a+rX /usr/local/cuda-12.9; rm -rf /tmp/cuda
ENV CUDA_PATH=/usr/local/cuda-12.9
# glibc 2.41 объявляет sinpi/cospi/sinpif/cospif с noexcept, а crt/math_functions.h
# из CUDA 12.9 — без него, и nvcc падает уже на проверке компилятора. Правка та же,
# что в пакетах CUDA дистрибутивов: добавить noexcept (true) к четырём объявлениям.
RUN header=/usr/local/cuda-12.9/include/crt/math_functions.h \
    && sed -i -E 's/^(extern __DEVICE_FUNCTIONS_DECL__ __device_builtin__ (double|float) +(sinpi|sinpif|cospi|cospif)\((double|float) x\));$/\1 noexcept (true);/' "$header" \
    && test "$(grep -cE '(sinpi|sinpif|cospi|cospif)\((double|float) x\) noexcept \(true\);' "$header")" = 4

# HOME произвольного uid: /home/canterlot открыт на запись, чтобы npm и git не
# падали на первом же обращении. Кэши (cargo, target, npm) монтируются снаружи.
RUN mkdir -p /home/canterlot && chmod 0777 /home/canterlot
ENV HOME=/home/canterlot \
    LANG=C.UTF-8 \
    PATH=/usr/local/bin:/usr/local/sbin:/usr/local/cargo/bin:/usr/sbin:/usr/bin:/sbin:/bin

WORKDIR /src
