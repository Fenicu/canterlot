export type SwRoute = 'api' | 'asset' | 'navigation' | 'share_target' | 'passthrough';
interface RequestInfo { url: string; method: string; mode: string; destination: string }

export function classify(req: RequestInfo): SwRoute {
  const path = new URL(req.url).pathname;
  if (path === '/api' || path.startsWith('/api/') || ['/healthz', '/config'].includes(path)) return 'api';
  // Системное «Поделиться» открывает share target навигацией верхнего уровня; fetch, iframe и no-cors форма с чужого сайта сюда не попадают.
  if (path === '/_share' && req.method === 'POST' && req.mode === 'navigate' && req.destination === 'document') return 'share_target';
  if (req.method !== 'GET') return 'passthrough';
  if (req.mode === 'navigate') return 'navigation';
  if (path.startsWith('/_app/immutable/') || path.startsWith('/icons/') || ['/theme-init.js', '/manifest.webmanifest', '/200.html'].includes(path)) return 'asset';
  return 'passthrough';
}

export function cacheable(response: Response): boolean {
  return response.ok && !response.redirected && !['opaque', 'opaqueredirect'].includes(response.type)
    && !/private|no-store/i.test(response.headers.get('Cache-Control') ?? '')
    && !response.headers.has('WWW-Authenticate');
}

export async function navigationResponse(
  req: RequestInfo,
  network: () => Promise<Response>,
  shell: () => Promise<Response | undefined>
): Promise<Response> {
  try { return await network(); }
  catch (error) {
    if (classify(req) === 'navigation') {
      const fallback = await shell();
      if (fallback) return fallback;
    }
    throw error;
  }
}
