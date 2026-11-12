import type { Vendor } from './vendor';

/** Знак вендора: контуры одного цвета, цвет задаёт аватар. */
export interface Logo {
  viewBox: string;
  evenodd: boolean;
  paths: string[];
}

const files = import.meta.glob<string>('./logos/*.svg', { query: '?raw', import: 'default', eager: true });

/** Разбирает монохромный SVG из lobehub: только `viewBox`, правило заливки и контуры `path`. */
export function parseLogo(svg: string): Logo {
  const viewBox = /viewBox="([^"]+)"/.exec(svg)?.[1] ?? '0 0 24 24';
  const evenodd = /<svg[^>]*fill-rule="evenodd"/.test(svg);
  const paths = [...svg.matchAll(/<path\b[^>]*\bd="([^"]+)"/g)].map((match) => match[1] ?? '');
  return { viewBox, evenodd, paths: paths.filter(Boolean) };
}

export const LOGOS: Partial<Record<Vendor, Logo>> = Object.fromEntries(
  Object.entries(files).map(([path, svg]) => [path.replace(/^.*\/(.+)\.svg$/, '$1'), parseLogo(svg)])
);
