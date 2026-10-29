export function removeKitAnnouncer({ content, filename = '' }) {
  const normalized = filename?.replaceAll('\\', '/');
  if (!normalized?.endsWith('/@sveltejs/kit/src/runtime/components/root.svelte') && !normalized?.endsWith('/.svelte-kit/generated/root.svelte')) return;
  const block = /\{#if mounted\}\s*<div[\s\S]*?id="svelte-announcer"[\s\S]*?<\/div>\s*\{\/if\}/;
  if (!block.test(content)) throw new Error('SvelteKit announcer changed; review the single live-region integration');
  return { code: content.replace(block, '') };
}
