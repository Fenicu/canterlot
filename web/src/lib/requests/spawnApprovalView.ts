import { modelLabel, permissionLabel } from '#lib/format/labels.js';

export interface SpawnApprovalView {
  facts: { label: string; value: string }[];
  brief: string;
}

const HARNESS_TITLES: Record<string, string> = {
  claude_code: 'Claude Code',
  claude: 'Claude Code',
  codex: 'Codex',
  opencode: 'OpenCode'
};

function text(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

/** Поля `request_payload` карточки одобрения: запуск, уже проверенный хабом. */
export function spawnApprovalView(payload: unknown): SpawnApprovalView {
  const top = (payload && typeof payload === 'object' ? payload : {}) as Record<string, unknown>;
  const harness = text(top.harness);
  const model = text(top.model);
  const base = text(top.base);
  const facts = [
    { label: 'Харнесс', value: HARNESS_TITLES[harness] ?? harness },
    { label: 'Модель', value: model ? modelLabel(model) : '' },
    { label: 'Режим', value: text(top.permission_mode) ? permissionLabel(text(top.permission_mode)) : '' },
    { label: 'Каталог', value: text(top.workdir) },
    { label: 'Отдельная рабочая копия', value: top.isolated === true ? (base ? `да, от ${base}` : 'да') : 'нет' }
  ];
  return { facts: facts.filter((fact) => fact.value), brief: text(top.brief) };
}
