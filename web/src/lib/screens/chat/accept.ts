// Приём результата делегированной сессии: что показывать по `spawn_depth` и состоянию рабочей копии.
export interface AcceptDetail {
  parent_id?: string | null;
  // Родителя могут удалить, а сессия остаётся делегированной: признак дают только глубина и хаб.
  spawn_depth?: number;
  archived?: boolean;
  worktree?: { branch?: string; state?: string; path?: string } | null;
}

export const ACCEPT_LABEL = 'Принять результат и убрать';
export const FORCE_LABEL = 'Убрать всё равно';
export const DIRTY_NOTE = 'В рабочей копии есть незакоммиченные правки';
const CONFIRM_WITH_COPY = 'Сессия остановится, рабочая копия будет удалена. Ветка с коммитами останется.';
const CONFIRM_NO_COPY = 'Сессия остановится.';

export interface AcceptView {
  /** Действие есть только у сессии, запущенной агентом, пока она не принята. */
  available: boolean;
  dirty: boolean;
  /** Идёт уборка: повторно просить не нужно. */
  removing: boolean;
  body: string;
  confirmLabel: string;
  force: boolean;
}

export function acceptView(detail: AcceptDetail | null | undefined): AcceptView {
  const state = detail?.worktree?.state;
  const dirty = state === 'dirty';
  const removing = state === 'removing';
  const hasCopy = Boolean(detail?.worktree) && state !== 'removed';
  const confirm = hasCopy ? CONFIRM_WITH_COPY : CONFIRM_NO_COPY;
  return {
    available: (detail?.spawn_depth ?? 0) > 0 && !detail?.archived && state !== 'removed' && !removing,
    dirty,
    removing,
    body: dirty ? `${DIRTY_NOTE}. ${confirm}` : confirm,
    confirmLabel: dirty ? FORCE_LABEL : ACCEPT_LABEL,
    force: dirty
  };
}
