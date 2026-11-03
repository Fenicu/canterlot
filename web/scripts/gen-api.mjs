import { readFile, writeFile, mkdir } from 'node:fs/promises';
import openapiTS, { astToString } from 'openapi-typescript';
import { compile } from 'json-schema-to-typescript';

// node scripts/gen-api.mjs          перегенерировать src/lib/api/generated из docs/api
// node scripts/gen-api.mjs --check  только сверить: код выхода 1, если сгенерированные типы устарели
const check = process.argv.includes('--check');
const target = new URL('../src/lib/api/generated/', import.meta.url);
const schema = JSON.parse(await readFile(new URL('../../docs/api/events.schema.json', import.meta.url), 'utf8'));
const files = {
  'openapi.ts': astToString(await openapiTS(new URL('../../docs/api/openapi.json', import.meta.url))),
  'events.ts': await compile({ ...schema, title: 'WireEvent' }, 'WireEvent', { bannerComment: '', additionalProperties: false })
};
for (const name of ['ClientOp', 'ServerOp']) {
  files[`${name}.ts`] = await compile({ $defs: schema.$defs, $ref: `#/$defs/${name}`, title: name }, name, { bannerComment: '', additionalProperties: false });
}

if (check) {
  const stale = [];
  for (const [name, content] of Object.entries(files)) {
    const current = await readFile(new URL(name, target), 'utf8').catch(() => '');
    if (current !== content) stale.push(name);
  }
  if (stale.length > 0) {
    process.stderr.write(`generated API types are stale: ${stale.join(', ')}; run npm run gen:api\n`);
    process.exit(1);
  }
  process.stdout.write('generated API types match docs/api\n');
} else {
  await mkdir(target, { recursive: true });
  for (const [name, content] of Object.entries(files)) await writeFile(new URL(name, target), content);
}
