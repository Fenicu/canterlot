-- «Владелец ждёт ответа»: отметка ставится вводом владельца и снимается одним
-- пушем «Всё готово», когда сессия осела и у неё не осталось работы делегатов.
alter table conversations add column owner_waiting_since timestamptz;
