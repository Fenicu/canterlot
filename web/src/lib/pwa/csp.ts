export async function scriptHashes(html: string): Promise<string[]> {
  const scripts = [...html.matchAll(/<script\b(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/g)];
  return Promise.all(scripts.map(async match => {
    const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(match[1])));
    return `sha256-${btoa(String.fromCharCode(...hash))}`;
  }));
}
export async function scriptsAllowed(html: string, hashes: string[]): Promise<boolean> {
  return !/\son\w+\s*=/i.test(html) && (await scriptHashes(html)).every(hash => hashes.includes(hash));
}
