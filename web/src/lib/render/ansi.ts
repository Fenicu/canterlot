/**
 * Очистка текста от управляющих последовательностей ANSI (CSI, OSC, переходы курсора, цвета).
 */
const ANSI_REGEX = new RegExp(
  [
    '[\\u001B\\u009B]\\][^\u0007\\u001B]*(?:\\u0007|\\u001B\\\\)',
    '[\\u001B\\u009B]\\[[0-?]*[ -/]*[@-~]',
    '[\\u001B\\u009B][()#;?]?[0-9A-Za-z]'
  ].join('|'),
  'g'
);

export function stripAnsi(s: string): string {
  if (typeof s !== 'string') return '';
  return s.replace(ANSI_REGEX, '');
}
