-- Служебная строка без хода: ввод сразу терминальный, живёт в ленте и уходит
-- агенту приставкой к следующему доставленному вводу (`note_attached_to`).
alter table inputs add column service_note boolean not null default false;
alter table inputs add column note_attached_to uuid references inputs(id) on delete set null;
create index inputs_service_notes on inputs(conversation_id, input_seq)
  where service_note and note_attached_to is null;
create index inputs_note_attached on inputs(note_attached_to)
  where note_attached_to is not null;
