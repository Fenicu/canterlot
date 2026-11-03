-- Скорость ответа читает блоки хода по команде ввода, в том числе за неделю на странице лимитов.
create index blocks_input_command on blocks (input_command_id) where input_command_id is not null;
