import { describe, it, expect } from 'vitest';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import openapiTS, { astToString } from 'openapi-typescript';
import { compile } from 'json-schema-to-typescript';

describe('api/generated-types', () => {
  it('generated_types_are_fresh_against_committed_contract_files', async () => {
    const targetDir = resolve(process.cwd(), 'src/lib/api/generated');
    const docsDir = resolve(process.cwd(), '../docs/api');

    // 1. openapi.ts
    const openapiJson = JSON.parse(await readFile(resolve(docsDir, 'openapi.json'), 'utf8'));
    const expectedOpenApi = astToString(await openapiTS(openapiJson));
    const actualOpenApi = await readFile(resolve(targetDir, 'openapi.ts'), 'utf8');
    expect(actualOpenApi.trim()).toBe(expectedOpenApi.trim());

    // 2. events.ts
    const schema = JSON.parse(await readFile(resolve(docsDir, 'events.schema.json'), 'utf8'));
    const expectedEvents = await compile({ ...schema, title: 'WireEvent' }, 'WireEvent', { bannerComment: '', additionalProperties: false });
    const actualEvents = await readFile(resolve(targetDir, 'events.ts'), 'utf8');
    expect(actualEvents.trim()).toBe(expectedEvents.trim());

    // 3. ClientOp.ts and ServerOp.ts
    for (const name of ['ClientOp', 'ServerOp']) {
      const expectedOp = await compile({ $defs: schema.$defs, $ref: `#/$defs/${name}`, title: name }, name, { bannerComment: '', additionalProperties: false });
      const actualOp = await readFile(resolve(targetDir, `${name}.ts`), 'utf8');
      expect(actualOp.trim()).toBe(expectedOp.trim());
    }
  });
});
