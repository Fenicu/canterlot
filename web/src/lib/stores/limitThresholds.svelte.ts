import { DEFAULT_LIMIT_THRESHOLDS, type LimitThresholds } from "./limits.js";

/**
 * Действующие пороги лимитов, общие для всех экранов. `loadConfig` обновляет их после
 * ответа хаба, а настройки — сразу после сохранения, поэтому полосы в уже смонтированных
 * списках перекрашиваются без перезагрузки.
 */
export const limitThresholds = $state<LimitThresholds>({ ...DEFAULT_LIMIT_THRESHOLDS });

export function setLimitThresholds(next: LimitThresholds): void {
  Object.assign(limitThresholds, next);
}
