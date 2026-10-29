import ts from 'typescript-eslint';
import svelte from 'eslint-plugin-svelte';
import { iconLabel } from './scripts/eslint-rules.mjs';
const restricted = [
  { property: 'innerHTML', message: 'Use the sanitised renderer.' },
  { property: 'outerHTML', message: 'Use the sanitised renderer.' },
  { property: 'insertAdjacentHTML', message: 'Use the sanitised renderer.' },
  { object: 'document', property: 'write', message: 'Use the sanitised renderer.' }
];
const consoleRestricted = ['log', 'info', 'warn', 'error', 'debug'].map(property => ({
  object: 'console',
  property,
  message: 'No private data in logs.'
}));
export default ts.config(
  { ignores: ['build/**', 'build-update/**', '.svelte-kit/**', 'node_modules/**', 'test-results/**', 'playwright-report/**', 'tests/unit/fixtures/**', 'src/lib/api/generated/**'] },
  ...ts.configs.recommended,
  ...svelte.configs['flat/recommended'],
  { files: ['**/*.svelte', '**/*.svelte.ts', '**/*.svelte.js'], languageOptions: { parserOptions: { parser: ts.parser } } },
  { files: ['**/*.{ts,js,mjs,svelte}'], plugins: { canterlot: { rules: { 'icon-label': iconLabel } } }, rules: {
    'no-restricted-properties': ['error', ...restricted, ...consoleRestricted],
    'no-eval': 'error', 'canterlot/icon-label': 'error',
    'svelte/no-at-html-tags': 'error', 'svelte/require-each-key': 'error'
  } },
  { files: ['src/lib/render/Html.svelte'], rules: { 'svelte/no-at-html-tags': 'off', 'no-restricted-properties': ['error', ...consoleRestricted] } }
);
