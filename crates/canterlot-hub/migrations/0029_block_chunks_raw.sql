-- U+0000 не хранится в text, а сумма воркера покрывает исходный текст дельт.
alter table block_chunks add column raw bytea;
