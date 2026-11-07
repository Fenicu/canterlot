/** Заглушка `$app/paths` для юнит-тестов: приложение живёт в корне, адрес не меняется. */
export const base = '';
export const assets = '';
export function resolve(path: string): string {
  return path;
}
