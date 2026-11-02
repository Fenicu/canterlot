export type ResumeReason = 'visible' | 'pageshow' | 'resume' | 'online' | 'focus' | 'clock_jump';
export interface Lifecycle {
  onResume(cb: (i: { hiddenMs: number; reason: ResumeReason }) => void): () => void;
  onSuspend(cb: (r: 'hidden' | 'pagehide' | 'freeze') => void): () => void;
  readonly visible: boolean;
}
export interface LifecycleEnvironment {
  window: Window;
  document: Document;
  wall(): number;
  monotonic(): number;
  schedule(fn: () => void, ms: number): () => void;
}
export function browserLifecycle(environment?: LifecycleEnvironment): Lifecycle {
  const env = environment ?? {
    window, document, wall: () => Date.now(), monotonic: () => performance.now(),
    schedule(fn: () => void, ms: number) { const id = setTimeout(fn, ms); return () => clearTimeout(id); }
  };
  const resumes = new Set<(i: { hiddenMs: number; reason: ResumeReason }) => void>();
  const suspends = new Set<(r: 'hidden' | 'pagehide' | 'freeze') => void>();
  let hiddenAt: number | undefined = env.document.visibilityState === 'hidden' ? env.wall() : undefined;
  let wall = env.wall(), monotonic = env.monotonic();
  let cancelTick: (() => void) | undefined;
  let listening = false;
  const resume = (reason: ResumeReason) => {
    const hiddenMs = hiddenAt === undefined ? 0 : Math.max(0, env.wall() - hiddenAt);
    hiddenAt = undefined;
    for (const cb of resumes) cb({ hiddenMs, reason });
  };
  const suspend = (reason: 'hidden' | 'pagehide' | 'freeze') => {
    hiddenAt ??= env.wall();
    for (const cb of suspends) cb(reason);
  };
  const visibility = () => env.document.visibilityState === 'hidden' ? suspend('hidden') : resume('visible');
  const pageshow = () => resume('pageshow');
  const pagehide = () => suspend('pagehide');
  const freeze = () => suspend('freeze');
  const thaw = () => resume('resume');
  const online = () => resume('online');
  const offline = () => suspend('hidden');
  const focus = () => resume('focus');
  const tick = () => {
    const nextWall = env.wall(), nextMonotonic = env.monotonic();
    const jump = Math.max(Math.abs((nextWall - wall) - (nextMonotonic - monotonic)), nextMonotonic - monotonic - 1000);
    wall = nextWall; monotonic = nextMonotonic;
    if (jump > 3000) resume('clock_jump');
    cancelTick = env.schedule(tick, 1000);
  };
  const bindings: [EventTarget, string, EventListener][] = [
    [env.document, 'visibilitychange', visibility], [env.window, 'pageshow', pageshow as EventListener],
    [env.window, 'pagehide', pagehide], [env.document, 'freeze', freeze], [env.document, 'resume', thaw],
    [env.window, 'online', online], [env.window, 'offline', offline], [env.window, 'focus', focus]
  ];
  function connect() {
    if (listening) return;
    listening = true; wall = env.wall(); monotonic = env.monotonic();
    for (const [target, name, cb] of bindings) target.addEventListener(name, cb);
    cancelTick = env.schedule(tick, 1000);
  }
  function release() {
    if (resumes.size || suspends.size) return;
    listening = false; cancelTick?.(); cancelTick = undefined;
    for (const [target, name, cb] of bindings) target.removeEventListener(name, cb);
  }
  return {
    get visible() { return env.document.visibilityState !== 'hidden'; },
    onResume(cb) { resumes.add(cb); connect(); return () => { resumes.delete(cb); release(); }; },
    onSuspend(cb) { suspends.add(cb); connect(); return () => { suspends.delete(cb); release(); }; }
  };
}
