-- Устройство помнит сеанс своего последнего входа: отзыв этого сеанса владельцем,
-- сервером, сменой пароля или каскадом забывает и устройство.
alter table owner_devices
  add column session_id uuid references owner_sessions(id) on delete set null;
create index owner_devices_session on owner_devices(session_id) where session_id is not null;
