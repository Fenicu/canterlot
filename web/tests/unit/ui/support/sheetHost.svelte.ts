/** Хозяин двух слоёв: лист со своим ключом истории и подтверждение поверх него. */
export function sheetHost() {
  let details = $state(false);
  let confirm = $state(false);
  const log = $state<string[]>([]);
  return {
    log,
    get details() {
      return details;
    },
    set details(next: boolean) {
      details = next;
    },
    get confirm() {
      return confirm;
    },
    set confirm(next: boolean) {
      confirm = next;
    }
  };
}

export type SheetHost = ReturnType<typeof sheetHost>;
