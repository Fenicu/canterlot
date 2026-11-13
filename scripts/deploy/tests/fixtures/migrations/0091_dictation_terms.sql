-- Словарь замен живой диктовки: владелец пишет как слышит, хаб подставляет термин.
-- Порядок задаёт `position`; список заменяется целиком одной транзакцией.
create table dictation_terms (
    owner_id uuid not null references owners(id) on delete cascade,
    position integer not null check (position >= 0),
    pattern text not null check (char_length(pattern) between 1 and 100),
    replacement text not null check (char_length(replacement) between 1 and 100),
    created_at timestamptz not null default clock_timestamp(),
    primary key (owner_id, position)
);
