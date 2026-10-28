import { readFileSync } from 'node:fs';
import { lintTree } from './token-lint.mjs';
const violations = lintTree('src', JSON.parse(readFileSync('token-lint.allow.json', 'utf8')));
for (const v of violations) process.stderr.write(`${v.path}:${v.line}: ${v.rule}: ${v.value}\n`);
if (violations.length) process.exitCode = 1;
else process.stdout.write('Token lint: PASS\n');
