# Лицензии сторонних зависимостей

Данный реестр содержит лицензии сторонних компонентов (Rust-крейтов и npm-пакетов), используемых в Canterlot.
Файл сгенерирован автоматически скриптом `scripts/gen-licenses.sh`.

## Сводка по лицензиям

| Лицензия | Rust | npm (runtime) | npm (dev) | Всего |
| --- | ---: | ---: | ---: | ---: |
| `(MIT OR Apache-2.0) AND Unicode-3.0` | 2 | 0 | 0 | 2 |
| `(MIT OR CC0-1.0)` | 0 | 0 | 1 | 1 |
| `(MPL-2.0 OR Apache-2.0)` | 0 | 1 | 0 | 1 |
| `Apache-2.0` | 7 | 4 | 20 | 31 |
| `Apache-2.0 / MIT` | 1 | 0 | 0 | 1 |
| `Apache-2.0 AND ISC` | 1 | 0 | 0 | 1 |
| `Apache-2.0 OR BSL-1.0` | 1 | 0 | 0 | 1 |
| `Apache-2.0 OR BSL-1.0 OR MIT` | 1 | 0 | 0 | 1 |
| `Apache-2.0 OR ISC OR MIT` | 3 | 0 | 0 | 3 |
| `Apache-2.0 OR MIT` | 22 | 0 | 0 | 22 |
| `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | 5 | 0 | 0 | 5 |
| `Apache-2.0/MIT` | 3 | 0 | 0 | 3 |
| `BSD-2-Clause` | 0 | 1 | 9 | 10 |
| `BSD-2-Clause OR Apache-2.0 OR MIT` | 2 | 0 | 0 | 2 |
| `BSD-3-Clause` | 1 | 2 | 3 | 6 |
| `BlueOak-1.0.0` | 0 | 0 | 2 | 2 |
| `CC0-1.0` | 0 | 0 | 1 | 1 |
| `ISC` | 2 | 1 | 10 | 13 |
| `MIT` | 63 | 68 | 159 | 290 |
| `MIT AND BSD-3-Clause` | 1 | 0 | 0 | 1 |
| `MIT OR Apache-2.0` | 208 | 0 | 0 | 208 |
| `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | 2 | 0 | 0 | 2 |
| `MIT OR Apache-2.0 OR Zlib` | 1 | 0 | 0 | 1 |
| `MIT-0` | 1 | 0 | 2 | 3 |
| `MIT/Apache-2.0` | 9 | 0 | 0 | 9 |
| `MPL-2.0` | 0 | 12 | 2 | 14 |
| `PSF-2.0` | 0 | 1 | 0 | 1 |
| `Python-2.0` | 0 | 0 | 2 | 2 |
| `Unicode-3.0` | 18 | 0 | 0 | 18 |
| `Unlicense OR MIT` | 3 | 0 | 0 | 3 |
| `Zlib` | 2 | 0 | 0 | 2 |
| `Zlib OR Apache-2.0 OR MIT` | 1 | 0 | 0 | 1 |
| **ИТОГО** | **360** | **90** | **211** | **661** |

## Требуют внимания

В данном разделе перечислены зависимости, у которых лицензия не указана либо не может быть удовлетворена ни одной лицензией из разрешённого списка (allowlist):
`MIT`, `Apache-2.0`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Zlib`, `Unicode-3.0`, `Unicode-DFS-2016`, `MPL-2.0`, `CC0-1.0`, `0BSD`, `BSL-1.0`, `Unlicense`, `MIT-0` (включая оговорку `WITH LLVM-exception` для `Apache-2.0`).
Для выражений `A OR B` достаточно удовлетворения одной из альтернатив; для `A AND B` обязательно удовлетворение обеих сторон.

### Rust-крейты

Среди Rust-крейтов рабочего пространства пакеты с неразрешёнными лицензиями отсутствуют (все 341 зависимость удовлетворяют allowlist).

### npm (runtime)

| Пакет | Версия | Лицензия | Причина / примечание |
| --- | --- | --- | --- |
| `argparse` | 3.0.4 | `PSF-2.0` | Лицензия вне allowlist: PSF-2.0 |

### npm (dev-only, не поставляются)

Пакеты используются исключительно для сборки, линтинга и тестирования; не поставляются пользователю.

| Пакет | Версия | Лицензия | Причина / примечание |
| --- | --- | --- | --- |
| `argparse` | 2.0.1 | `Python-2.0` | Лицензия вне allowlist: Python-2.0 |
| `lru-cache` | 11.5.3 | `BlueOak-1.0.0` | Лицензия вне allowlist: BlueOak-1.0.0 |
| `minimatch` | 10.2.6 | `BlueOak-1.0.0` | Лицензия вне allowlist: BlueOak-1.0.0 |

## Крейты Rust

Зависимости рабочего пространства Cargo (исключая собственные крейты `canterlot-*` и прототипы `spikes`). Всего: 360.

| Крейт | Версия | Лицензия | Выбрано | Репозиторий |
| --- | --- | --- | --- | --- |
| `ahash` | 0.8.12 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/tkaitchuck/ahash](https://github.com/tkaitchuck/ahash) |
| `aho-corasick` | 1.1.5 | `Unlicense OR MIT` | `MIT` | [https://github.com/BurntSushi/aho-corasick](https://github.com/BurntSushi/aho-corasick) |
| `allocator-api2` | 0.2.21 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/zakarumych/allocator-api2](https://github.com/zakarumych/allocator-api2) |
| `android_system_properties` | 0.1.6 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/nical/android_system_properties](https://github.com/nical/android_system_properties) |
| `anstream` | 1.0.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-cli/anstyle](https://github.com/rust-cli/anstyle) |
| `anstyle` | 1.0.14 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-cli/anstyle](https://github.com/rust-cli/anstyle) |
| `anstyle-parse` | 1.0.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-cli/anstyle](https://github.com/rust-cli/anstyle) |
| `anstyle-query` | 1.1.5 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-cli/anstyle](https://github.com/rust-cli/anstyle) |
| `anstyle-wincon` | 3.0.11 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-cli/anstyle](https://github.com/rust-cli/anstyle) |
| `anyhow` | 1.0.104 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/anyhow](https://github.com/dtolnay/anyhow) |
| `asn1-rs` | 0.7.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rusticata/asn1-rs](https://github.com/rusticata/asn1-rs) |
| `asn1-rs-derive` | 0.6.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rusticata/asn1-rs](https://github.com/rusticata/asn1-rs) |
| `asn1-rs-impl` | 0.2.0 | `MIT/Apache-2.0` | `MIT` | [https://github.com/rusticata/asn1-rs](https://github.com/rusticata/asn1-rs) |
| `async-stream` | 0.3.6 | `MIT` | `MIT` | [https://github.com/tokio-rs/async-stream](https://github.com/tokio-rs/async-stream) |
| `async-stream-impl` | 0.3.6 | `MIT` | `MIT` | [https://github.com/tokio-rs/async-stream](https://github.com/tokio-rs/async-stream) |
| `async-trait` | 0.1.92 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/async-trait](https://github.com/dtolnay/async-trait) |
| `atoi` | 2.0.0 | `MIT` | `MIT` | [https://github.com/pacman82/atoi-rs](https://github.com/pacman82/atoi-rs) |
| `atomic-waker` | 1.1.2 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/smol-rs/atomic-waker](https://github.com/smol-rs/atomic-waker) |
| `autocfg` | 1.5.1 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/cuviper/autocfg](https://github.com/cuviper/autocfg) |
| `axum` | 0.8.9 | `MIT` | `MIT` | [https://github.com/tokio-rs/axum](https://github.com/tokio-rs/axum) |
| `axum-core` | 0.5.6 | `MIT` | `MIT` | [https://github.com/tokio-rs/axum](https://github.com/tokio-rs/axum) |
| `base64` | 0.22.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/marshallpierce/rust-base64](https://github.com/marshallpierce/rust-base64) |
| `base64` | 0.23.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/marshallpierce/rust-base64](https://github.com/marshallpierce/rust-base64) |
| `bit-set` | 0.8.0 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/contain-rs/bit-set](https://github.com/contain-rs/bit-set) |
| `bit-vec` | 0.8.0 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/contain-rs/bit-vec](https://github.com/contain-rs/bit-vec) |
| `bit-vec` | 0.9.1 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/contain-rs/bit-vec](https://github.com/contain-rs/bit-vec) |
| `bitflags` | 2.13.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/bitflags/bitflags](https://github.com/bitflags/bitflags) |
| `block-buffer` | 0.10.4 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/utils](https://github.com/RustCrypto/utils) |
| `block-buffer` | 0.12.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/utils](https://github.com/RustCrypto/utils) |
| `borrow-or-share` | 0.2.4 | `MIT-0` | `MIT-0` | [https://github.com/yescallop/borrow-or-share](https://github.com/yescallop/borrow-or-share) |
| `bumpalo` | 3.20.3 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/fitzgen/bumpalo](https://github.com/fitzgen/bumpalo) |
| `bytecount` | 0.6.9 | `Apache-2.0/MIT` | `MIT` | [https://github.com/llogiq/bytecount](https://github.com/llogiq/bytecount) |
| `byteorder` | 1.5.0 | `Unlicense OR MIT` | `MIT` | [https://github.com/BurntSushi/byteorder](https://github.com/BurntSushi/byteorder) |
| `bytes` | 1.12.1 | `MIT` | `MIT` | [https://github.com/tokio-rs/bytes](https://github.com/tokio-rs/bytes) |
| `cc` | 1.10.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/cc-rs](https://github.com/rust-lang/cc-rs) |
| `cfg-if` | 1.0.5 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/cfg-if](https://github.com/rust-lang/cfg-if) |
| `cfg_aliases` | 0.2.2 | `MIT` | `MIT` | [https://github.com/katharostech/cfg_aliases](https://github.com/katharostech/cfg_aliases) |
| `chacha20` | 0.10.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/stream-ciphers](https://github.com/RustCrypto/stream-ciphers) |
| `chrono` | 0.4.45 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/chronotope/chrono](https://github.com/chronotope/chrono) |
| `clap` | 4.6.7 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/clap-rs/clap](https://github.com/clap-rs/clap) |
| `clap_builder` | 4.6.7 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/clap-rs/clap](https://github.com/clap-rs/clap) |
| `clap_derive` | 4.6.7 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/clap-rs/clap](https://github.com/clap-rs/clap) |
| `clap_lex` | 1.1.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/clap-rs/clap](https://github.com/clap-rs/clap) |
| `cmov` | 0.5.4 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/RustCrypto/utils](https://github.com/RustCrypto/utils) |
| `colorchoice` | 1.0.5 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-cli/anstyle](https://github.com/rust-cli/anstyle) |
| `const-oid` | 0.10.2 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/RustCrypto/formats](https://github.com/RustCrypto/formats) |
| `core-foundation` | 0.10.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/servo/core-foundation-rs](https://github.com/servo/core-foundation-rs) |
| `core-foundation-sys` | 0.8.7 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/servo/core-foundation-rs](https://github.com/servo/core-foundation-rs) |
| `cpufeatures` | 0.2.17 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/utils](https://github.com/RustCrypto/utils) |
| `cpufeatures` | 0.3.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/utils](https://github.com/RustCrypto/utils) |
| `crc` | 3.4.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/mrhooray/crc-rs](https://github.com/mrhooray/crc-rs) |
| `crc-catalog` | 2.5.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/akhilles/crc-catalog](https://github.com/akhilles/crc-catalog) |
| `crc32c` | 0.6.8 | `Apache-2.0/MIT` | `MIT` | [https://github.com/zowens/crc32c](https://github.com/zowens/crc32c) |
| `crossbeam-queue` | 0.3.14 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/crossbeam-rs/crossbeam](https://github.com/crossbeam-rs/crossbeam) |
| `crossbeam-utils` | 0.8.23 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/crossbeam-rs/crossbeam](https://github.com/crossbeam-rs/crossbeam) |
| `crypto-common` | 0.1.6 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/traits](https://github.com/RustCrypto/traits) |
| `crypto-common` | 0.2.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/traits](https://github.com/RustCrypto/traits) |
| `ctutils` | 0.4.5 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/RustCrypto/utils](https://github.com/RustCrypto/utils) |
| `data-encoding` | 2.11.1 | `MIT` | `MIT` | [https://github.com/ia0/data-encoding](https://github.com/ia0/data-encoding) |
| `der-parser` | 10.0.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rusticata/der-parser](https://github.com/rusticata/der-parser) |
| `deranged` | 0.5.9 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/jhpratt/deranged](https://github.com/jhpratt/deranged) |
| `digest` | 0.10.7 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/traits](https://github.com/RustCrypto/traits) |
| `digest` | 0.11.3 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/traits](https://github.com/RustCrypto/traits) |
| `displaydoc` | 0.2.7 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/yaahc/displaydoc](https://github.com/yaahc/displaydoc) |
| `dotenvy` | 0.15.7 | `MIT` | `MIT` | [https://github.com/allan2/dotenvy](https://github.com/allan2/dotenvy) |
| `either` | 1.19.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rayon-rs/either](https://github.com/rayon-rs/either) |
| `email_address` | 0.2.9 | `MIT` | `MIT` | [https://github.com/johnstonskj/rust-email_address](https://github.com/johnstonskj/rust-email_address) |
| `equivalent` | 1.0.3 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/indexmap-rs/equivalent](https://github.com/indexmap-rs/equivalent) |
| `errno` | 0.3.14 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/lambda-fairy/rust-errno](https://github.com/lambda-fairy/rust-errno) |
| `etcetera` | 0.11.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/lunacookies/etcetera](https://github.com/lunacookies/etcetera) |
| `event-listener` | 5.4.2 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/smol-rs/event-listener](https://github.com/smol-rs/event-listener) |
| `fancy-regex` | 0.19.2 | `MIT` | `MIT` | [https://github.com/fancy-regex/fancy-regex](https://github.com/fancy-regex/fancy-regex) |
| `fastrand` | 2.5.0 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/smol-rs/fastrand](https://github.com/smol-rs/fastrand) |
| `find-msvc-tools` | 0.1.15 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/cc-rs](https://github.com/rust-lang/cc-rs) |
| `fixedbitset` | 0.5.7 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/petgraph/fixedbitset](https://github.com/petgraph/fixedbitset) |
| `fluent-uri` | 0.4.1 | `MIT` | `MIT` | [https://github.com/yescallop/fluent-uri-rs](https://github.com/yescallop/fluent-uri-rs) |
| `flume` | 0.12.0 | `Apache-2.0/MIT` | `MIT` | [https://github.com/zesterer/flume](https://github.com/zesterer/flume) |
| `fnv` | 1.0.7 | `Apache-2.0 / MIT` | `MIT` | [https://github.com/servo/rust-fnv](https://github.com/servo/rust-fnv) |
| `foldhash` | 0.1.5 | `Zlib` | `Zlib` | [https://github.com/orlp/foldhash](https://github.com/orlp/foldhash) |
| `foldhash` | 0.2.0 | `Zlib` | `Zlib` | [https://github.com/orlp/foldhash](https://github.com/orlp/foldhash) |
| `form_urlencoded` | 1.2.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/servo/rust-url](https://github.com/servo/rust-url) |
| `fraction` | 0.17.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dnsl48/fraction](https://github.com/dnsl48/fraction) |
| `futures-channel` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/futures-rs](https://github.com/rust-lang/futures-rs) |
| `futures-core` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/futures-rs](https://github.com/rust-lang/futures-rs) |
| `futures-executor` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/futures-rs](https://github.com/rust-lang/futures-rs) |
| `futures-intrusive` | 0.5.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/Matthias247/futures-intrusive](https://github.com/Matthias247/futures-intrusive) |
| `futures-io` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/futures-rs](https://github.com/rust-lang/futures-rs) |
| `futures-macro` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/futures-rs](https://github.com/rust-lang/futures-rs) |
| `futures-sink` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/futures-rs](https://github.com/rust-lang/futures-rs) |
| `futures-task` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/futures-rs](https://github.com/rust-lang/futures-rs) |
| `futures-util` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/futures-rs](https://github.com/rust-lang/futures-rs) |
| `generic-array` | 0.14.9 | `MIT` | `MIT` | [https://github.com/fizyk20/generic-array](https://github.com/fizyk20/generic-array) |
| `getrandom` | 0.2.17 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-random/getrandom](https://github.com/rust-random/getrandom) |
| `getrandom` | 0.3.4 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-random/getrandom](https://github.com/rust-random/getrandom) |
| `getrandom` | 0.4.3 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-random/getrandom](https://github.com/rust-random/getrandom) |
| `h2` | 0.4.20 | `MIT` | `MIT` | [https://github.com/hyperium/h2](https://github.com/hyperium/h2) |
| `hashbrown` | 0.15.5 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/hashbrown](https://github.com/rust-lang/hashbrown) |
| `hashbrown` | 0.16.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/hashbrown](https://github.com/rust-lang/hashbrown) |
| `hashbrown` | 0.17.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/hashbrown](https://github.com/rust-lang/hashbrown) |
| `hashlink` | 0.11.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/djc/hashlink](https://github.com/djc/hashlink) |
| `heck` | 0.5.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/withoutboats/heck](https://github.com/withoutboats/heck) |
| `hex` | 0.4.3 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/KokaKiwi/rust-hex](https://github.com/KokaKiwi/rust-hex) |
| `hkdf` | 0.13.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/KDFs/](https://github.com/RustCrypto/KDFs/) |
| `hmac` | 0.13.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/MACs](https://github.com/RustCrypto/MACs) |
| `http` | 1.5.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/hyperium/http](https://github.com/hyperium/http) |
| `http-body` | 1.1.0 | `MIT` | `MIT` | [https://github.com/hyperium/http-body](https://github.com/hyperium/http-body) |
| `http-body-util` | 0.1.5 | `MIT` | `MIT` | [https://github.com/hyperium/http-body](https://github.com/hyperium/http-body) |
| `httparse` | 1.10.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/seanmonstar/httparse](https://github.com/seanmonstar/httparse) |
| `httpdate` | 1.0.3 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/pyfisch/httpdate](https://github.com/pyfisch/httpdate) |
| `humantime` | 2.4.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/chronotope/humantime](https://github.com/chronotope/humantime) |
| `humantime-serde` | 1.1.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/jean-airoldie/humantime-serde](https://github.com/jean-airoldie/humantime-serde) |
| `hybrid-array` | 0.4.15 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/hybrid-array](https://github.com/RustCrypto/hybrid-array) |
| `hyper` | 1.12.0 | `MIT` | `MIT` | [https://github.com/hyperium/hyper](https://github.com/hyperium/hyper) |
| `hyper-rustls` | 0.27.10 | `Apache-2.0 OR ISC OR MIT` | `MIT` | [https://github.com/rustls/hyper-rustls](https://github.com/rustls/hyper-rustls) |
| `hyper-timeout` | 0.5.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/hjr3/hyper-timeout](https://github.com/hjr3/hyper-timeout) |
| `hyper-util` | 0.1.21 | `MIT` | `MIT` | [https://github.com/hyperium/hyper-util](https://github.com/hyperium/hyper-util) |
| `iana-time-zone` | 0.1.65 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/strawlab/iana-time-zone](https://github.com/strawlab/iana-time-zone) |
| `iana-time-zone-haiku` | 0.1.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/strawlab/iana-time-zone](https://github.com/strawlab/iana-time-zone) |
| `icu_collections` | 2.4.0 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `icu_locale_core` | 2.4.0 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `icu_normalizer` | 2.4.0 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `icu_normalizer_data` | 2.4.0 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `icu_properties` | 2.4.0 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `icu_properties_data` | 2.4.0 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `icu_provider` | 2.4.0 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `idna` | 1.1.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/servo/rust-url/](https://github.com/servo/rust-url/) |
| `idna_adapter` | 1.2.2 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/hsivonen/idna_adapter](https://github.com/hsivonen/idna_adapter) |
| `indexmap` | 2.14.2 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/indexmap-rs/indexmap](https://github.com/indexmap-rs/indexmap) |
| `ipnet` | 2.12.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/krisprice/ipnet](https://github.com/krisprice/ipnet) |
| `is_terminal_polyfill` | 1.70.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/polyfill-rs/is_terminal_polyfill](https://github.com/polyfill-rs/is_terminal_polyfill) |
| `itertools` | 0.14.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-itertools/itertools](https://github.com/rust-itertools/itertools) |
| `itoa` | 1.0.18 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/itoa](https://github.com/dtolnay/itoa) |
| `js-sys` | 0.3.106 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/js-sys](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/js-sys) |
| `jsonschema` | 0.60.2 | `MIT` | `MIT` | [https://github.com/Stranger6667/jsonschema](https://github.com/Stranger6667/jsonschema) |
| `jsonschema-regex` | 0.60.2 | `MIT` | `MIT` | [https://github.com/Stranger6667/jsonschema](https://github.com/Stranger6667/jsonschema) |
| `jsonschema-value` | 0.60.2 | `MIT` | `MIT` | [https://github.com/Stranger6667/jsonschema](https://github.com/Stranger6667/jsonschema) |
| `lazy_static` | 1.5.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang-nursery/lazy-static.rs](https://github.com/rust-lang-nursery/lazy-static.rs) |
| `libc` | 0.2.191 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/libc](https://github.com/rust-lang/libc) |
| `libsqlite3-sys` | 0.37.0 | `MIT` | `MIT` | [https://github.com/rusqlite/rusqlite](https://github.com/rusqlite/rusqlite) |
| `linux-raw-sys` | 0.12.1 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | `MIT` | [https://github.com/sunfishcode/linux-raw-sys](https://github.com/sunfishcode/linux-raw-sys) |
| `litemap` | 0.8.3 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `lock_api` | 0.4.14 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/Amanieu/parking_lot](https://github.com/Amanieu/parking_lot) |
| `log` | 0.4.34 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/log](https://github.com/rust-lang/log) |
| `lru-slab` | 0.1.3 | `MIT OR Apache-2.0 OR Zlib` | `MIT` | [https://github.com/Ralith/lru-slab](https://github.com/Ralith/lru-slab) |
| `matchit` | 0.8.4 | `MIT AND BSD-3-Clause` | `MIT AND BSD-3-Clause` | [https://github.com/ibraheemdev/matchit](https://github.com/ibraheemdev/matchit) |
| `md-5` | 0.11.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/hashes](https://github.com/RustCrypto/hashes) |
| `memchr` | 2.8.3 | `Unlicense OR MIT` | `MIT` | [https://github.com/BurntSushi/memchr](https://github.com/BurntSushi/memchr) |
| `micromap` | 0.3.0 | `MIT` | `MIT` | [https://github.com/yegor256/micromap](https://github.com/yegor256/micromap) |
| `mime` | 0.3.17 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/hyperium/mime](https://github.com/hyperium/mime) |
| `minimal-lexical` | 0.2.1 | `MIT/Apache-2.0` | `MIT` | [https://github.com/Alexhuszagh/minimal-lexical](https://github.com/Alexhuszagh/minimal-lexical) |
| `mio` | 1.2.4 | `MIT` | `MIT` | [https://github.com/tokio-rs/mio](https://github.com/tokio-rs/mio) |
| `multimap` | 0.10.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/havarnov/multimap](https://github.com/havarnov/multimap) |
| `nom` | 7.1.3 | `MIT` | `MIT` | [https://github.com/Geal/nom](https://github.com/Geal/nom) |
| `num` | 0.4.3 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-num/num](https://github.com/rust-num/num) |
| `num-bigint` | 0.4.8 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-num/num-bigint](https://github.com/rust-num/num-bigint) |
| `num-cmp` | 0.1.0 | `MIT/Apache-2.0` | `MIT` | [https://github.com/lifthrasiir/num-cmp](https://github.com/lifthrasiir/num-cmp) |
| `num-complex` | 0.4.6 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-num/num-complex](https://github.com/rust-num/num-complex) |
| `num-conv` | 0.2.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/jhpratt/num-conv](https://github.com/jhpratt/num-conv) |
| `num-integer` | 0.1.47 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-num/num-integer](https://github.com/rust-num/num-integer) |
| `num-iter` | 0.1.46 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-num/num-iter](https://github.com/rust-num/num-iter) |
| `num-rational` | 0.4.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-num/num-rational](https://github.com/rust-num/num-rational) |
| `num-traits` | 0.2.19 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-num/num-traits](https://github.com/rust-num/num-traits) |
| `oid-registry` | 0.8.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rusticata/oid-registry](https://github.com/rusticata/oid-registry) |
| `once_cell` | 1.21.4 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/matklad/once_cell](https://github.com/matklad/once_cell) |
| `once_cell_polyfill` | 1.70.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/polyfill-rs/once_cell_polyfill](https://github.com/polyfill-rs/once_cell_polyfill) |
| `openssl-probe` | 0.2.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rustls/openssl-probe](https://github.com/rustls/openssl-probe) |
| `outref` | 0.5.2 | `MIT` | `MIT` | [https://github.com/Nugine/outref](https://github.com/Nugine/outref) |
| `parking` | 2.2.1 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/smol-rs/parking](https://github.com/smol-rs/parking) |
| `parking_lot` | 0.12.5 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/Amanieu/parking_lot](https://github.com/Amanieu/parking_lot) |
| `parking_lot_core` | 0.9.12 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/Amanieu/parking_lot](https://github.com/Amanieu/parking_lot) |
| `pastey` | 0.2.3 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/as1100k/pastey](https://github.com/as1100k/pastey) |
| `pem` | 4.0.0 | `MIT` | `MIT` | [https://github.com/jcreekmore/pem-rs](https://github.com/jcreekmore/pem-rs) |
| `percent-encoding` | 2.3.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/servo/rust-url/](https://github.com/servo/rust-url/) |
| `petgraph` | 0.8.3 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/petgraph/petgraph](https://github.com/petgraph/petgraph) |
| `pin-project` | 1.1.13 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/taiki-e/pin-project](https://github.com/taiki-e/pin-project) |
| `pin-project-internal` | 1.1.13 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/taiki-e/pin-project](https://github.com/taiki-e/pin-project) |
| `pin-project-lite` | 0.2.17 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/taiki-e/pin-project-lite](https://github.com/taiki-e/pin-project-lite) |
| `pkg-config` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/pkg-config-rs](https://github.com/rust-lang/pkg-config-rs) |
| `potential_utf` | 0.1.6 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `powerfmt` | 0.2.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/jhpratt/powerfmt](https://github.com/jhpratt/powerfmt) |
| `ppv-lite86` | 0.2.21 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/cryptocorrosion/cryptocorrosion](https://github.com/cryptocorrosion/cryptocorrosion) |
| `prettyplease` | 0.2.37 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/prettyplease](https://github.com/dtolnay/prettyplease) |
| `proc-macro2` | 1.0.107 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/proc-macro2](https://github.com/dtolnay/proc-macro2) |
| `prost` | 0.14.4 | `Apache-2.0` | `Apache-2.0` | [https://github.com/tokio-rs/prost](https://github.com/tokio-rs/prost) |
| `prost-build` | 0.14.4 | `Apache-2.0` | `Apache-2.0` | [https://github.com/tokio-rs/prost](https://github.com/tokio-rs/prost) |
| `prost-derive` | 0.14.4 | `Apache-2.0` | `Apache-2.0` | [https://github.com/tokio-rs/prost](https://github.com/tokio-rs/prost) |
| `prost-types` | 0.14.4 | `Apache-2.0` | `Apache-2.0` | [https://github.com/tokio-rs/prost](https://github.com/tokio-rs/prost) |
| `pulldown-cmark` | 0.13.5 | `MIT` | `MIT` | [https://github.com/raphlinus/pulldown-cmark](https://github.com/raphlinus/pulldown-cmark) |
| `pulldown-cmark-to-cmark` | 22.0.3 | `Apache-2.0` | `Apache-2.0` | [https://github.com/Byron/pulldown-cmark-to-cmark](https://github.com/Byron/pulldown-cmark-to-cmark) |
| `quinn` | 0.11.12 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/quinn-rs/quinn](https://github.com/quinn-rs/quinn) |
| `quinn-proto` | 0.11.19 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/quinn-rs/quinn](https://github.com/quinn-rs/quinn) |
| `quinn-udp` | 0.5.16 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/quinn-rs/quinn](https://github.com/quinn-rs/quinn) |
| `quote` | 1.0.47 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/quote](https://github.com/dtolnay/quote) |
| `r-efi` | 5.3.0 | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | `MIT` | [https://github.com/r-efi/r-efi](https://github.com/r-efi/r-efi) |
| `r-efi` | 6.0.0 | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | `MIT` | [https://github.com/r-efi/r-efi](https://github.com/r-efi/r-efi) |
| `rand` | 0.10.3 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-random/rand](https://github.com/rust-random/rand) |
| `rand` | 0.9.5 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-random/rand](https://github.com/rust-random/rand) |
| `rand_chacha` | 0.9.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-random/rand](https://github.com/rust-random/rand) |
| `rand_core` | 0.10.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-random/rand_core](https://github.com/rust-random/rand_core) |
| `rand_core` | 0.9.5 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-random/rand](https://github.com/rust-random/rand) |
| `rand_pcg` | 0.10.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-random/rngs](https://github.com/rust-random/rngs) |
| `rcgen` | 0.14.10 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rustls/rcgen](https://github.com/rustls/rcgen) |
| `redox_syscall` | 0.5.18 | `MIT` | `MIT` | [https://gitlab.redox-os.org/redox-os/syscall](https://gitlab.redox-os.org/redox-os/syscall) |
| `ref-cast` | 1.0.27 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/ref-cast](https://github.com/dtolnay/ref-cast) |
| `ref-cast-impl` | 1.0.27 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/ref-cast](https://github.com/dtolnay/ref-cast) |
| `referencing` | 0.60.2 | `MIT` | `MIT` | [https://github.com/Stranger6667/jsonschema](https://github.com/Stranger6667/jsonschema) |
| `regex` | 1.13.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/regex](https://github.com/rust-lang/regex) |
| `regex-automata` | 0.4.18 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/regex](https://github.com/rust-lang/regex) |
| `regex-syntax` | 0.8.11 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/regex](https://github.com/rust-lang/regex) |
| `reqwest` | 0.12.28 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/seanmonstar/reqwest](https://github.com/seanmonstar/reqwest) |
| `ring` | 0.17.14 | `Apache-2.0 AND ISC` | `Apache-2.0 AND ISC` | [https://github.com/briansmith/ring](https://github.com/briansmith/ring) |
| `rustc-hash` | 2.1.3 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/rust-lang/rustc-hash](https://github.com/rust-lang/rustc-hash) |
| `rustc_version` | 0.4.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/djc/rustc-version-rs](https://github.com/djc/rustc-version-rs) |
| `rusticata-macros` | 4.1.0 | `MIT/Apache-2.0` | `MIT` | [https://github.com/rusticata/rusticata-macros](https://github.com/rusticata/rusticata-macros) |
| `rustix` | 1.1.6 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | `MIT` | [https://github.com/bytecodealliance/rustix](https://github.com/bytecodealliance/rustix) |
| `rustls` | 0.23.46 | `Apache-2.0 OR ISC OR MIT` | `MIT` | [https://github.com/rustls/rustls](https://github.com/rustls/rustls) |
| `rustls-native-certs` | 0.8.5 | `Apache-2.0 OR ISC OR MIT` | `MIT` | [https://github.com/rustls/rustls-native-certs](https://github.com/rustls/rustls-native-certs) |
| `rustls-pki-types` | 1.15.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rustls/pki-types](https://github.com/rustls/pki-types) |
| `rustls-webpki` | 0.103.16 | `ISC` | `ISC` | [https://github.com/rustls/webpki](https://github.com/rustls/webpki) |
| `rustversion` | 1.0.23 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/rustversion](https://github.com/dtolnay/rustversion) |
| `ryu` | 1.0.23 | `Apache-2.0 OR BSL-1.0` | `Apache-2.0` | [https://github.com/dtolnay/ryu](https://github.com/dtolnay/ryu) |
| `schannel` | 0.1.30 | `MIT` | `MIT` | [https://github.com/steffengy/schannel-rs](https://github.com/steffengy/schannel-rs) |
| `scopeguard` | 1.2.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/bluss/scopeguard](https://github.com/bluss/scopeguard) |
| `security-framework` | 3.7.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/kornelski/rust-security-framework](https://github.com/kornelski/rust-security-framework) |
| `security-framework-sys` | 2.17.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/kornelski/rust-security-framework](https://github.com/kornelski/rust-security-framework) |
| `semver` | 1.0.28 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/semver](https://github.com/dtolnay/semver) |
| `serde` | 1.0.229 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/serde-rs/serde](https://github.com/serde-rs/serde) |
| `serde_core` | 1.0.229 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/serde-rs/serde](https://github.com/serde-rs/serde) |
| `serde_derive` | 1.0.229 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/serde-rs/serde](https://github.com/serde-rs/serde) |
| `serde_json` | 1.0.154 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/serde-rs/json](https://github.com/serde-rs/json) |
| `serde_path_to_error` | 0.1.20 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/path-to-error](https://github.com/dtolnay/path-to-error) |
| `serde_spanned` | 1.1.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/toml-rs/toml](https://github.com/toml-rs/toml) |
| `serde_urlencoded` | 0.7.1 | `MIT/Apache-2.0` | `MIT` | [https://github.com/nox/serde_urlencoded](https://github.com/nox/serde_urlencoded) |
| `sha1` | 0.10.7 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/hashes](https://github.com/RustCrypto/hashes) |
| `sha1` | 0.11.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/hashes](https://github.com/RustCrypto/hashes) |
| `sha2` | 0.10.9 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/hashes](https://github.com/RustCrypto/hashes) |
| `sha2` | 0.11.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/RustCrypto/hashes](https://github.com/RustCrypto/hashes) |
| `sharded-slab` | 0.1.7 | `MIT` | `MIT` | [https://github.com/hawkw/sharded-slab](https://github.com/hawkw/sharded-slab) |
| `shlex` | 2.0.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/comex/rust-shlex](https://github.com/comex/rust-shlex) |
| `signal-hook-registry` | 1.4.8 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/vorner/signal-hook](https://github.com/vorner/signal-hook) |
| `slab` | 0.4.12 | `MIT` | `MIT` | [https://github.com/tokio-rs/slab](https://github.com/tokio-rs/slab) |
| `smallvec` | 1.16.4 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/servo/rust-smallvec](https://github.com/servo/rust-smallvec) |
| `socket2` | 0.6.5 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rust-lang/socket2](https://github.com/rust-lang/socket2) |
| `spin` | 0.9.9 | `MIT` | `MIT` | [https://github.com/mvdnes/spin-rs](https://github.com/mvdnes/spin-rs) |
| `sqlx` | 0.9.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/launchbadge/sqlx](https://github.com/launchbadge/sqlx) |
| `sqlx-core` | 0.9.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/launchbadge/sqlx](https://github.com/launchbadge/sqlx) |
| `sqlx-macros` | 0.9.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/launchbadge/sqlx](https://github.com/launchbadge/sqlx) |
| `sqlx-macros-core` | 0.9.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/launchbadge/sqlx](https://github.com/launchbadge/sqlx) |
| `sqlx-mysql` | 0.9.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/launchbadge/sqlx](https://github.com/launchbadge/sqlx) |
| `sqlx-postgres` | 0.9.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/launchbadge/sqlx](https://github.com/launchbadge/sqlx) |
| `sqlx-sqlite` | 0.9.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/launchbadge/sqlx](https://github.com/launchbadge/sqlx) |
| `stable_deref_trait` | 1.2.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/storyyeller/stable_deref_trait](https://github.com/storyyeller/stable_deref_trait) |
| `stringprep` | 0.1.5 | `MIT/Apache-2.0` | `MIT` | [https://github.com/sfackler/rust-stringprep](https://github.com/sfackler/rust-stringprep) |
| `strsim` | 0.11.1 | `MIT` | `MIT` | [https://github.com/rapidfuzz/strsim-rs](https://github.com/rapidfuzz/strsim-rs) |
| `strum` | 0.28.0 | `MIT` | `MIT` | [https://github.com/Peternator7/strum](https://github.com/Peternator7/strum) |
| `strum_macros` | 0.28.0 | `MIT` | `MIT` | [https://github.com/Peternator7/strum](https://github.com/Peternator7/strum) |
| `subtle` | 2.6.1 | `BSD-3-Clause` | `BSD-3-Clause` | [https://github.com/dalek-cryptography/subtle](https://github.com/dalek-cryptography/subtle) |
| `syn` | 2.0.119 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/syn](https://github.com/dtolnay/syn) |
| `syn` | 3.0.9 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/syn](https://github.com/dtolnay/syn) |
| `sync_wrapper` | 1.0.2 | `Apache-2.0` | `Apache-2.0` | [https://github.com/Actyx/sync_wrapper](https://github.com/Actyx/sync_wrapper) |
| `synstructure` | 0.13.2 | `MIT` | `MIT` | [https://github.com/mystor/synstructure](https://github.com/mystor/synstructure) |
| `synstructure` | 0.14.0 | `MIT` | `MIT` | [https://github.com/mystor/synstructure](https://github.com/mystor/synstructure) |
| `tempfile` | 3.27.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/Stebalien/tempfile](https://github.com/Stebalien/tempfile) |
| `thiserror` | 2.0.21 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/thiserror](https://github.com/dtolnay/thiserror) |
| `thiserror-impl` | 2.0.21 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/dtolnay/thiserror](https://github.com/dtolnay/thiserror) |
| `thread_local` | 1.1.10 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/Amanieu/thread_local-rs](https://github.com/Amanieu/thread_local-rs) |
| `time` | 0.3.55 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/time-rs/time](https://github.com/time-rs/time) |
| `time-core` | 0.1.9 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/time-rs/time](https://github.com/time-rs/time) |
| `time-macros` | 0.2.32 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/time-rs/time](https://github.com/time-rs/time) |
| `tinystr` | 0.8.5 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `tinyvec` | 1.13.3 | `Zlib OR Apache-2.0 OR MIT` | `MIT` | [https://github.com/Lokathor/tinyvec](https://github.com/Lokathor/tinyvec) |
| `tokio` | 1.54.1 | `MIT` | `MIT` | [https://github.com/tokio-rs/tokio](https://github.com/tokio-rs/tokio) |
| `tokio-macros` | 2.7.2 | `MIT` | `MIT` | [https://github.com/tokio-rs/tokio](https://github.com/tokio-rs/tokio) |
| `tokio-rustls` | 0.26.6 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rustls/tokio-rustls](https://github.com/rustls/tokio-rustls) |
| `tokio-stream` | 0.1.19 | `MIT` | `MIT` | [https://github.com/tokio-rs/tokio](https://github.com/tokio-rs/tokio) |
| `tokio-tungstenite` | 0.29.0 | `MIT` | `MIT` | [https://github.com/snapview/tokio-tungstenite](https://github.com/snapview/tokio-tungstenite) |
| `tokio-tungstenite` | 0.30.0 | `MIT` | `MIT` | [https://github.com/snapview/tokio-tungstenite](https://github.com/snapview/tokio-tungstenite) |
| `tokio-util` | 0.7.20 | `MIT` | `MIT` | [https://github.com/tokio-rs/tokio](https://github.com/tokio-rs/tokio) |
| `toml` | 1.1.8+spec-1.1.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/toml-rs/toml](https://github.com/toml-rs/toml) |
| `toml_datetime` | 1.1.2+spec-1.1.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/toml-rs/toml](https://github.com/toml-rs/toml) |
| `toml_parser` | 1.1.5+spec-1.1.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/toml-rs/toml](https://github.com/toml-rs/toml) |
| `toml_writer` | 1.1.3+spec-1.1.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/toml-rs/toml](https://github.com/toml-rs/toml) |
| `tonic` | 0.14.6 | `MIT` | `MIT` | [https://github.com/hyperium/tonic](https://github.com/hyperium/tonic) |
| `tonic-build` | 0.14.6 | `MIT` | `MIT` | [https://github.com/hyperium/tonic](https://github.com/hyperium/tonic) |
| `tonic-prost` | 0.14.6 | `MIT` | `MIT` | [https://github.com/hyperium/tonic](https://github.com/hyperium/tonic) |
| `tonic-prost-build` | 0.14.6 | `MIT` | `MIT` | [https://github.com/hyperium/tonic](https://github.com/hyperium/tonic) |
| `tower` | 0.5.3 | `MIT` | `MIT` | [https://github.com/tower-rs/tower](https://github.com/tower-rs/tower) |
| `tower-http` | 0.6.11 | `MIT` | `MIT` | [https://github.com/tower-rs/tower-http](https://github.com/tower-rs/tower-http) |
| `tower-layer` | 0.3.3 | `MIT` | `MIT` | [https://github.com/tower-rs/tower](https://github.com/tower-rs/tower) |
| `tower-service` | 0.3.3 | `MIT` | `MIT` | [https://github.com/tower-rs/tower](https://github.com/tower-rs/tower) |
| `tracing` | 0.1.44 | `MIT` | `MIT` | [https://github.com/tokio-rs/tracing](https://github.com/tokio-rs/tracing) |
| `tracing-attributes` | 0.1.31 | `MIT` | `MIT` | [https://github.com/tokio-rs/tracing](https://github.com/tokio-rs/tracing) |
| `tracing-core` | 0.1.36 | `MIT` | `MIT` | [https://github.com/tokio-rs/tracing](https://github.com/tokio-rs/tracing) |
| `tracing-subscriber` | 0.3.23 | `MIT` | `MIT` | [https://github.com/tokio-rs/tracing](https://github.com/tokio-rs/tracing) |
| `try-lock` | 0.2.5 | `MIT` | `MIT` | [https://github.com/seanmonstar/try-lock](https://github.com/seanmonstar/try-lock) |
| `tungstenite` | 0.29.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/snapview/tungstenite-rs](https://github.com/snapview/tungstenite-rs) |
| `tungstenite` | 0.30.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/snapview/tungstenite-rs](https://github.com/snapview/tungstenite-rs) |
| `typenum` | 1.20.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/paholg/typenum](https://github.com/paholg/typenum) |
| `unicase` | 2.10.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/seanmonstar/unicase](https://github.com/seanmonstar/unicase) |
| `unicode-bidi` | 0.3.18 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/servo/unicode-bidi](https://github.com/servo/unicode-bidi) |
| `unicode-general-category` | 1.1.0 | `Apache-2.0` | `Apache-2.0` | [https://github.com/yeslogic/unicode-general-category](https://github.com/yeslogic/unicode-general-category) |
| `unicode-ident` | 1.0.27 | `(MIT OR Apache-2.0) AND Unicode-3.0` | `MIT AND Unicode-3.0` | [https://github.com/dtolnay/unicode-ident](https://github.com/dtolnay/unicode-ident) |
| `unicode-normalization` | 0.1.26 | `(MIT OR Apache-2.0) AND Unicode-3.0` | `MIT AND Unicode-3.0` | [https://github.com/unicode-rs/unicode-normalization](https://github.com/unicode-rs/unicode-normalization) |
| `unicode-properties` | 0.1.4 | `MIT/Apache-2.0` | `MIT` | [https://github.com/unicode-rs/unicode-properties](https://github.com/unicode-rs/unicode-properties) |
| `untrusted` | 0.9.0 | `ISC` | `ISC` | [https://github.com/briansmith/untrusted](https://github.com/briansmith/untrusted) |
| `url` | 2.5.8 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/servo/rust-url](https://github.com/servo/rust-url) |
| `utf8_iter` | 1.0.4 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/hsivonen/utf8_iter](https://github.com/hsivonen/utf8_iter) |
| `utf8parse` | 0.2.2 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/alacritty/vte](https://github.com/alacritty/vte) |
| `utoipa` | 6.0.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/juhaku/utoipa](https://github.com/juhaku/utoipa) |
| `utoipa-axum` | 0.3.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/juhaku/utoipa](https://github.com/juhaku/utoipa) |
| `utoipa-gen` | 6.0.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/juhaku/utoipa](https://github.com/juhaku/utoipa) |
| `uuid` | 1.29.0 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/uuid-rs/uuid](https://github.com/uuid-rs/uuid) |
| `uuid-simd` | 0.8.0 | `MIT` | `MIT` | [https://github.com/Nugine/simd](https://github.com/Nugine/simd) |
| `vcpkg` | 0.2.15 | `MIT/Apache-2.0` | `MIT` | [https://github.com/mcgoo/vcpkg-rs](https://github.com/mcgoo/vcpkg-rs) |
| `version_check` | 0.9.5 | `MIT/Apache-2.0` | `MIT` | [https://github.com/SergioBenitez/version_check](https://github.com/SergioBenitez/version_check) |
| `vsimd` | 0.8.0 | `MIT` | `MIT` | [https://github.com/Nugine/simd](https://github.com/Nugine/simd) |
| `want` | 0.3.2 | `MIT` | `MIT` | [https://github.com/seanmonstar/want](https://github.com/seanmonstar/want) |
| `wasi` | 0.11.1+wasi-snapshot-preview1 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | `MIT` | [https://github.com/bytecodealliance/wasi](https://github.com/bytecodealliance/wasi) |
| `wasip2` | 1.0.4+wasi-0.2.12 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | `MIT` | [https://github.com/bytecodealliance/wasi-rs](https://github.com/bytecodealliance/wasi-rs) |
| `wasm-bindgen` | 0.2.129 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/wasm-bindgen/wasm-bindgen](https://github.com/wasm-bindgen/wasm-bindgen) |
| `wasm-bindgen-futures` | 0.4.79 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/futures](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/futures) |
| `wasm-bindgen-macro` | 0.2.129 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro) |
| `wasm-bindgen-macro-support` | 0.2.129 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/wasm-bindgen/wasm-bindgen/tree/main/crates/macro-support](https://github.com/wasm-bindgen/wasm-bindgen/tree/main/crates/macro-support) |
| `wasm-bindgen-shared` | 0.2.129 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/shared](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/shared) |
| `web-sys` | 0.3.106 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/web-sys](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/web-sys) |
| `web-time` | 1.1.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/daxpedda/web-time](https://github.com/daxpedda/web-time) |
| `whoami` | 2.1.3 | `Apache-2.0 OR BSL-1.0 OR MIT` | `MIT` | [https://github.com/ardaku/whoami](https://github.com/ardaku/whoami) |
| `windows-core` | 0.62.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows-implement` | 0.60.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows-interface` | 0.59.3 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows-link` | 0.2.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows-result` | 0.4.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows-strings` | 0.5.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows-sys` | 0.52.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows-sys` | 0.61.2 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows-targets` | 0.52.6 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows_aarch64_gnullvm` | 0.52.6 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows_aarch64_msvc` | 0.52.6 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows_i686_gnu` | 0.52.6 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows_i686_gnullvm` | 0.52.6 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows_i686_msvc` | 0.52.6 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows_x86_64_gnu` | 0.52.6 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows_x86_64_gnullvm` | 0.52.6 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `windows_x86_64_msvc` | 0.52.6 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/microsoft/windows-rs](https://github.com/microsoft/windows-rs) |
| `winnow` | 1.0.4 | `MIT` | `MIT` | [https://github.com/winnow-rs/winnow](https://github.com/winnow-rs/winnow) |
| `wit-bindgen` | 0.57.1 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | `MIT` | [https://github.com/bytecodealliance/wit-bindgen](https://github.com/bytecodealliance/wit-bindgen) |
| `writeable` | 0.6.4 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `x509-parser` | 0.18.1 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/rusticata/x509-parser](https://github.com/rusticata/x509-parser) |
| `yasna` | 0.6.0 | `MIT OR Apache-2.0` | `MIT` | [https://github.com/qnighy/yasna.rs](https://github.com/qnighy/yasna.rs) |
| `yoke` | 0.8.4 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `yoke-derive` | 0.8.4 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `zerocopy` | 0.8.63 | `BSD-2-Clause OR Apache-2.0 OR MIT` | `MIT` | [https://github.com/google/zerocopy](https://github.com/google/zerocopy) |
| `zerocopy-derive` | 0.8.63 | `BSD-2-Clause OR Apache-2.0 OR MIT` | `MIT` | [https://github.com/google/zerocopy](https://github.com/google/zerocopy) |
| `zerofrom` | 0.1.8 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `zerofrom-derive` | 0.1.8 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `zeroize` | 1.9.1 | `Apache-2.0 OR MIT` | `MIT` | [https://github.com/RustCrypto/utils](https://github.com/RustCrypto/utils) |
| `zerotrie` | 0.2.5 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `zerovec` | 0.11.9 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `zerovec-derive` | 0.11.7 | `Unicode-3.0` | `Unicode-3.0` | [https://github.com/unicode-org/icu4x](https://github.com/unicode-org/icu4x) |
| `zmij` | 1.0.23 | `MIT` | `MIT` | [https://github.com/dtolnay/zmij](https://github.com/dtolnay/zmij) |

## Runtime-пакеты npm

Пакеты веб-интерфейса `web/`, поставляемые в составе клиентского приложения. Всего: 90.

| Пакет | Версия | Лицензия | Выбрано | Репозиторий |
| --- | --- | --- | --- | --- |
| `@jridgewell/gen-mapping` | 0.3.13 | `MIT` | `MIT` | [https://github.com/jridgewell/sourcemaps](https://github.com/jridgewell/sourcemaps) |
| `@jridgewell/remapping` | 2.3.5 | `MIT` | `MIT` | [https://github.com/jridgewell/sourcemaps](https://github.com/jridgewell/sourcemaps) |
| `@jridgewell/resolve-uri` | 3.1.2 | `MIT` | `MIT` | [https://github.com/jridgewell/resolve-uri](https://github.com/jridgewell/resolve-uri) |
| `@jridgewell/sourcemap-codec` | 1.6.0 | `MIT` | `MIT` | [https://github.com/jridgewell/sourcemaps](https://github.com/jridgewell/sourcemaps) |
| `@jridgewell/trace-mapping` | 0.3.31 | `MIT` | `MIT` | [https://github.com/jridgewell/sourcemaps](https://github.com/jridgewell/sourcemaps) |
| `@noble/hashes` | 2.4.0 | `MIT` | `MIT` | [https://github.com/paulmillr/noble-hashes](https://github.com/paulmillr/noble-hashes) |
| `@oxc-project/types` | 0.155.0 | `MIT` | `MIT` | [https://github.com/oxc-project/oxc](https://github.com/oxc-project/oxc) |
| `@polka/url` | 1.0.0-next.29 | `MIT` | `MIT` | [https://github.com/lukeed/polka](https://github.com/lukeed/polka) |
| `@rolldown/binding-android-arm-eabi` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/binding-android-arm64` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/binding-darwin-arm64` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/binding-darwin-x64` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/binding-freebsd-x64` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/binding-linux-arm-gnueabihf` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/binding-linux-arm64-gnu` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/binding-linux-arm64-musl` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/binding-linux-ppc64-gnu` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/binding-linux-s390x-gnu` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/binding-linux-x64-gnu` | 1.2.15 | `MIT` | `MIT` | [https://github.com/rolldown/rolldown](https://github.com/rolldown/rolldown) |
| `@rolldown/binding-linux-x64-musl` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/binding-openharmony-arm64` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/binding-win32-arm64-msvc` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/binding-win32-x64-msvc` | 1.2.15 | `MIT` | `MIT` | - |
| `@rolldown/pluginutils` | 1.0.1 | `MIT` | `MIT` | [https://github.com/rolldown/plugins](https://github.com/rolldown/plugins) |
| `@standard-schema/spec` | 1.1.0 | `MIT` | `MIT` | [https://github.com/standard-schema/standard-schema](https://github.com/standard-schema/standard-schema) |
| `@sveltejs/acorn-typescript` | 1.0.13 | `MIT` | `MIT` | [https://github.com/sveltejs/acorn-typescript](https://github.com/sveltejs/acorn-typescript) |
| `@sveltejs/kit` | 2.70.3 | `MIT` | `MIT` | [https://github.com/sveltejs/kit](https://github.com/sveltejs/kit) |
| `@sveltejs/vite-plugin-svelte` | 7.3.1 | `MIT` | `MIT` | [https://github.com/sveltejs/vite-plugin-svelte](https://github.com/sveltejs/vite-plugin-svelte) |
| `@types/cookie` | 0.6.0 | `MIT` | `MIT` | [https://github.com/DefinitelyTyped/DefinitelyTyped](https://github.com/DefinitelyTyped/DefinitelyTyped) |
| `@types/estree` | 1.0.9 | `MIT` | `MIT` | [https://github.com/DefinitelyTyped/DefinitelyTyped](https://github.com/DefinitelyTyped/DefinitelyTyped) |
| `@types/node` | 24.19.2 | `MIT` | `MIT` | [https://github.com/DefinitelyTyped/DefinitelyTyped](https://github.com/DefinitelyTyped/DefinitelyTyped) |
| `@types/trusted-types` | 2.0.7 | `MIT` | `MIT` | [https://github.com/DefinitelyTyped/DefinitelyTyped](https://github.com/DefinitelyTyped/DefinitelyTyped) |
| `@typescript-eslint/types` | 8.72.1 | `MIT` | `MIT` | [https://github.com/typescript-eslint/typescript-eslint](https://github.com/typescript-eslint/typescript-eslint) |
| `acorn` | 8.19.0 | `MIT` | `MIT` | [https://github.com/acornjs/acorn](https://github.com/acornjs/acorn) |
| `argparse` | 3.0.4 | `PSF-2.0` | — | [https://github.com/nodeca/argparse](https://github.com/nodeca/argparse) |
| `aria-query` | 5.3.1 | `Apache-2.0` | `Apache-2.0` | [https://github.com/A11yance/aria-query](https://github.com/A11yance/aria-query) |
| `axobject-query` | 4.1.0 | `Apache-2.0` | `Apache-2.0` | [https://github.com/A11yance/axobject-query](https://github.com/A11yance/axobject-query) |
| `clsx` | 2.1.1 | `MIT` | `MIT` | [https://github.com/lukeed/clsx](https://github.com/lukeed/clsx) |
| `cookie` | 0.6.0 | `MIT` | `MIT` | [https://github.com/jshttp/cookie](https://github.com/jshttp/cookie) |
| `deepmerge` | 4.3.1 | `MIT` | `MIT` | [https://github.com/TehShrike/deepmerge](https://github.com/TehShrike/deepmerge) |
| `detect-libc` | 2.1.2 | `Apache-2.0` | `Apache-2.0` | [https://github.com/lovell/detect-libc](https://github.com/lovell/detect-libc) |
| `devalue` | 5.9.4 | `MIT` | `MIT` | [https://github.com/sveltejs/devalue](https://github.com/sveltejs/devalue) |
| `dompurify` | 3.4.17 | `(MPL-2.0 OR Apache-2.0)` | `Apache-2.0` | [https://github.com/cure53/DOMPurify](https://github.com/cure53/DOMPurify) |
| `entities` | 8.1.0 | `BSD-2-Clause` | `BSD-2-Clause` | [https://github.com/fb55/entities](https://github.com/fb55/entities) |
| `esm-env` | 1.2.2 | `MIT` | `MIT` | [https://github.com/benmccann/esm-env](https://github.com/benmccann/esm-env) |
| `esrap` | 2.4.0 | `MIT` | `MIT` | [https://github.com/sveltejs/esrap](https://github.com/sveltejs/esrap) |
| `fdir` | 6.5.0 | `MIT` | `MIT` | [https://github.com/thecodrr/fdir](https://github.com/thecodrr/fdir) |
| `fsevents` | 2.3.3 | `MIT` | `MIT` | - |
| `highlight.js` | 11.12.0 | `BSD-3-Clause` | `BSD-3-Clause` | [https://github.com/highlightjs/highlight.js](https://github.com/highlightjs/highlight.js) |
| `html-to-image` | 1.11.13 | `MIT` | `MIT` | [https://github.com/bubkoo/html-to-image](https://github.com/bubkoo/html-to-image) |
| `is-reference` | 3.0.3 | `MIT` | `MIT` | [https://github.com/Rich-Harris/is-reference](https://github.com/Rich-Harris/is-reference) |
| `kleur` | 4.1.5 | `MIT` | `MIT` | [https://github.com/lukeed/kleur](https://github.com/lukeed/kleur) |
| `lightningcss` | 1.33.0 | `MPL-2.0` | `MPL-2.0` | [https://github.com/parcel-bundler/lightningcss](https://github.com/parcel-bundler/lightningcss) |
| `lightningcss-android-arm64` | 1.33.0 | `MPL-2.0` | `MPL-2.0` | - |
| `lightningcss-darwin-arm64` | 1.33.0 | `MPL-2.0` | `MPL-2.0` | - |
| `lightningcss-darwin-x64` | 1.33.0 | `MPL-2.0` | `MPL-2.0` | - |
| `lightningcss-freebsd-x64` | 1.33.0 | `MPL-2.0` | `MPL-2.0` | - |
| `lightningcss-linux-arm-gnueabihf` | 1.33.0 | `MPL-2.0` | `MPL-2.0` | - |
| `lightningcss-linux-arm64-gnu` | 1.33.0 | `MPL-2.0` | `MPL-2.0` | - |
| `lightningcss-linux-arm64-musl` | 1.33.0 | `MPL-2.0` | `MPL-2.0` | - |
| `lightningcss-linux-x64-gnu` | 1.33.0 | `MPL-2.0` | `MPL-2.0` | [https://github.com/parcel-bundler/lightningcss](https://github.com/parcel-bundler/lightningcss) |
| `lightningcss-linux-x64-musl` | 1.33.0 | `MPL-2.0` | `MPL-2.0` | - |
| `lightningcss-win32-arm64-msvc` | 1.33.0 | `MPL-2.0` | `MPL-2.0` | - |
| `lightningcss-win32-x64-msvc` | 1.33.0 | `MPL-2.0` | `MPL-2.0` | - |
| `linkify-it` | 6.1.0 | `MIT` | `MIT` | [https://github.com/markdown-it/linkify-it](https://github.com/markdown-it/linkify-it) |
| `locate-character` | 3.0.0 | `MIT` | `MIT` | [https://gitlab.com/Rich-Harris/locate-character](https://gitlab.com/Rich-Harris/locate-character) |
| `magic-string` | 0.30.21 | `MIT` | `MIT` | [https://github.com/Rich-Harris/magic-string](https://github.com/Rich-Harris/magic-string) |
| `magic-string` | 1.4.5 | `MIT` | `MIT` | [https://github.com/Rich-Harris/magic-string](https://github.com/Rich-Harris/magic-string) |
| `markdown-it` | 15.0.3 | `MIT` | `MIT` | [https://github.com/markdown-it/markdown-it](https://github.com/markdown-it/markdown-it) |
| `mdurl` | 2.1.0 | `MIT` | `MIT` | [https://github.com/markdown-it/mdurl](https://github.com/markdown-it/mdurl) |
| `mrmime` | 2.0.1 | `MIT` | `MIT` | [https://github.com/lukeed/mrmime](https://github.com/lukeed/mrmime) |
| `nanoid` | 3.3.20 | `MIT` | `MIT` | [https://github.com/ai/nanoid](https://github.com/ai/nanoid) |
| `obug` | 2.2.1 | `MIT` | `MIT` | [https://github.com/sxzz/obug](https://github.com/sxzz/obug) |
| `picocolors` | 1.1.1 | `ISC` | `ISC` | [https://github.com/alexeyraspopov/picocolors](https://github.com/alexeyraspopov/picocolors) |
| `picomatch` | 4.0.7 | `MIT` | `MIT` | [https://github.com/micromatch/picomatch](https://github.com/micromatch/picomatch) |
| `postcss` | 8.5.32 | `MIT` | `MIT` | [https://github.com/postcss/postcss](https://github.com/postcss/postcss) |
| `punycode.js` | 2.3.1 | `MIT` | `MIT` | [https://github.com/mathiasbynens/punycode.js](https://github.com/mathiasbynens/punycode.js) |
| `rolldown` | 1.2.15 | `MIT` | `MIT` | [https://github.com/rolldown/rolldown](https://github.com/rolldown/rolldown) |
| `set-cookie-parser` | 3.2.1 | `MIT` | `MIT` | [https://github.com/nfriedly/set-cookie-parser](https://github.com/nfriedly/set-cookie-parser) |
| `sirv` | 3.0.2 | `MIT` | `MIT` | [https://github.com/lukeed/sirv](https://github.com/lukeed/sirv) |
| `source-map-js` | 1.2.2 | `BSD-3-Clause` | `BSD-3-Clause` | [https://github.com/7rulnik/source-map-js](https://github.com/7rulnik/source-map-js) |
| `svelte` | 5.57.3 | `MIT` | `MIT` | [https://github.com/sveltejs/svelte](https://github.com/sveltejs/svelte) |
| `tinyglobby` | 0.2.17 | `MIT` | `MIT` | [https://github.com/SuperchupuDev/tinyglobby](https://github.com/SuperchupuDev/tinyglobby) |
| `totalist` | 3.0.1 | `MIT` | `MIT` | [https://github.com/lukeed/totalist](https://github.com/lukeed/totalist) |
| `typescript` | 5.9.3 | `Apache-2.0` | `Apache-2.0` | [https://github.com/microsoft/TypeScript](https://github.com/microsoft/TypeScript) |
| `uc.micro` | 3.0.0 | `MIT` | `MIT` | [https://github.com/markdown-it/uc.micro](https://github.com/markdown-it/uc.micro) |
| `undici-types` | 7.24.6 | `MIT` | `MIT` | [https://github.com/nodejs/undici](https://github.com/nodejs/undici) |
| `vite` | 8.3.5 | `MIT` | `MIT` | [https://github.com/vitejs/vite](https://github.com/vitejs/vite) |
| `vitefu` | 1.1.3 | `MIT` | `MIT` | [https://github.com/svitejs/vitefu](https://github.com/svitejs/vitefu) |
| `zimmerframe` | 1.1.5 | `MIT` | `MIT` | [https://github.com/sveltejs/zimmerframe](https://github.com/sveltejs/zimmerframe) |

## Пакеты npm для разработки

Пакеты, используемые только на этапе сборки, линтинга и тестирования (`dev: true`); не поставляются конечным пользователям. Всего записей: 211.

- `@apidevtools/json-schema-ref-parser` (11.9.3) — `MIT`
- `@asamuzakjp/css-color` (7.1.9) — `MIT`
- `@asamuzakjp/dom-selector` (10.0.5) — `MIT`
- `@axe-core/playwright` (4.14.0) — `MPL-2.0`
- `@babel/code-frame` (7.29.7) — `MIT`
- `@babel/helper-validator-identifier` (7.29.7) — `MIT`
- `@bramus/specificity` (2.4.3) — `MIT`
- `@cacheable/memory` (2.2.0) — `MIT`
- `@cacheable/utils` (2.5.0) — `MIT`
- `@csstools/color-helpers` (6.1.2) — `MIT-0`
- `@csstools/css-calc` (3.4.3) — `MIT`
- `@csstools/css-color-parser` (4.2.6) — `MIT`
- `@csstools/css-parser-algorithms` (4.0.2) — `MIT`
- `@csstools/css-syntax-patches-for-csstree` (1.1.15) — `MIT-0`
- `@csstools/css-tokenizer` (4.0.2) — `MIT`
- `@eslint-community/eslint-utils` (4.10.1) — `MIT`
- `@eslint-community/regexpp` (4.12.2) — `MIT`
- `@eslint/config-array` (0.23.5) — `Apache-2.0`
- `@eslint/config-helpers` (0.7.0) — `Apache-2.0`
- `@eslint/core` (1.2.1) — `Apache-2.0`
- `@eslint/object-schema` (3.0.5) — `Apache-2.0`
- `@eslint/plugin-kit` (0.7.3) — `Apache-2.0`
- `@exodus/bytes` (1.16.0) — `MIT`
- `@humanfs/core` (0.19.2) — `Apache-2.0`
- `@humanfs/node` (0.16.8) — `Apache-2.0`
- `@humanfs/types` (0.15.0) — `Apache-2.0`
- `@humanwhocodes/module-importer` (1.0.1) — `Apache-2.0`
- `@humanwhocodes/retry` (0.4.3) — `Apache-2.0`
- `@jsdevtools/ono` (7.1.3) — `MIT`
- `@keyv/bigmap` (1.3.1) — `MIT`
- `@keyv/serialize` (1.1.1) — `MIT`
- `@playwright/test` (1.64.1) — `Apache-2.0`
- `@redocly/ajv` (8.11.2) — `MIT`
- `@redocly/config` (0.22.0) — `MIT`
- `@redocly/openapi-core` (1.34.22) — `MIT`
- `@sveltejs/adapter-static` (3.0.10) — `MIT`
- `@sveltejs/load-config` (0.2.3) — `MIT`
- `@types/chai` (5.2.3) — `MIT`
- `@types/deep-eql` (4.0.2) — `MIT`
- `@types/esrecurse` (4.3.1) — `MIT`
- `@types/json-schema` (7.0.15) — `MIT`
- `@types/linkify-it` (5.0.0) — `MIT`
- `@types/lodash` (4.17.27) — `MIT`
- `@types/markdown-it` (14.2.0) — `MIT`
- `@types/mdurl` (2.0.0) — `MIT`
- `@typescript-eslint/eslint-plugin` (8.72.1) — `MIT`
- `@typescript-eslint/parser` (8.72.1) — `MIT`
- `@typescript-eslint/project-service` (8.72.1) — `MIT`
- `@typescript-eslint/scope-manager` (8.72.1) — `MIT`
- `@typescript-eslint/tsconfig-utils` (8.72.1) — `MIT`
- `@typescript-eslint/type-utils` (8.72.1) — `MIT`
- `@typescript-eslint/typescript-estree` (8.72.1) — `MIT`
- `@typescript-eslint/utils` (8.72.1) — `MIT`
- `@typescript-eslint/visitor-keys` (8.72.1) — `MIT`
- `@vitest/mocker` (5.0.5) — `MIT`
- `@vitest/spy` (5.0.5) — `MIT`
- `acorn-jsx` (5.3.2) — `MIT`
- `agent-base` (7.1.4) — `MIT`
- `ajv` (6.15.0) — `MIT`
- `ajv` (8.20.0) — `MIT`
- `ajv-formats` (3.0.1) — `MIT`
- `ansi-colors` (4.1.3) — `MIT`
- `argparse` (2.0.1) — `Python-2.0`
- `assertion-error` (2.0.1) — `MIT`
- `axe-core` (4.14.0) — `MPL-2.0`
- `balanced-match` (1.0.2) — `MIT`
- `balanced-match` (4.0.4) — `MIT`
- `bidi-js` (1.1.0) — `MIT`
- `brace-expansion` (2.1.7) — `MIT`
- `brace-expansion` (5.0.12) — `MIT`
- `cacheable` (2.5.0) — `MIT`
- `chai` (6.3.0) — `MIT`
- `change-case` (5.4.4) — `MIT`
- `chokidar` (4.0.3) — `MIT`
- `colorette` (1.4.0) — `MIT`
- `cross-spawn` (7.0.6) — `MIT`
- `css-tree` (3.2.1) — `MIT`
- `cssesc` (3.0.0) — `MIT`
- `data-urls` (8.0.0) — `MIT`
- `debug` (4.4.3) — `MIT`
- `decimal.js` (10.6.0) — `MIT`
- `deep-is` (0.1.4) — `MIT`
- `es-module-lexer` (2.3.2) — `MIT`
- `escape-string-regexp` (4.0.0) — `MIT`
- `eslint` (10.13.0) — `MIT`
- `eslint-plugin-svelte` (3.23.1) — `MIT`
- `eslint-scope` (8.4.0) — `BSD-2-Clause`
- `eslint-scope` (9.1.2) — `BSD-2-Clause`
- `eslint-visitor-keys` (3.4.3) — `Apache-2.0`
- `eslint-visitor-keys` (4.2.1) — `Apache-2.0`
- `eslint-visitor-keys` (5.0.1) — `Apache-2.0`
- `espree` (10.4.0) — `BSD-2-Clause`
- `espree` (11.2.0) — `BSD-2-Clause`
- `esquery` (1.7.0) — `BSD-3-Clause`
- `esrecurse` (4.3.0) — `BSD-2-Clause`
- `estraverse` (5.3.0) — `BSD-2-Clause`
- `estree-walker` (3.0.3) — `MIT`
- `esutils` (2.0.3) — `BSD-2-Clause`
- `expect-type` (1.4.0) — `Apache-2.0`
- `fake-indexeddb` (6.2.6) — `Apache-2.0`
- `fast-deep-equal` (3.1.3) — `MIT`
- `fast-json-stable-stringify` (2.1.0) — `MIT`
- `fast-levenshtein` (2.0.6) — `MIT`
- `fast-uri` (3.1.9) — `BSD-3-Clause`
- `file-entry-cache` (11.1.5) — `MIT`
- `find-up` (5.0.0) — `MIT`
- `flat-cache` (6.1.23) — `MIT`
- `flatted` (3.4.4) — `ISC`
- `glob-parent` (6.0.2) — `ISC`
- `globals` (16.5.0) — `MIT`
- `hashery` (1.5.1) — `MIT`
- `hookified` (1.15.1) — `MIT`
- `hookified` (2.2.0) — `MIT`
- `html-encoding-sniffer` (7.0.0) — `MIT`
- `https-proxy-agent` (7.0.6) — `MIT`
- `ignore` (5.3.2) — `MIT`
- `ignore` (7.0.14) — `MIT`
- `imurmurhash` (0.1.4) — `MIT`
- `index-to-position` (1.2.0) — `MIT`
- `is-extglob` (2.1.1) — `MIT`
- `is-glob` (4.0.3) — `MIT`
- `is-potential-custom-element-name` (1.0.1) — `MIT`
- `isexe` (2.0.0) — `ISC`
- `js-levenshtein` (1.1.6) — `MIT`
- `js-tokens` (4.0.0) — `MIT`
- `js-yaml` (4.3.2) — `MIT`
- `js-yaml` (5.5.0) — `MIT`
- `jsdom` (30.1.3) — `MIT`
- `json-schema-to-typescript` (16.0.0) — `MIT`
- `json-schema-traverse` (0.4.1) — `MIT`
- `json-schema-traverse` (1.0.0) — `MIT`
- `json-stable-stringify-without-jsonify` (1.0.1) — `MIT`
- `keyv` (5.6.0) — `MIT`
- `known-css-properties` (0.37.0) — `MIT`
- `levn` (0.4.1) — `MIT`
- `lilconfig` (2.1.0) — `MIT`
- `locate-path` (6.0.0) — `MIT`
- `lodash` (4.18.1) — `MIT`
- `lru-cache` (11.5.3) — `BlueOak-1.0.0`
- `magic-string` (1.4.5) — `MIT`
- `mdn-data` (2.27.1) — `CC0-1.0`
- `minimatch` (10.2.6) — `BlueOak-1.0.0`
- `minimatch` (5.1.9) — `ISC`
- `minimist` (1.2.8) — `MIT`
- `mri` (1.2.0) — `MIT`
- `ms` (2.1.3) — `MIT`
- `natural-compare` (1.4.0) — `MIT`
- `obug` (3.0.0) — `MIT`
- `openapi-typescript` (7.13.0) — `MIT`
- `optionator` (0.9.4) — `MIT`
- `p-limit` (3.1.0) — `MIT`
- `p-locate` (5.0.0) — `MIT`
- `parse-json` (8.3.0) — `MIT`
- `parse5` (8.0.1) — `MIT`
- `path-exists` (4.0.0) — `MIT`
- `path-key` (3.1.1) — `MIT`
- `pixelmatch` (8.0.0) — `ISC`
- `playwright` (1.64.1) — `Apache-2.0`
- `playwright-core` (1.64.1) — `Apache-2.0`
- `pluralize` (8.0.0) — `MIT`
- `pngjs` (7.0.0) — `MIT`
- `postcss-load-config` (3.1.4) — `MIT`
- `postcss-safe-parser` (7.1.0) — `MIT`
- `postcss-scss` (4.0.9) — `MIT`
- `postcss-selector-parser` (7.1.7) — `MIT`
- `prelude-ls` (1.2.1) — `MIT`
- `prettier` (3.9.10) — `MIT`
- `punycode` (2.3.1) — `MIT`
- `qified` (0.10.1) — `MIT`
- `readdirp` (4.1.2) — `MIT`
- `require-from-string` (2.0.2) — `MIT`
- `sade` (1.8.1) — `MIT`
- `saxes` (6.0.0) — `ISC`
- `semver` (7.8.5) — `ISC`
- `shebang-command` (2.0.0) — `MIT`
- `shebang-regex` (3.0.0) — `MIT`
- `std-env` (4.3.0) — `MIT`
- `supports-color` (10.2.2) — `MIT`
- `svelte-check` (4.7.7) — `MIT`
- `svelte-eslint-parser` (1.10.0) — `MIT`
- `tinybench` (6.2.2) — `MIT`
- `tinyexec` (1.3.1) — `MIT`
- `tldts` (7.4.19) — `MIT`
- `tldts-core` (7.4.20) — `MIT`
- `tough-cookie` (6.0.2) — `BSD-3-Clause`
- `tr46` (7.0.0) — `MIT`
- `ts-api-utils` (2.5.0) — `MIT`
- `type-check` (0.4.0) — `MIT`
- `type-fest` (4.41.0) — `(MIT OR CC0-1.0)` (выбрано: `MIT`)
- `typescript-eslint` (8.72.1) — `MIT`
- `undici` (8.12.3) — `MIT`
- `uri-js` (4.4.1) — `BSD-2-Clause`
- `uri-js-replace` (1.0.2) — `MIT`
- `util-deprecate` (1.0.2) — `MIT`
- `vitest` (5.0.5) — `MIT`
- `w3c-xmlserializer` (6.0.0) — `MIT`
- `webidl-conversions` (8.0.1) — `BSD-2-Clause`
- `whatwg-mimetype` (5.0.0) — `MIT`
- `whatwg-url` (17.3.0) — `MIT`
- `which` (2.0.2) — `ISC`
- `why-is-node-running` (3.2.1) — `MIT`
- `word-wrap` (1.2.5) — `MIT`
- `xml-name-validator` (5.0.0) — `Apache-2.0`
- `xmlchars` (2.2.0) — `MIT`
- `yaml` (1.10.3) — `ISC`
- `yaml-ast-parser` (0.0.43) — `Apache-2.0`
- `yargs-parser` (21.1.1) — `ISC`
- `yocto-queue` (0.1.0) — `MIT`

