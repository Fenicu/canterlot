import type { Plugin } from 'vite';
export function darkTokens(css: string, palette?: string): Record<string, string>;
export function manifest(css: string): {
  id: string; start_url: string; scope: string; name: string; short_name: string; lang: string; display: string;
  theme_color: string; background_color: string;
  icons: { src: string; sizes: string; type: string; purpose: string }[];
  shortcuts: { name: string; url: string }[];
  share_target: { action: string; method: string; enctype: string; params: { title: string; text: string; url: string; files: { name: string; accept: string[] }[] } };
};
export function pwaManifest(): Plugin;
