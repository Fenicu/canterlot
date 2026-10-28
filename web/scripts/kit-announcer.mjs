export function removeKitAnnouncer({ content, filename }) {
  if (!filename?.replaceAll('\\', '/').endsWith('/.svelte-kit/generated/root.svelte')) return;
  const block = /\{#if mounted\}\s*<div id="svelte-announcer"[^>]*>[\s\S]*?<\/div>\s*\{\/if\}/;
  if (!block.test(content)) throw new Error('SvelteKit announcer changed; review the single live-region integration');
  return { code: content.replace(block, '') };
}
