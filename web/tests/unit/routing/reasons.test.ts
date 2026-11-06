import { describe, expect, test } from 'vitest';
import { detailText, queueReasonLabel, reasonText, refusalText, utcClock, verdictLabel } from '#lib/routing/reasons.js';
import { asDecision, routeLine, skippedText } from '#lib/routing/decision.js';

const NOW = Date.UTC(2026, 10, 6, 12, 0);
const N = ' ';

describe('reason translation', () => {
  test('limit over threshold names provider, window and both numbers', () => {
    expect(reasonText('codex weekly 83% > 80%', NOW)).toBe(`Codex — неделя 83${N}% при пороге 80${N}%`);
    expect(reasonText('claude five_hour 96.5% > 95%', NOW)).toBe(`Claude — 5 часов 96,5${N}% при пороге 95${N}%`);
    expect(reasonText('opencode monthly 99% > 95%', NOW)).toBe(`OpenCode — месяц 99${N}% при пороге 95${N}%`);
  });

  test('quota pause converts the hub UTC clock to local time', () => {
    expect(reasonText('quota paused until 18:40 UTC', NOW)).toBe(`квота на паузе до ${utcClock('18:40', NOW)}`);
    expect(reasonText('quota paused', NOW)).toBe('квота на паузе');
  });

  test('busy pairs, whitelist, catalog and workdir', () => {
    expect(reasonText('2 of 2 opencode-go/muse-spark-1.3-contributor sessions busy')).toBe('занято 2 из 2 сессий muse-spark-1.3-contributor');
    expect(reasonText("not in the owner's list")).toBe('не в белом списке владельца');
    expect(reasonText('not available on this machine')).toBe('нет на этой машине');
    expect(reasonText('excluded for workdir /srv/prod')).toBe('исключена для каталога /srv/prod');
  });

  test('unknown limits with and without the observation age', () => {
    expect(reasonText('codex weekly limits unknown (observed 2 h ago)')).toBe(`лимиты Codex (неделя) неизвестны: последнее наблюдение 2${N}ч назад`);
    expect(reasonText('claude five_hour limits unknown')).toBe('лимиты Claude (5 часов) неизвестны');
    expect(reasonText('limits unknown')).toBe('лимиты неизвестны');
  });

  test('levels, effort and permission mode', () => {
    expect(reasonText('level xhigh not in candidate levels')).toBe('уровень xhigh не входит в уровни кандидата');
    expect(reasonText('effort Some("max") not available for model opus')).toBe('усилие max недоступно для модели opus');
    expect(reasonText('permission mode plan not available for opencode')).toBe('режим разрешений plan недоступен для opencode');
  });

  test('unknown reasons stay as they are', () => {
    expect(reasonText('something brand new happened')).toBe('something brand new happened');
    expect(reasonText('')).toBe('');
    expect(reasonText(null)).toBe('');
  });

  test('queue detail: rate, guard parts and cleanup candidates', () => {
    expect(detailText('10 spawns in the last hour; the next slot opens at 14:32 UTC', NOW)).toBe(
      `10 запусков за час, следующий в ${utcClock('14:32', NOW)}`
    );
    expect(detailText('disk 12 GB free < 30 GB; load 2.1 > 1.5 per core.')).toBe(
      `диск: свободно 12${N}ГБ при минимуме 30${N}ГБ; нагрузка 2,1 на ядро при пороге 1,5`
    );
    expect(
      detailText('disk 12 GB free < 30 GB. T3 idle 3 h, T4 stopped 5 h: session_accept frees their worktrees and target dirs')
    ).toBe(`диск: свободно 12${N}ГБ при минимуме 30${N}ГБ. Место освободит приём результата: «T3» простаивает 3${N}ч, «T4» остановлена 5${N}ч`);
    expect(detailText('memory 2 GB available < 4 GB; disk 4% free < 10%')).toBe(
      `память: доступно 2${N}ГБ при минимуме 4${N}ГБ; диск: свободно 4${N}% при минимуме 10${N}%`
    );
  });

  test('start-now refusal names the code in Russian and falls back to the message', () => {
    expect(refusalText('not_in_whitelist', "This pair is not in the owner's spawn whitelist")).toBe(
      'не в белом списке владельца'
    );
    expect(refusalText('machine_unavailable', 'The machine is not available')).toBe('машина недоступна');
    expect(refusalText('brand_new_code', 'Новое объяснение')).toBe('Новое объяснение');
    expect(refusalText('brand_new_code', '')).toBe('');
  });

  test('queue reasons and verdicts have Russian labels', () => {
    expect(queueReasonLabel('rate')).toBe('частота запусков');
    expect(queueReasonLabel('resources')).toBe('машина занята');
    expect(queueReasonLabel('weird')).toBe('weird');
    expect(verdictLabel('chosen')).toBe('выбран');
    expect(verdictLabel('parallel_busy')).toBe('занят');
  });
});

describe('route line', () => {
  const decision = asDecision({
    policy_revision: 3,
    task_type: 'bugfix',
    harness: 'opencode',
    model: 'opencode-go/deepseek-v4.1-flash',
    effort: 'high',
    considered: [
      { harness: 'codex', model: 'default', verdict: 'limits_over', reason: 'codex weekly 83% > 80%' },
      { harness: 'opencode', model: 'opencode-go/deepseek-v4.1-flash', verdict: 'chosen', reason: '' }
    ]
  })!;

  test('names the route, the chosen pair and skipped candidates', () => {
    const titles = (harness: string, model: string) => (model.includes('deepseek') ? 'DeepSeek' : undefined);
    expect(routeLine(decision, 'Исправление', titles)).toBe(`Маршрут «Исправление»: DeepSeek · высокое. Пропущен: Codex — неделя 83${N}% при пороге 80${N}%`);
  });

  test('falls back to the task key and model id', () => {
    expect(routeLine({ ...decision, considered: [] }, '')).toBe('Маршрут «bugfix»: deepseek-v4.1-flash · высокое');
  });

  test('skipped text names the provider for limits and the pair otherwise', () => {
    expect(skippedText({ harness: 'claude_code', model: 'opus', verdict: 'limits_over', reason: 'claude weekly 91% > 90%' })).toBe(
      `Claude — неделя 91${N}% при пороге 90${N}%`
    );
    expect(skippedText({ harness: 'claude_code', model: 'opus', verdict: 'skipped', reason: "not in the owner's list" })).toBe(
      'opus — не в белом списке владельца'
    );
  });

  test('rejects values that are not decisions', () => {
    expect(asDecision(null)).toBeNull();
    expect(asDecision({ task_type: 1 })).toBeNull();
  });
});
