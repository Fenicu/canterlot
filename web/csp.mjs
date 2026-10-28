export const directives = {
  'default-src': ['self'], 'script-src': ['self'], 'style-src': ['self'],
  'img-src': ['self', 'blob:', 'data:'], 'connect-src': ['self'],
  'worker-src': ['self', 'blob:'], 'manifest-src': ['self'], 'object-src': ['none'],
  'base-uri': ['self'], 'form-action': ['self'], 'frame-ancestors': ['none']
};
const keywords = new Set(['self', 'none']);
export function productionCsp({ websocketOrigin, scriptHashes = [] } = {}) {
  const policy = structuredClone(directives);
  if (websocketOrigin) {
    const url = new URL(websocketOrigin);
    if (!['ws:', 'wss:'].includes(url.protocol) || url.origin !== websocketOrigin) throw new Error('Invalid WebSocket origin');
    policy['connect-src'].push(websocketOrigin);
  }
  policy['script-src'].push(...scriptHashes);
  return Object.entries(policy).map(([name, values]) => `${name} ${values.map(value => keywords.has(value) || value.startsWith('sha256-') ? `'${value}'` : value).join(' ')}`).join('; ');
}
export default productionCsp();
