<script lang="ts">
  let { label, checked, onchange }: { label: string; checked: boolean; onchange: (on: boolean) => void } = $props();
</script>
<label class:on={checked}>
  <input type="checkbox" {checked} onchange={(event) => onchange(event.currentTarget.checked)} />
  <span class="dot" aria-hidden="true"></span>
  <span class="text">{label}</span>
</label>
<style>
  label { position: relative; display: inline-flex; align-items: center; gap: var(--s4); min-width: 0; max-width: 100%; min-height: var(--target); padding: var(--s2) var(--s8) var(--s2) var(--s6); border-radius: var(--radius-pill); background: var(--chat-bg); color: var(--secondary); font-size: var(--fs-meta); cursor: pointer; }
  label.on { background: var(--selected); color: var(--text); }
  input { position: absolute; inset: 0; width: 100%; height: 100%; margin: 0; opacity: 0; cursor: pointer; }
  input:focus-visible ~ .dot { outline: var(--focus-width) solid var(--accent); outline-offset: var(--s1); }
  .dot { flex: none; width: var(--mark-large); height: var(--mark-large); border-radius: var(--radius-round); background: var(--badge); }
  .on .dot { background: var(--accent); }
  .text { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
