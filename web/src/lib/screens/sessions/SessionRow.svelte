<script lang="ts">
  import { resolve } from '$app/paths';
  import { fmt } from '#lib/format/ru.js';
  import { modelAliasNote, resolvedModelLabel } from '#lib/format/labels.js';
  import Icon from '#lib/ui/Icon.svelte';
  import { timeTip } from '#lib/ui/time-tip.js';
  import type { ListRow } from '#lib/stores/list.svelte.js';
  import { colorVar } from './colors';
  import { stateMark } from './rowState';
  import type { Activity } from './activityText';
  import { getRuntime } from '#lib/app/runtime.js';
  import { planBadge } from '#lib/screens/plan/planText.js';
  import VendorAvatar from '#lib/vendors/VendorAvatar.svelte';
  import { VENDOR_TITLES, vendorOf } from '#lib/vendors/vendor.js';
  import { catalogTitle, onlineMachines, rowLine } from './rowText';
  import { snoozedUntil } from './snooze';

  let {
    item,
    now,
    current = false,
    depth = 0,
    copy = false,
    onopen
  }: {
    item: ListRow;
    now: number;
    current?: boolean;
    depth?: number;
    /** Копия в разделе «Ждут вас»: отдельная метка строки, без `data-session`. */
    copy?: boolean;
    onopen: (id: string, node?: HTMLElement | null) => void;
  } = $props();

  function runtime() {
    try {
      return getRuntime();
    } catch {
      return null;
    }
  }
  const machines = $derived(runtime()?.machines.items ?? null);
  const machine = $derived(machines?.get(item.machine.id) ?? null);
  const line = $derived(rowLine(item, now, { title: catalogTitle(item, machine), machines: machines ? onlineMachines(machines.values()) : 1 }));
  // Метка плана у оркестратора: сколько задач готово из всех неотменённых.
  const plan = $derived(planBadge(runtime()?.list.plan.of(item.id) ?? []));
  const vendor = $derived(vendorOf(item.harness.provider, item.model_resolved || item.model));
  // Знак на аватаре говорит только о состоянии процесса; цвет владельца — полоска у левого края строки.
  const mark = $derived(stateMark(item.activity as Activity));
  const owner = $derived(item.color ? colorVar(item.color) : null);
  const stampTs = $derived.by(() => {
    const parsed = item.last_activity_at ? Date.parse(item.last_activity_at) : Number.NaN;
    return Number.isFinite(parsed) ? parsed : null;
  });
  const stamp = $derived(stampTs !== null ? fmt.clock(stampTs) : '');
  const queued = $derived(item.queue?.queued ?? 0);
  const snoozedAt = $derived(snoozedUntil(item, now));
  const modelNote = $derived([item.harness.title, resolvedModelLabel(item.model, item.model_resolved), item.machine.name, modelAliasNote(item.model, item.model_resolved)].filter(Boolean).join(' · '));
  // Пробел в конце собирается в скрипте: Svelte срезает его у края элемента.
  const workedPrefix = 'работала ';
  let timer = 0;
  let held = false;

  function down(event: PointerEvent) {
    held = false;
    // Удержание заменяет контекстное меню только пальцу и перу: у мыши есть правая кнопка,
    // а медленный клик не должен открывать меню вместо чата.
    if (event.pointerType === 'mouse') return;
    const node = (event.currentTarget as HTMLElement | null)?.closest('article') ?? null;
    timer = window.setTimeout(() => {
      held = true;
      onopen(item.id, node);
    }, 500);
  }
  function up() {
    window.clearTimeout(timer);
  }
  function click(event: MouseEvent) {
    if (!held) return;
    event.preventDefault();
    held = false;
  }
  function open(event: Event) {
    const node = (event.currentTarget as HTMLElement | null)?.closest('article') ?? null;
    onopen(item.id, node);
  }
</script>

<article class="row" data-state={mark.state} class:dim={mark.dim} class:strong={mark.strong} class:current class:child={depth > 0} class:depth-1={depth === 1} class:depth-2={depth >= 2} data-session={copy ? undefined : item.id} data-waiting={copy ? item.id : undefined} data-vendor={vendor ?? 'letter'} oncontextmenu={(event) => { event.preventDefault(); open(event); }}>
  {#if owner}<span class="owner" data-owner-color={item.color} style:--owner="var({owner})" aria-hidden="true"></span>{/if}
  <a href={resolve(`/c/${item.id}` as '/')} aria-current={current ? 'page' : undefined} onpointerdown={down} onpointerup={up} onpointercancel={up} onclick={click}>
    <VendorAvatar {vendor} model={item.model_resolved || item.model} title={item.title} tone={mark.dot ?? 'none'} halo={mark.halo} dashed={mark.dashed} dim={mark.dim} size={depth > 0 ? 'small' : 'row'} />
    <span class="sr-only">{mark.word}</span>
    <span class="main">
      <span class="l1">
        <span class="title" title={item.title}>{item.title}</span>
        {#if stamp && stampTs !== null}
          <time datetime={item.last_activity_at ?? undefined} use:timeTip={stampTs}>{stamp}</time>
        {/if}
      </span>
      <span class="l2">
        <span class="activity" data-tone={line.tone} data-preview={line.preview ? '' : undefined}>
          <span class="sr-only">{vendor ? VENDOR_TITLES[vendor] : ''}</span>
          {#if line.live}<span class="live" aria-hidden="true"></span>{/if}<span class="text">{line.text}</span>
          {#if line.who}<span class="who" title={modelNote}>· {line.who}</span>{/if}
        </span>
        {#if depth > 0 && item.working_ms && !line.timed}
          <span class="worked" data-worked title={`работала ${fmt.durationCoarse(item.working_ms)}`}><span class="sr-only">{workedPrefix}</span>{fmt.durationCoarse(item.working_ms)}</span>
        {/if}
        {#if plan}
          <span class="plan" data-role="plan-badge"><span class="sr-only">{plan.label}</span><span aria-hidden="true">{plan.text}</span></span>
        {/if}
        {#if snoozedAt !== null}
          <span class="snoozed" data-snoozed><span class="sr-only">Отложена</span>до {fmt.clock(snoozedAt)}</span>
        {/if}
        {#if queued > 0}
          <span class="queued" data-queued><span class="sr-only">В очереди сообщений: {queued}</span><span aria-hidden="true">+{queued}</span></span>
        {/if}
        {#if item.pinned}
          <span class="pin" data-pinned><span class="sr-only">Закреплена</span><Icon name="pin" filled /></span>
        {/if}
        {#if item.unread_count > 1}
          <span class="sr-only">Непрочитанных: {item.unread_count}</span><span class="count" aria-hidden="true">{item.unread_count}</span>
        {:else if item.unread_count === 1}
          <span class="sr-only">Есть непрочитанное</span><span class="count unread" aria-hidden="true">1</span>
        {/if}
      </span>
    </span>
  </a>
  <button type="button" aria-label={`Действия: ${item.title}`} onpointerdown={(event) => event.stopPropagation()} onclick={open}>
    <Icon name="more" />
  </button>
</article>

<style>
  .row { --avatar-ring: var(--list-bg, var(--bg)); position: relative; display: flex; align-items: center; margin-inline: var(--s3); border-radius: var(--radius-sm); }
  .row.current { --avatar-ring: var(--selected); background: var(--selected); }
  @media (hover: hover) and (pointer: fine) {
    .row:not(.current):hover { --avatar-ring: var(--hover); background: var(--hover); }
    .row:not(.current):not(:hover):not(:focus-within) button { opacity: 0; }
  }
  a { flex: 1; min-width: 0; display: flex; align-items: center; gap: var(--s5); min-height: calc(var(--avatar) + var(--s6)); padding: var(--s3) var(--s0) var(--s3) var(--s5); border-radius: var(--radius-sm); color: inherit; }
  .main { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: var(--s1); }
  .l1, .l2 { display: flex; align-items: center; gap: var(--s3); min-width: 0; }
  .title { flex: 1; min-width: 0; overflow: hidden; font-weight: var(--weight-semibold); text-overflow: ellipsis; white-space: nowrap; }
  time { flex: none; color: var(--time); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .activity { flex: 1; min-width: 0; overflow: hidden; color: var(--muted); font-size: var(--fs-meta); white-space: nowrap; display: flex; align-items: baseline; gap: var(--s1); }
  /* Состояние занимает ровно свою ширину: короткий статус не добивается отступом до хвоста. */
  .text { flex: 0 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  /* Хвост «модель · машина» забирает остаток и сжимается первым. */
  .who { flex: 1 1 0; min-width: 0; overflow: hidden; color: var(--secondary); text-overflow: ellipsis; }
  .activity[data-tone='accent'] .text { color: var(--accent-link); }
  /* Связь пропала: живой акцент замирает вместе с часами, слово остаётся. */
  :global([data-stale]) .activity[data-tone='accent'] .text { color: var(--muted); }
  :global([data-stale]) .live { background: var(--muted); }
  .activity[data-tone='warning'] .text { color: var(--warning); }
  .activity[data-tone='danger'] .text { color: var(--danger); }
  .live { display: inline-block; width: var(--mark); height: var(--mark); margin-right: var(--s2); border-radius: var(--radius-round); background: var(--accent); vertical-align: middle; }
  /* Цвет владельца — тонкая полоска у левого края, чтобы не спорить с кольцом и точкой состояния. */
  .owner { white-space: nowrap; position: absolute; left: var(--s0); top: var(--s4); bottom: var(--s4); width: calc(var(--s1) * 1.5); border-radius: var(--radius-pill); background: var(--owner); }
  /* Процесса нет: строка гаснет и читается как архивная. */
  .row.dim .title { color: var(--secondary); font-weight: var(--weight-normal); }
  .row.dim .activity .text { color: var(--muted); }
  /* Ожидание владельца — самое важное состояние: слово жирное в любой палитре. */
  .row.strong .activity .text { font-weight: var(--weight-bold); }
  /* Подвисшая сессия — беда того же ряда, что сбой: красное кольцо и красные слова. */
  .row[data-state='stalled'] .activity .text { color: var(--danger); }
  /* Усыплена: спокойный холодный тон вместо серой «Остановлена», без значка. */
  .row[data-state='hibernated'] .activity .text { color: var(--blue); }
  .snoozed { flex: none; color: var(--secondary); font-size: var(--fs-xs); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .plan, .queued { flex: none; max-width: 50%; padding: var(--s0) var(--s3); overflow: hidden; border-radius: var(--radius-pill); background: var(--chip); color: var(--chip-text); font-size: var(--fs-xs); font-variant-numeric: tabular-nums; text-overflow: ellipsis; white-space: nowrap; }
  .worked { flex: none; color: var(--muted); font-size: var(--fs-xs); font-variant-numeric: tabular-nums; white-space: nowrap; }
  /* В узком ряду ожидания или сбоя состояние важнее потраченного времени: без «работала …» помещается «Ждёт разрешения» целиком. */
  .row.child[data-state='waiting'] .worked, .row.child[data-state='failed'] .worked { display: none; }
  .pin { flex: none; display: inline-flex; color: var(--badge); }
  .pin :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .count { white-space: nowrap; flex: none; min-width: var(--s9); height: var(--s9); padding: var(--s0) var(--s3); box-sizing: border-box; display: inline-flex; align-items: center; justify-content: center; border-radius: var(--radius-pill); background: var(--accent); color: var(--accent-ink); font-size: var(--fs-xs); font-weight: var(--weight-bold); font-variant-numeric: tabular-nums; }
  button { flex: none; align-self: stretch; min-width: var(--target); display: inline-flex; align-items: center; justify-content: center; border: var(--s0); border-radius: var(--radius-sm); background: var(--transparent); color: var(--secondary); }
  /* Телефон: аватар крупнее, кнопка меню — пальцевая мишень. */
  :global([data-layout='phone']) .row { --avatar: var(--avatar-phone); }
  /* Под точным указателем плотный список обходится узкой кнопкой меню. */
  @media (pointer: fine) { button { min-width: var(--s12); } }
  :global([data-layout='phone']) .title { font-size: var(--fs-input); }
  /* Состояние важнее размера: 12 px хватает, чтобы «Ждёт разрешения · Bash · 12 мин» влезло на 360 px даже со счётчиком. */
  :global([data-layout='phone']) .activity { font-size: var(--fs-sm); }
  /* На телефоне строка отдаёт состояние первой: отступы уже, чтобы «Ждёт разрешения · Bash · 12 мин» влезло целиком. */
  :global([data-layout='phone']) .row:not(.child) a { gap: var(--s4); padding-inline-start: var(--s4); }
  /* Делегированная сессия: компактная строка под родителем. */
  .row.child a { gap: var(--s4); min-height: calc(var(--avatar-small) + var(--s5)); padding-block: var(--s2); }
  .row.child .title { font-size: var(--fs-meta); }
  .row.child .activity { font-size: var(--fs-sm); }
  :global([data-layout='phone']) .row.child .title { font-size: var(--fs-body); }
  :global([data-layout='phone']) .row.child .activity { font-size: var(--fs-meta); }
</style>
