/** Удаляет ANSI, включая незавершённую последовательность в конце потока. */
export function stripAnsi(s: string): string {
  if (typeof s !== 'string') return '';
  const parts: string[] = [];
  let plainStart = 0;
  let i = 0;
  while (i < s.length) {
    const code = s.charCodeAt(i);
    if (code !== 0x1b && (code < 0x80 || code > 0x9f)) {
      i++;
      continue;
    }

    parts.push(s.slice(plainStart, i));
    let command = code;
    i++;
    if (code === 0x1b && i < s.length) {
      command = s.charCodeAt(i);
      // ESC followed by another control starts a new sequence.
      if (command < 0x20 || command > 0x7e) {
        plainStart = i;
        continue;
      }
      i++;
    }

    if (command === 0x5b || command === 0x9b) {
      while (i < s.length && s.charCodeAt(i) >= 0x30 && s.charCodeAt(i) <= 0x3f) i++;
      while (i < s.length && s.charCodeAt(i) >= 0x20 && s.charCodeAt(i) <= 0x2f) i++;
      if (i < s.length && s.charCodeAt(i) >= 0x40 && s.charCodeAt(i) <= 0x7e) i++;
    } else if (
      command === 0x5d || command === 0x9d ||
      command === 0x50 || command === 0x90 ||
      command === 0x58 || command === 0x98 ||
      command === 0x5e || command === 0x9e ||
      command === 0x5f || command === 0x9f
    ) {
      const osc = command === 0x5d || command === 0x9d;
      while (i < s.length) {
        const body = s.charCodeAt(i++);
        if (body === 0x9c || (osc && body === 0x07)) break;
        if (body === 0x1b && s.charCodeAt(i) === 0x5c) {
          i++;
          break;
        }
      }
    } else if (code === 0x1b && command >= 0x20 && command <= 0x2f) {
      while (i < s.length && s.charCodeAt(i) >= 0x20 && s.charCodeAt(i) <= 0x2f) i++;
      if (i < s.length && s.charCodeAt(i) >= 0x30 && s.charCodeAt(i) <= 0x7e) i++;
    }
    plainStart = i;
  }
  parts.push(s.slice(plainStart));
  return parts.join('');
}
