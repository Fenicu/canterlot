type Fade = 'start' | 'end' | 'both';

function fadeOf(pre: HTMLElement): Fade | null {
  const rest = pre.scrollWidth - pre.clientWidth;
  if (rest <= 1) return null;
  const before = pre.scrollLeft > 1;
  const after = rest - pre.scrollLeft > 1;
  if (before && after) return 'both';
  return before ? 'start' : 'end';
}

function sync(pre: HTMLElement): void {
  const fade = fadeOf(pre);
  if (fade) pre.dataset.fade = fade;
  else delete pre.dataset.fade;
}

/** Блок кода шире карточки обрезался бы молча: край, за которым есть продолжение, гаснет. */
export function edgeFade(node: HTMLElement) {
  const all = () => node.querySelectorAll<HTMLElement>('pre').forEach(sync);
  const onScroll = (event: Event) => {
    if (event.target instanceof HTMLPreElement) sync(event.target);
  };
  node.addEventListener('scroll', onScroll, true);
  all();
  const resize = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(all);
  resize?.observe(node);
  return {
    destroy() {
      node.removeEventListener('scroll', onScroll, true);
      resize?.disconnect();
    }
  };
}
