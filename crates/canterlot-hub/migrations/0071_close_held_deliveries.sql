-- Терминальный ввод больше не держит свою выдачу: текст уже дошёл до агента
-- вливанием или отменён владельцем. Команда завершается тем же итогом, что
-- ставит обычную завершённую выдачу, и перестаёт выглядеть ждущей отправки.
-- Повтор безопасен: трогаются только удержанные выдачи терминальных вводов.
update commands c
set delivery_state = 'completed',
    completed_at = coalesce(c.completed_at, now()),
    last_status = case i.state
        when 'cancelled' then 'cancel_won'
        when 'rejected' then 'explicit_rejection'
        else 'accepted'
    end,
    last_status_rank = 3,
    result = case i.state
        when 'cancelled' then jsonb_build_object('cancelled', true)
        else jsonb_build_object('superseded', i.state)
    end
from inputs i
where i.command_id = c.id
  and c.kind = 'deliver_input'
  and c.delivery_state = 'held'
  and c.completed_at is null
  and i.state in ('accepted', 'cancelled', 'rejected');
