/** Подписи значений протокола: хранимые значения не меняются, сырая строка на экран не попадает. */

type Value = string | null | undefined;

function lookup(map: Record<string, string>, value: Value): string | undefined {
  return value && Object.hasOwn(map, value) ? map[value] : undefined;
}

export const PERMISSION_MODES = ["owner", "ask", "bypass", "read_only"] as const;

/** Новая сессия запускается без подтверждений: решение владельца. */
export const DEFAULT_PERMISSION_MODE = "bypass";

const PERMISSION_LABELS: Record<string, string> = {
  owner: "Как в настройках CLI",
  ask: "Спрашивать",
  bypass: "Без подтверждений",
  read_only: "Только чтение",
};

/** Пустой режим хранят сессии, созданные до выбора режима в форме: хаб запускает их как `owner`. */
export function permissionValue(value: Value): string {
  return value || "owner";
}

export function permissionLabel(value: Value): string {
  return lookup(PERMISSION_LABELS, permissionValue(value)) ?? "Другой режим";
}

const PERMISSION_HINTS: Record<string, Record<string, string>> = {
  claude: {
    owner:
      "Режим не передаётся: Claude Code берёт режим по умолчанию из своих настроек, ~/.claude/settings.json. В режиме «не спрашивать» (dontAsk) работает только список разрешённых действий, остальное отклоняется молча, без карточки.",
    ask: "Claude Code спрашивает о действиях вне списка разрешённых: решение приходит карточкой.",
    bypass: "Claude Code выполняет всё без подтверждений, карточек разрешений не будет.",
    read_only: "Режим plan: Claude Code читает и планирует, файлы не меняет.",
  },
  codex: {
    owner:
      "Режим не передаётся: Codex берёт approval_policy и sandbox_mode из своей конфигурации, ~/.codex/config.toml.",
    ask: "Codex спрашивает перед действиями: решение приходит карточкой, песочницы нет.",
    bypass: "Codex выполняет всё без подтверждений и без песочницы.",
    read_only: "Codex работает в песочнице только для чтения и ни о чём не спрашивает.",
  },
  opencode: {
    owner:
      "Режим не передаётся: OpenCode берёт правила из своей конфигурации, opencode.json. По умолчанию это выполнение без вопросов.",
    ask: "OpenCode спрашивает перед каждым действием: решение приходит карточкой.",
    bypass: "OpenCode выполняет всё без подтверждений.",
    read_only:
      "OpenCode читает и ищет, а запись и команды модели не предлагаются.",
  },
  antigravity: {
    owner:
      "Режим не передаётся: Antigravity берёт правила из настроек владельца в ~/.gemini. Часть инструментов разрешена там без запроса.",
    ask: "Antigravity спрашивает о действиях вне правил владельца: решение приходит карточкой.",
    bypass: "Antigravity выполняет всё без подтверждений, карточек разрешений не будет.",
  },
};

/** Antigravity работает на каталоге владельца: форма предупреждает об общей истории и MCP. */
export const ANTIGRAVITY_CATALOG_NOTE =
  "Сессия использует ~/.gemini владельца: настройки, MCP-серверы и история общие с его Antigravity.";

const PRESET_CHANGE_NOTE =
  "Смена инструкций перезапускает агента между ходами с сохранением истории. Пока идёт фоновая работа, смена ждёт её конца: задачи не останавливаются.";
const PRESET_CHANGE_NOTE_OPENCODE =
  "Смена инструкций действует со следующего хода без перезапуска, фоновые задачи продолжат работать. Переход на «Как у владельца» и обратно перезапускает агента и останавливает их.";

/** Хаб принимает картинку для любой сессии OpenCode: модель без такого ввода получает путь. */
export function modelImagesNote(images: boolean | null | undefined): string {
  return images === false
    ? "Модель не принимает картинки: вложение придёт ей путём к файлу."
    : "";
}

export function presetChangeNote(provider: Value): string {
  return provider === "opencode" ? PRESET_CHANGE_NOTE_OPENCODE : PRESET_CHANGE_NOTE;
}

export function permissionHint(value: Value, provider: Value): string {
  const family =
    provider === "codex" || provider === "opencode" || provider === "antigravity"
      ? provider
      : "claude";
  return lookup(PERMISSION_HINTS[family] ?? {}, permissionValue(value)) ?? "";
}

/** Каталог машины пуст: у агента нет ни одной доступной модели. */
export const NO_MODELS_HINT =
  "Этому агенту на машине недоступна ни одна модель: проверьте его вход и список провайдеров.";

const EFFORT_LABELS: Record<string, string> = {
  default: "По умолчанию",
  none: "Без рассуждений",
  minimal: "Минимальное",
  low: "Низкое",
  medium: "Обычное",
  high: "Высокое",
  xhigh: "Очень высокое",
  max: "Максимальное",
};

export function effortLabel(value: Value): string {
  return lookup(EFFORT_LABELS, value) ?? "Другое усилие";
}

const EXECUTION_STATES: Record<string, string> = {
  idle: "Не запущено",
  live: "Работает",
  created: "Создаётся",
  starting: "Запускается",
  running: "Работает",
  stopping: "Останавливается",
  stopped: "Остановлено",
  failed: "Сбой",
  owner_lost: "Связь с исполнением потеряна",
};

export function executionStateLabel(value: Value): string {
  return lookup(EXECUTION_STATES, value) ?? "Состояние неизвестно";
}

const FAILURES: Record<string, string> = {
  owner_lost: "потерян владелец исполнения",
  context_exhausted: "исчерпан контекст",
  crashed: "процесс завершился с ошибкой",
  start_failed: "не удалось запустить",
  codex_state_backfill_stuck:
    "застряло обновление базы Codex: перенесите state_5.sqlite из домашнего каталога canterlot и запустите снова",
  protocol_incompatible: "несовместимый протокол агента",
  start_rejected: "запуск отклонён машиной",
  worktree_not_git: "исходная папка не репозиторий git",
  worktree_base_unknown: "неизвестная база для рабочей копии",
  worktree_failed: "не удалось подготовить рабочую копию",
  cli_incompatible: "несовместимая версия CLI",
  auth_required: "агенту нужна авторизация",
  storage_failure: "ошибка хранилища на машине",
  supervisor_died: "процесс сопровождения завершился",
  admin_resolved: "закрыто администратором",
  invalid_instructions: "инструкции не подошли агенту",
  session_stuck: "один и тот же отказ провайдера подряд",
};

/** Причина сбоя строчными: подставляется после «Сбой:» и в текст карточки. */
export function failureLabel(value: Value): string {
  return lookup(FAILURES, value) ?? "причина неизвестна";
}

const COMPACTION: Record<string, string> = {
  requested: "Контекст сжимается",
  failed: "Сжатие не удалось",
};

/** Ход сжатия контекста на карточке застрявшей сессии. */
export function compactionLabel(value: Value): string | undefined {
  return lookup(COMPACTION, value);
}

/** Сводка фазы при повторе запроса: `retry <попытка>` от адаптера, повторяет сам агент. */
export function retryLabel(summary: Value): string | null {
  const attempt = typeof summary === "string" ? /^retry (\d+)$/.exec(summary) : null;
  return attempt ? `Повторяет запрос к модели, попытка ${attempt[1]}` : null;
}

const OUTCOMES: Record<string, string> = {
  user_stop: "Остановлено вами",
  force_stopped: "Остановлено принудительно",
  normal_exit: "Завершено штатно",
  start_cancelled: "Запуск отменён",
};

export function executionOutcomeLabel(value: Value): string {
  const known = lookup(OUTCOMES, value);
  if (known) return known;
  const failure = lookup(FAILURES, value);
  return failure ? capitalize(failure) : "Итог неизвестен";
}

const SCOPE_STATUSES: Record<string, string> = {
  idle: "Не подключено",
  loading_snapshot: "Загружается",
  subscribing: "Подключается",
  live: "На связи",
  gap_recovery: "Восстанавливает пропуск",
  reconnecting: "Переподключается",
  forbidden: "Нет доступа",
  deleted: "Сессия удалена",
  incompatible: "Нужно обновить приложение",
  degraded: "Синхронизация не удаётся",
};

export function scopeStatusLabel(value: Value): string {
  return lookup(SCOPE_STATUSES, value) ?? "Состояние неизвестно";
}

const DIAGNOSTIC_KINDS: Record<string, string> = {
  limit_reached: "Достигнут лимит",
  block_conflicting_final: "Расхождение итогового текста",
  adapter_diagnostic: "Сообщение адаптера",
  state_divergence: "Расхождение состояния",
  block_revision_gap: "Пропуск в потоке блока",
  block_checksum_mismatch: "Контрольная сумма блока не совпала",
};

export function diagnosticKindLabel(value: Value): string {
  return lookup(DIAGNOSTIC_KINDS, value) ?? "Служебное событие";
}

const DIAGNOSTIC_EVENT_TYPES: Record<string, string> = {
  codex_state_backfill: "Codex обновляет свою базу",
  codex_state_backfill_stuck: "застряло обновление базы Codex",
};

/** Причина события адаптера по-русски; неизвестную показываем как есть. */
export function diagnosticEventLabel(value: Value): string | undefined {
  return lookup(DIAGNOSTIC_EVENT_TYPES, value);
}

const BLOCK_ENDS: Record<string, string> = {
  interrupted: "Прервано",
  stopped: "Исполнение остановлено",
  process_exited: "Процесс агента завершился",
  machine_rebooted: "Машина перезагружена",
};

export function blockEndLabel(value: Value): string {
  const known = lookup(BLOCK_ENDS, value);
  if (known) return known;
  const failure = lookup(FAILURES, value);
  return failure ? capitalize(failure) : "Ответ оборвался";
}

const REJECTIONS: Record<string, string> = {
  generation_closed: "Исполнение уже завершено",
  execution_stopped: "Исполнение остановлено",
  execution_terminal: "Исполнение завершилось",
  too_late: "Сообщение уже передано агенту",
  transmission_started: "Сообщение уже передаётся агенту",
  already_transmitted: "Сообщение уже передано агенту",
  undetermined: "Приём не подтверждён",
  unsupported_command: "Машина не поддерживает это действие",
  capability_missing: "Машина не поддерживает это действие",
  request_closed: "Запрос уже закрыт",
};

export function rejectionLabel(value: Value): string {
  return lookup(REJECTIONS, value) ?? "Сообщение не принято";
}

const SETTING_KEYS: Record<string, string> = {
  model: "модель",
  effort: "усилие",
  permission_mode: "режим разрешений",
  instruction_profile: "инструкции",
};

export function settingKeyLabel(value: Value): string {
  return lookup(SETTING_KEYS, value) ?? "настройка";
}

const AVAILABILITY: Record<string, string> = {
  available: "В сети",
  ready: "В сети",
  degraded: "Связь нестабильна",
  unavailable: "Не в сети",
};

export function availabilityLabel(value: Value): string {
  return lookup(AVAILABILITY, value) ?? "Не в сети";
}

const PROVIDERS: Record<string, string> = {
  claude: "Claude",
  codex: "Codex",
  opencode: "OpenCode",
  antigravity: "Antigravity",
};

export function providerLabel(value: Value): string {
  return lookup(PROVIDERS, value?.toLowerCase()) ?? "Другой провайдер";
}

function capitalize(text: string): string {
  return text.charAt(0).toLocaleUpperCase("ru-RU") + text.slice(1);
}

/** Название инструмента, через который идёт сессия: как в макете, без слова Code. */
export function harnessLabel(harness: { title?: string; provider?: string } | null | undefined): string {
  return lookup(PROVIDERS, harness?.provider?.toLowerCase()) ?? harness?.title ?? "Агент";
}

/**
 * Человеческое имя модели: `claude-fable-5-2` — «Fable 5.2», `opus[1m]` — «Opus 1M»,
 * `gpt-6.1-sol` — «GPT-6.1 Sol». У `провайдер/модель` остаётся модель как есть.
 */
export const modelLabel = (value: string): string => {
  const base = value.replace(/\[1m\]$/i, '');
  if (base !== value) return `${modelLabel(base)} 1M`;
  const claude = /^claude-([a-z]+)-(\d+)(?:-(\d{1,2}))?(?:-\d{8})?$/.exec(value);
  if (claude) {
    const [, family = '', major, minor] = claude;
    return `${capitalize(family)} ${minor ? `${major}.${minor}` : major}`;
  }
  const gpt = /^gpt-(\d+(?:\.\d+)?)((?:-[a-z0-9]+)*)$/.exec(value);
  if (gpt) {
    const [, version = '', rest = ''] = gpt;
    return [`GPT-${version}`, ...rest.split('-').filter(Boolean).map(capitalize)].join(' ');
  }
  if (/^[a-z]+$/.test(value)) return capitalize(value);
  return value.slice(value.indexOf('/') + 1);
};

/**
 * Модель сессии для шапки и строки: конкретная версия, если хаб её знает
 * (`fable[1m]` → «Fable 5.2 1M»), иначе имя псевдонима.
 */
export function resolvedModelLabel(model: string, resolved?: string | null): string {
  if (!resolved || resolved === model) return modelLabel(model);
  const wide = /\[1m\]$/i.test(model) && !/\[1m\]$/i.test(resolved);
  return `${modelLabel(resolved)}${wide ? ' 1M' : ''}`;
}

/** Подсказка по требованию: какой псевдоним во что разрешился, `fable[1m] → claude-fable-5-2`. */
export function modelAliasNote(model: string, resolved?: string | null): string {
  return resolved && resolved !== model ? `${model} → ${resolved}` : '';
}
