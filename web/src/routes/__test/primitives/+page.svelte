<script lang="ts">
  import Button from '#lib/ui/Button.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import Pill from '#lib/ui/Pill.svelte';
  import Mark from '#lib/ui/Mark.svelte';
  import Toggle from '#lib/ui/Toggle.svelte';
  import Segmented from '#lib/ui/Segmented.svelte';
  import Field from '#lib/ui/Field.svelte';
  import Select from '#lib/ui/Select.svelte';
  import Textarea from '#lib/ui/Textarea.svelte';
  import Gauge from '#lib/ui/Gauge.svelte';
  import Disclosure from '#lib/ui/Disclosure.svelte';
  import TopBar from '#lib/ui/TopBar.svelte';
  import BottomNav from '#lib/ui/BottomNav.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import StageTrack from '#lib/ui/StageTrack.svelte';
  import { onMount } from 'svelte';
  import { theme } from '#lib/theme.js';
  import { setMotionScale } from '#lib/motion.js';
  import type { StageState } from '#lib/ui/stage-track.js';
  onMount(() => {
    if (import.meta.env.VITE_UI_TESTS !== '1') return;
    void Promise.all([import('#lib/uploads/patch.js'), import('#lib/uploads/hasher.js')]).then(([patch, hasher]) => {
      Object.assign(window, { __canterlotPatchChunk: patch.patchChunk, __canterlotHash: hasher.hashInWorker });
    });
  });
  let open = $state(false); let confirm = $state(false); let confirmed = $state(false);
  let checked = $state(false); let value = $state('one'); let input = $state(''); let select = $state('one');
  let index = $state(0); let phase = $state<StageState>('active'); let progress = $state(.25);
  const options = [{ value: 'one', label: 'Первый' }, { value: 'two', label: 'Второй' }, { value: 'three', label: 'Третий' }];
  const stages = [{ id: 'saved', label: 'сохранено' }, { id: 'machine', label: 'на машине' }, { id: 'sending', label: 'передаётся' }, { id: 'accepted', label: 'принято' }, { id: 'working', label: 'агент работает' }];
</script>
<svelte:head><title>Проверка примитивов · canterlot</title></svelte:head>
{#if import.meta.env.VITE_UI_TESTS === '1'}
  <TopBar title="Проверка примитивов" subtitle="Тестовая страница">{#snippet actions()}<IconButton icon="moon" label="Сменить тему" onclick={() => theme.toggle()} />{/snippet}</TopBar>
  <main id="main" tabindex="-1">
    <section><h2>Действия</h2><div class="row"><Button onclick={() => open = true}>Открыть панель</Button><Button disabled={true} reason="Сначала подключите машину">Отправить</Button><IconButton icon="info" label="Информация" onclick={() => open = true} /></div></section>
    <section><h2>Выбор</h2><Toggle bind:checked label="Автоматическое продолжение" /><Segmented name="sample" label="Режим" {options} bind:value /><p data-testid="selection">{value}</p></section>
    <section><h2>Поля</h2><Field label="Название" hint="Для списка сессий">{#snippet children({ id, describedby, invalid })}<input {id} aria-describedby={describedby} aria-invalid={invalid} bind:value={input} />{/snippet}</Field><Select label="Вариант" {options} bind:value={select} /><Textarea label="Сообщение" hint="Enter: новая строка" bind:value={input} /><Gauge value={81} warn label="Использовано" /></section>
    <section><h2>Состояния</h2><div class="row"><Pill color="success">0,2 с</Pill><Mark color="amber" /><Icon name="branch" /></div><Disclosure title="Подробности"><p>Данные состояния</p></Disclosure></section>
    <section><h2>Доставка</h2><StageTrack {stages} {index} state={phase} label="Доставка сообщения" reason="Обрыв передачи" status={index === 4 && phase === 'done' ? 'доставлено' : undefined} /><div class="row"><Button onclick={() => { index = Math.min(4, index + 1); phase = 'active'; }}>Следующий этап</Button><Button onclick={() => phase = 'error'}>Ошибка этапа</Button><Button onclick={() => phase = 'held'}>Ожидать</Button><Button onclick={() => { index = 4; phase = 'done'; }}>Завершить доставку</Button></div></section>
    <section><h2>Прерывание</h2><StageTrack form="named" label="Прерывание хода" stages={[{ id: 'request', label: 'запрошено' }, { id: 'stop', label: 'процессы завершаются', kind: 'spin' }, { id: 'done', label: 'остановлено' }]} index={1} /></section>
    <section><h2>Вложение</h2><StageTrack label="Доставка вложения" stages={[{ id: 'upload', label: 'загрузка', kind: 'ring' }, { id: 'hub', label: 'на хабе' }, { id: 'machine', label: 'на машине' }, { id: 'done', label: 'готово агенту' }]} index={0} {progress} /><Button onclick={() => progress = .65}>Передано 65 процентов</Button></section>
    <section><h2>Скорость</h2><div class="row"><Button onclick={() => setMotionScale(0)}>Без анимации</Button><Button onclick={() => setMotionScale(.5)}>Быстрее</Button><Button onclick={() => setMotionScale(1)}>Обычная</Button><Button onclick={() => setMotionScale(1.5)}>Медленнее</Button></div></section>
    <section><Button onclick={() => confirm = true}>Запросить подтверждение</Button>{#if confirmed}<p>Подтверждено</p>{/if}</section>
  </main>
  <BottomNav items={[{ href: '/', label: 'Сессии' }, { href: '/__test/primitives', label: 'Примитивы', current: true }]} />
  <Sheet {open} title="Параметры" routeKey="primitives" onclose={() => open = false}><p>Настройки панели</p><Button onclick={() => open = false}>Готово</Button></Sheet>
  {#if confirm}<ConfirmDialog title="Подтвердить действие" body="Действие в тестовой странице" confirmLabel="Подтвердить" onconfirm={() => confirmed = true} onclose={() => confirm = false} />{/if}
{:else}<main id="main"><h1>Страница недоступна</h1></main>{/if}
<style>
  main { max-width: var(--reading-max); margin: auto; padding: var(--s8); }
  section { margin-bottom: var(--s10); }
  h2 { margin-bottom: var(--s6); }
  .row { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s4); margin-block: var(--s4); }
</style>
