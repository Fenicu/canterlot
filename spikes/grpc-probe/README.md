# Прототип gRPC

Самостоятельный пакет для ADR-0001. Он не входит в корневой workspace;
зависимости и их разрешение закреплены собственным `Cargo.lock`.
Требуются Rust 1.99.1, `protoc` и Linux с `/proc/self/status` для замеров RSS.

Из этого каталога:

```bash
source ~/.cargo/env
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

Тест `control_latency_under_data_flood` последовательно запускает два дочерних
процесса, по 10 секунд каждый. Каждый процесс содержит сервер и клиент, использует
TLS с новой тестовой CA и четыре потока Tokio. `shared` клонирует один `Channel`;
`separate` дважды вызывает `Endpoint::connect`. Различие TCP-соединений проверяется
по адресам клиента, наблюдаемым сервером.

Повторить замер отдельно, сохранив JSON с исходными отсортированными RTT:

```bash
source ~/.cargo/env
cargo run --locked --quiet -- shared > measurements/shared.json
cargo run --locked --quiet -- separate > measurements/separate.json
```

`Data` непрерывно передаёт сообщения ровно по 524288 байт после сериализации
Protobuf и получает их эхо. Производитель резервирует место в `mpsc(8)` до
создания сообщения. `queue_max` учитывает занятые и зарезервированные места;
внутренние буферы TLS/HTTP2 в эту очередь не входят. `Control` отправляет
heartbeat раз в 100 мс; RTT измеряется монотонными часами от начала отправки
до соответствующего ответа. Для раздельных соединений тест требует p95 < 300 мс;
для общего соединения порога задержки нет.

RSS измеряется для всего дочернего процесса: до нагрузки, в её конце до
освобождения потоков, а также после каждого heartbeat. Пик является максимумом
выборки, а не показанием `VmHWM`. Прикладной heartbeat 100 мс используется только
для замеров; целевое значение для продукта составляет 5 с.

Для проверки реальных статусов ошибок:

```bash
source ~/.cargo/env
cargo test --test transport oversize_message_is_rejected_without_crash -- --nocapture
cargo test --test transport disconnect_ends_stream_without_library_retry -- --nocapture
```

Поле `response_bytes`, эхо данных, заголовок `x-probe-peer` и отключаемый TCP I/O
служат только тестовым стендом. Полного handshake установки, durable ACK,
прикладного reconnect и рабочего протокола этот пакет не реализует.
