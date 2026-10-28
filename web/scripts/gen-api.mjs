import { readFile, writeFile, mkdir } from 'node:fs/promises';
import openapiTS, { astToString } from 'openapi-typescript';
import { compile } from 'json-schema-to-typescript';
const target = new URL('../src/lib/api/generated/', import.meta.url);
await mkdir(target, { recursive: true });
await writeFile(new URL('openapi.ts', target), astToString(await openapiTS(new URL('../../docs/api/openapi.json', import.meta.url))));
const schema = JSON.parse(await readFile(new URL('../../docs/api/events.schema.json', import.meta.url), 'utf8'));
await writeFile(new URL('events.ts', target), await compile({ ...schema, title: 'WireEvent' }, 'WireEvent', { bannerComment: '', additionalProperties: false }));
for (const name of ['ClientOp', 'ServerOp']) await writeFile(new URL(`${name}.ts`, target), await compile({ $defs: schema.$defs, $ref: `#/$defs/${name}`, title: name }, name, { bannerComment: '', additionalProperties: false }));
