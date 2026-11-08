-- Поиск по истории (pilot.md §12.1 п. 6): ответы агента и сообщения ленты.
-- Документ строится выражением, без хранимого tsvector и отдельной таблицы: индекс сам
-- следует за закрытием блока, вычисткой отправителя и удалением строк.
-- Первые 32768 символов: `to_tsvector` отказывает на документе больше 1 МБ, и такой отказ
-- внутри выражения индекса уронил бы закрытие блока.
-- Тела SQL-стандарта связываются при создании и не зависят от `search_path`, который
-- обслуживание индекса сужает. Запросы зовут эти же функции, иначе индекс не подходит.

create function canterlot_block_doc(content text) returns tsvector
    language sql immutable strict parallel safe
    return pg_catalog.to_tsvector('pg_catalog.russian'::pg_catalog.regconfig,
                                  pg_catalog.left(content, 32768));

-- Текстовые части ввода через строку; lax jsonpath не падает на содержимом другой формы.
create function canterlot_input_doc(content jsonb) returns tsvector
    language sql immutable strict parallel safe
    return pg_catalog.to_tsvector('pg_catalog.russian'::pg_catalog.regconfig,
        pg_catalog.left(pg_catalog.array_to_string(array(
            select pg_catalog.jsonb_array_elements_text(pg_catalog.jsonb_path_query_array(
                content, 'lax $[*] ? (@.type == "text").text'))), E'\n'), 32768));

-- Один хаб и ни одного писателя во время миграций: обычное построение занимает секунды.
create index blocks_search on blocks using gin (canterlot_block_doc(content))
    where kind = 'text' and state <> 'streaming';
create index inputs_search on inputs using gin (canterlot_input_doc(content))
    where feed_seq is not null;
