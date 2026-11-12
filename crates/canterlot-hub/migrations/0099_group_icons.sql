-- Значок группы для полосы групп и превью последнего ответа агента в строке списка.
alter table conversation_groups add column icon text;
alter table conversation_groups add constraint conversation_groups_icon check (icon is null or icon in (
  'folder', 'briefcase', 'home', 'star', 'code', 'rocket', 'book', 'flask',
  'wrench', 'server', 'globe', 'heart', 'user', 'users', 'gamepad', 'music'));
-- Превью считается при конце хода; отметка хода говорит, для какого хода оно посчитано.
alter table conversation_derived add column preview text, add column preview_turn_at timestamptz;
