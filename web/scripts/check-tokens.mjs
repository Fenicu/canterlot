import { readFileSync } from 'node:fs';
import { lintTree } from './token-lint.mjs';
const violations = lintTree('src', JSON.parse(readFileSync('token-lint.allow.json', 'utf8')));
// Предупреждение печатается, но не валит прогон: это форма, которая ломается на переносе редко,
// и запись в списке разрешений превратила бы её в тишину.
for (const v of violations) if (v.severity === 'warning') process.stderr.write(`${v.path}:${v.line}: warning ${v.rule}: ${v.value}\n`);
const errors = violations.filter((v) => v.severity !== 'warning');
for (const v of errors) process.stderr.write(`${v.path}:${v.line}: ${v.rule}: ${v.value}\n`);
if (errors.length) process.exitCode = 1;
else process.stdout.write('Token lint: PASS\n');
