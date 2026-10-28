export const iconLabel = {
  meta: { type: 'problem', schema: [], messages: { label: 'IconButton requires a label.' } },
  create(context) { return { SvelteElement(node) {
    if (node.name?.name !== 'IconButton') return;
    if (!node.startTag.attributes.some(a => a.type === 'SvelteAttribute' && a.key.name === 'label' && a.value?.length)) context.report({ node, messageId: 'label' });
  } }; }
};
