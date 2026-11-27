<script lang="ts">
  import type { components } from '#lib/api/generated/openapi.js';
  import { issueJoinCode, reissueJoinCode } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { stepUp, StepUpCancelled } from '#lib/auth/stepup.svelte.js';
  import Button from '#lib/ui/Button.svelte';
  import CopyButton from '#lib/timeline/cards/CopyButton.svelte';
  import Field from '#lib/ui/Field.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import { groupCode, type PendingMachine } from './pending.js';

  type JoinCode = components['schemas']['JoinCodeView'];

  let { machine, onclose, onissued }: {
    /** Есть только при перевыпуске: код выдаётся заново той же ожидающей машине. */
    machine?: PendingMachine;
    onclose: () => void;
    onissued?: () => void;
  } = $props();

  let name = $state('');
  let busy = $state(false);
  let error = $state('');
  // Код живёт только в этом листе: в хранилище или журнал он не попадает.
  let result = $state<JoinCode | null>(null);
  const grouped = $derived(result ? groupCode(result.code) : '');
  const title = $derived(machine ? 'Новый код' : 'Новая машина');

  /** `targetId` задан при перевыпуске: новая машина не создаётся второй раз. */
  async function obtain(targetId?: string): Promise<void> {
    if (busy) return;
    const trimmed = name.trim();
    if (!targetId && !trimmed) {
      error = 'Введите имя машины';
      return;
    }
    busy = true;
    error = '';
    try {
      // Ключ создаётся внутри действия: повтор после подтверждения входа не должен стать повтором запроса.
      const data = await stepUp.run(() =>
        targetId
          ? reissueJoinCode(targetId, crypto.randomUUID()).then((reply) => reply.data)
          : issueJoinCode({ name: trimmed }, crypto.randomUUID()).then((reply) => reply.data)
      );
      result = data;
      name = '';
      onissued?.();
    } catch (err) {
      if (!(err instanceof StepUpCancelled)) {
        error = errorText(err, 'Не удалось получить код. Повторите при восстановлении связи');
      }
    } finally {
      busy = false;
    }
  }

  function submit(event: SubmitEvent): void {
    event.preventDefault();
    void obtain();
  }

  // Перевыпуск из списка ожидающих: код нужен сразу, но только один раз за открытие листа.
  // Отмена окна подтверждения не должна снова его открывать.
  let autoIssued = false;
  $effect(() => {
    if (!machine || autoIssued) return;
    autoIssued = true;
    void obtain(machine.machine_id);
  });
</script>

<Sheet open title={title} {onclose}>
  <div class="add" data-join-sheet>
    {#if result}
      {@const issued = result}
      <p>Выполните на новой машине:</p>
      <div class="line" data-join-command>
        <code>{issued.command}</code>
        <CopyButton text={issued.command} label="Скопировать команду" />
      </div>
      <p>Код (действует 15 минут):</p>
      <div class="line" data-join-code>
        <code>{grouped}</code>
        <CopyButton text={grouped} label="Скопировать код" />
      </div>
      <p class="note">Код одноразовый. Если не успели или ответ потерялся, нажмите «Новый код».</p>
      <div class="actions"><Button onclick={() => void obtain(issued.machine_id)}>Новый код</Button></div>
    {:else if !machine}
      <form onsubmit={submit}>
        <Field label="Имя машины" error={error || undefined}>
          {#snippet children({ id, describedby, invalid })}
            <input {id} type="text" maxlength="64" autocomplete="off" bind:value={name} aria-describedby={describedby} aria-invalid={invalid} />
          {/snippet}
        </Field>
        <Button type="submit" variant="primary" full busy={busy}>Получить код</Button>
      </form>
    {:else if error}
      <p class="error" role="alert">{error}</p>
      <Button onclick={() => void obtain(machine.machine_id)}>Повторить</Button>
    {:else}
      <p class="note" role="status">Получаю код…</p>
    {/if}
  </div>
</Sheet>

<style>
  .add { display: flex; flex-direction: column; gap: var(--s4); min-width: 0; }
  .add p { margin: var(--s0); }
  .note { color: var(--muted); font-size: var(--fs-sm); }
  .error { color: var(--danger); font-size: var(--fs-sm); }
  form { display: flex; flex-direction: column; gap: var(--s6); }
  .line { display: flex; align-items: center; gap: var(--s3); min-width: 0; padding: var(--s4) var(--s6); border-radius: var(--radius-sm); background: var(--code); }
  .line code { min-width: 0; overflow-wrap: anywhere; font-family: var(--font-mono); font-size: var(--fs-sm); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s4); }
</style>
