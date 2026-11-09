/** Поля листа настроек сессии, которые владелец меняет до нажатия «Сохранить». */
export interface SettingsFormValues {
  model: string;
  effort: string;
  permission: string;
  preset: string;
  crunchEnabled: boolean;
  resumeText: string;
}

type Field = keyof SettingsFormValues;

const SETTINGS_FIELDS: readonly Field[] = ["model", "effort", "permission", "preset"];
const CRUNCH_FIELDS: readonly Field[] = ["crunchEnabled", "resumeText"];

/**
 * Сводит поля формы с хабом. Снимок после переподключения переприсваивает настройки теми же
 * значениями, поэтому поля настроек обновляются только при открытии листа и новой ревизии,
 * поля кранча — при его настоящей смене. Поле, которое владелец уже изменил, не перезаписывается.
 */
export class SettingsFormSync {
  /** Что форма последний раз взяла у хаба (или отправила): отличие от него и есть правка владельца. */
  #synced: SettingsFormValues | null = null;
  #rev: number | null = null;
  #crunchKey: string | null = null;

  /** Значения для формы или `null`, если хаб не сообщил ничего нового. */
  sync(input: {
    rev: number;
    crunchKey: string;
    source: SettingsFormValues;
    current: SettingsFormValues;
  }): SettingsFormValues | null {
    const first = this.#synced === null;
    const settingsChanged = first || input.rev !== this.#rev;
    const crunchChanged = first || input.crunchKey !== this.#crunchKey;
    if (!settingsChanged && !crunchChanged) return null;
    this.#rev = input.rev;
    this.#crunchKey = input.crunchKey;
    const synced: SettingsFormValues = { ...(this.#synced ?? input.source) };
    const next: SettingsFormValues = { ...input.current };
    const take = (fields: readonly Field[]): void => {
      for (const field of fields) {
        const touched = !first && input.current[field] !== synced[field];
        if (!touched) (next as unknown as Record<Field, unknown>)[field] = input.source[field];
        (synced as unknown as Record<Field, unknown>)[field] = input.source[field];
      }
    };
    if (settingsChanged) take(SETTINGS_FIELDS);
    if (crunchChanged) take(CRUNCH_FIELDS);
    this.#synced = synced;
    return next;
  }

  /** Значения ушли на хаб: дальше поля снова следуют за ним. */
  saved(current: SettingsFormValues): void {
    this.#synced = { ...current };
  }
}
