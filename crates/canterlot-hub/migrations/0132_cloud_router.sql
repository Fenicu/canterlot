-- Облачные окна через приватную сеть и роутер (этап C2c): публичный IPv4 больше не
-- заказывается. Сервер живёт в сети с локальным адресом и выходит в интернет через NAT
-- роутера; носитель подключается по DNAT-правилу `router_ip:dnat_port -> local_ip:22`.
-- Поля заказа адреса удаляются: адрес, который нельзя заказать, не должен тарифицироваться
-- и не оставляет обязательств по удалению плавающего адреса.

alter table cloud_windows
    add column local_ip text,
    add column dnat_port integer,
    add column dnat_rule_id text,
    -- Метка вызова создания правила: до неё повтор возможен, после — только усыновление
    -- из полного списка (второго POST для одного и того же порта не бывает).
    add column dnat_create_attempted_at timestamptz,
    add column dnat_delete_accepted_at timestamptz,
    add column dnat_gone_at timestamptz,
    add column dnat_delete_attempts integer not null default 0,
    add column next_dnat_delete_at timestamptz;

alter table cloud_windows
    add constraint cloud_windows_dnat_port_check
    check (dnat_port is null or (dnat_port between 1024 and 65535));

alter table cloud_windows
    drop column ipv4,
    drop column ipv4_hourly_kopecks,
    drop column ip_requested_at,
    drop column floating_ip_id,
    drop column ip_delete_accepted_at,
    drop column ip_gone_at,
    drop column ip_delete_attempts;

-- Блок провайдера теперь ставится и при отказе `no_paid`: сервер не включается, пока аккаунт
-- не переведён на предоплату. Снимает блок по-прежнему только владелец.

-- Открытое DNAT-правило — такое же обязательство, как неудалённый сервер: оно не снимается,
-- пока не подтверждено его отсутствие в полном списке правил роутера.
comment on column cloud_windows.dnat_rule_id is
    'Идентификатор правила DNAT роутера; удаляется после подтверждённого удаления сервера';
