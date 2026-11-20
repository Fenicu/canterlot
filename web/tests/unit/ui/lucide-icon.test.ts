import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { LUCIDE_VERSION, lucidePath, registryLine } from '../../../scripts/lucide-icon.mjs';
import { icons } from '../../../src/lib/ui/icons';

const FIXTURE = `<!-- @license lucide-static v1.77.0 - ISC -->
<svg class="lucide" xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor">
  <path d="m15 12-9.373 9.373a1 1 0 0 1-3.001-3L12 9" />
  <path d="m18 15 4-4" />
  <circle cx="12" cy="12" r="3" />
  <rect width="8" height="4" x="2" y="3" rx="1" />
  <line x1="2" x2="22" y1="2" y2="22" />
  <polyline points="3 3 9 9 3 15" />
</svg>`;

describe('lucide-icon converter', () => {
  it('turns_every_shape_into_one_absolute_path_string', () => {
    expect(lucidePath(FIXTURE)).toBe(
      'M15 12l-9.373 9.373a1 1 0 0 1-3.001-3L12 9' +
        'M18 15l4-4' +
        'M9 12a3 3 0 1 0 6 0a3 3 0 1 0 -6 0' +
        'M3 3h6a1 1 0 0 1 1 1v2a1 1 0 0 1 -1 1h-6a1 1 0 0 1 -1 -1v-2a1 1 0 0 1 1 -1Z' +
        'M2 2L22 22' +
        'M3 3L9 9L3 15'
    );
  });

  it('a_relative_move_in_the_middle_never_starts_from_the_previous_subpath', () => {
    const d = lucidePath('<svg><path d="m1 1 2 2" /><path d="m5 5 2 2" /></svg>');
    expect(d).toBe('M1 1l2 2M5 5l2 2');
    expect(d.match(/m/g)).toBeNull();
  });

  it('a_move_without_implicit_lines_stays_a_plain_absolute_move', () => {
    expect(lucidePath('<svg><path d="m4 4h5" /></svg>')).toBe('M4 4h5');
  });

  it('an_element_the_converter_does_not_know_is_an_error_not_a_silent_gap', () => {
    expect(() => lucidePath('<svg><use href="#a" /></svg>')).toThrow(/use/);
    expect(() => lucidePath('<svg><g><path d="M1 1" /></g></svg>')).toThrow(/g/);
  });

  it('the_registry_line_names_the_key_in_camel_case_and_quotes_the_path', () => {
    expect(registryLine('wifi-off', '<svg><path d="M1 1" /></svg>')).toBe("wifiOff: 'M1 1',");
  });

  it('the_version_is_pinned', () => {
    expect(LUCIDE_VERSION).toBe('1.77.0');
  });
});

describe('registry geometry comes from the pinned Lucide files', () => {
  for (const [file, key] of [
    ['brain', 'brain'],
    ['hammer', 'hammer'],
    ['wifi', 'wifi'],
    ['wifi-off', 'wifiOff']
  ] as const) {
    it(`${key}_is_the_mechanical_conversion_of_${file}_svg`, () => {
      const svg = readFileSync(`tests/unit/fixtures/lucide/${file}.svg`, 'utf8');
      expect(svg).toContain(`lucide-static v${LUCIDE_VERSION} - ISC`);
      expect(icons[key]).toBe(lucidePath(svg));
    });
  }
});
