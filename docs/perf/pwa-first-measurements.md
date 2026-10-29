# Первые замеры PWA: P-01…P-06 и дымовые P-13/P-14

Первые числа по ТЗ §16.2 на настоящих хабе и воркере с синтетическим адаптером. Это первые, информационные замеры: промах мимо цели фиксируется здесь и не блокирует этап. Чек-лист с настоящим телефоном и рабочим Firefox: `docs/perf/phone-acceptance.md`.

## Как мерили

- Телефонный проект Playwright: Chromium 412×892, касание, дроссель процессора 4× через CDP (`Emulation.setCPUThrottlingRate`).
- Сеть: loopback; для P-02 и второго ряда P-06 кромка добавляет по 40 мс в каждую сторону (RTT 80 мс, профиль §16.1).
- Хаб, воркер, браузер и кромка работают на одном хосте, поэтому часы общие: пробы берут сырые `Date.now()` и временные метки самого хаба, а `X-Server-Time-Ms` в числа не входит.
- Запуск: `npm run stack:build && npm run test:perf` из `web/`; отчёт пересобирается из файлов `web/e2e/.stack/perf/*.json`.

## Честные ограничения

- Эмулированный телефон на настольном процессоре: дроссель не воспроизводит память, термоограничение и планировщик Android.
- Loopback без потерь и без радиоканала; задержка кромки постоянная, джиттера нет.
- Хост общий: рядом идут сборки других задач, поэтому рядом с каждым распределением записана средняя нагрузка. Число образцов меньше итогового профиля §16.1.
- Реальные числа на Galaxy S25 Ultra и трасса `chrome://inspect` для P-13 и P-14 получаются по чек-листу.

## Сводка

| Замер | N | p50, мс | p95, мс | p99, мс | Цель | Итог |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| P-01 | 200 | 113.35 | 145.52 | 359.4 | p95 ≤ 100 мс | вне цели |
| P-02 | 200 | 229.55 | 311.3 | 623.67 | p95 ≤ 300 мс, p99 ≤ 800 мс | вне цели |
| P-03 | 200 | 27 | 44 | 47.01 | p95 ≤ 200 мс | в цели |
| P-04 | 4020 | 111 | 312 | 368 | p95 ≤ 250 мс, p99 ≤ 800 мс | вне цели |
| P-05 | 50 | 100.25 | 186.82 | 233.56 | p95 ≤ 300 мс | в цели |
| P-06a | 200 | 138.19 | 157.57 | 162.75 | p95 ≤ 300 мс | в цели |
| P-06b | 200 | 222.88 | 240.32 | 245.9 | p95 ≤ 300 мс | в цели |
| P-13 | 1738 | 24 | 56 | 72 | p95 ≤ 100 мс (смоук) | в цели |
| P-14 | 8 | 103 | 135.45 | 144.69 | нет задач ≥ 500 мс (смоук) | в цели |

Вне цели в этом прогоне: P-01, P-02, P-04. Это первые числа на эмулированном телефоне при дросселе процессора 4× и общей нагрузке хоста; они не блокируют этап и служат точкой отсчёта.

## Подробно

### P-01: нажатие «Отправить» до локального элемента в полосе и свободного поля ввода

- Цель: p95 ≤ 100 мс; результат: вне цели (первый замер, не блокирует).
- Образцов: 200; длительность прогона 143.5 с.
- Мин 82 мс, p50 113.35 мс, p95 145.52 мс, p99 359.4 мс, макс 394.6 мс.
- Условия:
  - `platform`: linux x64
  - `cpus`: 36
  - `loadavg_1m_at_end`: 4.53
  - `loadavg_5m_at_end`: 5.81
  - `memory_free_gb`: 25.31
  - `node_version`: v24.22.1
  - `cpu_throttle`: 4x (CDP Emulation.setCPUThrottlingRate)
  - `viewport`: 412x892, touch, Chromium
  - `browser`: chromium (project phone)
  - `sends`: 200
  - `interval_between_sends_ms`: ~120 + fill
  - `definition`: event.timeStamp щелчка до следующего rAF после появления элемента в полосе или ленте и очистки поля (проверка в rAF)

### P-02: нажатие «Отправить» до ответа 201 хаба при RTT 80 мс

- Цель: p95 ≤ 300 мс, p99 ≤ 800 мс; результат: вне цели (первый замер, не блокирует).
- Образцов: 200; длительность прогона 146.7 с.
- Мин 172.2 мс, p50 229.55 мс, p95 311.3 мс, p99 623.67 мс, макс 833.5 мс.
- Условия:
  - `platform`: linux x64
  - `cpus`: 36
  - `loadavg_1m_at_end`: 4.2
  - `loadavg_5m_at_end`: 5.05
  - `memory_free_gb`: 25.82
  - `node_version`: v24.22.1
  - `cpu_throttle`: 4x (CDP Emulation.setCPUThrottlingRate)
  - `viewport`: 412x892, touch, Chromium
  - `browser`: chromium (project phone)
  - `sends`: 200
  - `network_rtt`: 80 мс (кромка: по 40 мс в каждую сторону)
  - `definition`: event.timeStamp щелчка до responseEnd записи Resource Timing POST …/inputs (журнал операций, IndexedDB и HTTP)

### P-03: input_queued до квитанции воркера (on_machine или transmitting) по часам хаба

- Цель: p95 ≤ 200 мс; результат: в пределах цели.
- Образцов: 200; длительность прогона 45.2 с.
- Мин 23 мс, p50 27 мс, p95 44 мс, p99 47.01 мс, макс 49 мс.
- Условия:
  - `platform`: linux x64
  - `cpus`: 36
  - `loadavg_1m_at_end`: 5.37
  - `loadavg_5m_at_end`: 5.27
  - `memory_free_gb`: 25.48
  - `node_version`: v24.22.1
  - `inputs`: 200
  - `pairs_from_frame_log`: 200
  - `pairs_from_hub_events_after_reconnect`: 0
  - `inputs_with_explicit_on_machine`: 200
  - `receipt`: on_machine, иначе transmitting
  - `clock`: committed_at хаба для обоих событий (журнал кадров кромки)
  - `note`: верхняя оценка: квитанция идёт и от воркера к хабу

### P-04: committed_at_from сегмента до его отрисовки

- Цель: p95 ≤ 250 мс, p99 ≤ 800 мс; результат: вне цели (первый замер, не блокирует).
- Образцов: 4020; длительность прогона 108.4 с.
- Мин 56 мс, p50 111 мс, p95 312 мс, p99 368 мс, макс 547 мс.
- Условия:
  - `platform`: linux x64
  - `cpus`: 36
  - `loadavg_1m_at_end`: 6.52
  - `loadavg_5m_at_end`: 5.73
  - `memory_free_gb`: 24.57
  - `node_version`: v24.22.1
  - `cpu_throttle`: 4x (CDP Emulation.setCPUThrottlingRate)
  - `viewport`: 412x892, touch, Chromium
  - `browser`: chromium (project phone)
  - `source_deltas`: 10000
  - `stream_rate`: ~100 дельт/с (по три каждые 30 мс)
  - `delta_events_on_hub`: 10000
  - `ledger`: чисто (нет дыр и двойного учёта)
  - `definition`: committed_at_from старейшей склеенной дельты хаба против сырого Date.now() после двух rAF
  - `samples_cap`: пробы хранят не более 10 000 последних значений

### P-05: щелчок «Прервать» до первого interrupt_state_changed в странице

- Цель: p95 ≤ 300 мс; результат: в пределах цели.
- Образцов: 50; длительность прогона 56.0 с.
- Мин 63.9 мс, p50 100.25 мс, p95 186.82 мс, p99 233.56 мс, макс 246.5 мс.
- Условия:
  - `platform`: linux x64
  - `cpus`: 36
  - `loadavg_1m_at_end`: 4.36
  - `loadavg_5m_at_end`: 5.33
  - `memory_free_gb`: 25.94
  - `node_version`: v24.22.1
  - `cpu_throttle`: 4x (CDP Emulation.setCPUThrottlingRate)
  - `viewport`: 412x892, touch, Chromium
  - `browser`: chromium (project phone)
  - `interrupts`: 50
  - `long_running_script`: delta + hang
  - `definition`: верхняя оценка «принято обработчиком воркера»: первое видимое клиенту событие прогресса прерывания (Date.now() в странице)

### P-06a: GET /history?limit=100 на чате из 10 000 вызовов напрямую в хаб

- Цель: p95 ≤ 300 мс; результат: в пределах цели.
- Образцов: 200; длительность прогона 27.8 с.
- Мин 121.44 мс, p50 138.19 мс, p95 157.57 мс, p99 162.75 мс, макс 167.97 мс.
- Условия:
  - `platform`: linux x64
  - `cpus`: 36
  - `loadavg_1m_at_end`: 2.78
  - `loadavg_5m_at_end`: 4.93
  - `memory_free_gb`: 26.01
  - `node_version`: v24.22.1
  - `requests`: 200
  - `items_in_chat`: 10 000 вызовов инструментов (два ввода по 5 000 шагов tool)
  - `network`: loopback, без кромки

### P-06b: GET /history?limit=100 через кромку с RTT 80 мс

- Цель: p95 ≤ 300 мс; результат: в пределах цели.
- Образцов: 200; длительность прогона 44.6 с.
- Мин 202.78 мс, p50 222.88 мс, p95 240.32 мс, p99 245.9 мс, макс 273.79 мс.
- Условия:
  - `platform`: linux x64
  - `cpus`: 36
  - `loadavg_1m_at_end`: 2.1
  - `loadavg_5m_at_end`: 4.44
  - `memory_free_gb`: 25.62
  - `node_version`: v24.22.1
  - `requests`: 200
  - `items_in_chat`: 10 000 вызовов инструментов
  - `network_rtt`: 80 мс (по 40 мс в каждую сторону на кромке)

### P-13: Event Timing нажатий клавиш при наборе во время потока в длинном чате

- Цель: p95 ≤ 100 мс (смоук); результат: в пределах цели.
- Образцов: 1738; длительность прогона 300.3 с.
- Мин 16 мс, p50 24 мс, p95 56 мс, p99 72 мс, макс 136 мс.
- Условия:
  - `platform`: linux x64
  - `cpus`: 36
  - `loadavg_1m_at_end`: 12.15
  - `loadavg_5m_at_end`: 8.55
  - `memory_free_gb`: 25.48
  - `node_version`: v24.22.1
  - `cpu_throttle`: 4x (CDP Emulation.setCPUThrottlingRate)
  - `viewport`: 412x892, touch, Chromium
  - `browser`: chromium (project phone)
  - `stream`: 3 000 дельт по 100 мс (5 минут) в чат из 10 000 вызовов инструментов
  - `keystrokes`: 1738
  - `keydown_events_over_16ms`: 1738
  - `definition`: duration записей keydown (Event Timing, порог 16 мс; события быстрее порога учтены как 8 мс)

### P-14: длинные задачи основного потока при том же сценарии

- Цель: нет задач ≥ 500 мс (смоук); результат: в пределах цели.
- Образцов: 8; длительность прогона 300.3 с.
- Мин 55 мс, p50 103 мс, p95 135.45 мс, p99 144.69 мс, макс 147 мс.
- Условия:
  - `platform`: linux x64
  - `cpus`: 36
  - `loadavg_1m_at_end`: 12.15
  - `loadavg_5m_at_end`: 8.55
  - `memory_free_gb`: 25.48
  - `node_version`: v24.22.1
  - `cpu_throttle`: 4x (CDP Emulation.setCPUThrottlingRate)
  - `viewport`: 412x892, touch, Chromium
  - `browser`: chromium (project phone)
  - `stream`: 3 000 дельт по 100 мс (5 минут) в чат из 10 000 вызовов инструментов
  - `keystrokes`: 1738
  - `keydown_events_over_16ms`: 1738
  - `long_tasks_ge_50ms`: 8
  - `long_tasks_ge_200ms`: 0
  - `long_tasks_ge_500ms`: 0
