const KEY = "canterlot.device_id";

/** Случайный идентификатор установки: отличает эхо своей записи от чужого устройства. */
export function getDeviceId(storage: Storage = localStorage): string {
  const saved = storage.getItem(KEY);
  if (saved) return saved;
  const id = crypto.randomUUID();
  storage.setItem(KEY, id);
  return id;
}
