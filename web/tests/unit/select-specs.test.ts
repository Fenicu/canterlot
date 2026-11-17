import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { findUncovered, matchesGlob, selectSpecs } from '../../e2e/select-specs.mjs';

type Areas = {
  global: string[];
  globalSpecs: string[];
  areas: { name: string; description?: string; sources: string[]; specs: string[] }[];
};

const areas: Areas = {
  global: ['src/lib/styles/**', 'playwright.config.ts', 'e2e/**/*.ts'],
  globalSpecs: ['e2e/ui/**/*.spec.ts', 'e2e/parity/**/*.spec.ts'],
  areas: [
    {
      name: 'feed',
      sources: ['src/lib/timeline/**'],
      specs: ['e2e/ui/dialog.spec.ts', 'e2e/ui/timeline.spec.ts']
    },
    {
      name: 'auth',
      sources: ['src/lib/auth/**'],
      specs: ['e2e/ui/sign-in.spec.ts']
    }
  ]
};

const allSpecs = [
  'e2e/ui/dialog.spec.ts',
  'e2e/ui/timeline.spec.ts',
  'e2e/ui/sign-in.spec.ts',
  'e2e/parity/visual-language.spec.ts'
];

const both = ['phone', 'desktop'];
const phone = ['phone'];

describe('matchesGlob', () => {
  it('matches_a_double_star_glob_across_directories', () => {
    expect(matchesGlob('src/lib/timeline/**', 'src/lib/timeline/cards/defs/tools.ts')).toBe(true);
    expect(matchesGlob('src/lib/timeline/**', 'src/lib/timeline/Timeline.svelte')).toBe(true);
  });

  it('does_not_match_a_sibling_directory_with_a_similar_name', () => {
    expect(matchesGlob('src/lib/timeline/**', 'src/lib/timelines/Timeline.svelte')).toBe(false);
  });

  it('matches_a_single_star_within_one_segment', () => {
    expect(matchesGlob('e2e/ui/*.ts', 'e2e/ui/fixtures.ts')).toBe(true);
    expect(matchesGlob('e2e/ui/*.ts', 'e2e/ui/nested/fixtures.ts')).toBe(false);
  });
});

describe('findUncovered', () => {
  it('returns_only_source_paths_outside_every_area_and_the_global_list', () => {
    const paths = [
      'src/lib/timeline/rows.ts',
      'src/lib/styles/base.css',
      'src/lib/auth/api.ts',
      'src/lib/mystery/x.ts'
    ];
    expect(findUncovered(paths, areas)).toEqual(['src/lib/mystery/x.ts']);
  });
});

describe('selectSpecs', () => {
  it('selects_only_the_changed_spec_on_both_projects', () => {
    expect(selectSpecs(['e2e/ui/dialog.spec.ts'], areas, allSpecs)).toEqual([
      { path: 'e2e/ui/dialog.spec.ts', reasons: ['изменена спека'], projects: both }
    ]);
  });

  it('selects_every_spec_of_the_area_on_both_projects_for_a_changed_source_file', () => {
    expect(selectSpecs(['src/lib/rows.ts'], { ...areas, areas: [{ ...areas.areas[0]!, sources: ['src/lib/rows.ts'] }] }, allSpecs)).toEqual([
      { path: 'e2e/ui/dialog.spec.ts', reasons: ['область «feed»'], projects: both },
      { path: 'e2e/ui/timeline.spec.ts', reasons: ['область «feed»'], projects: both }
    ]);
  });

  it('selects_the_whole_spec_set_on_phone_only_for_a_global_path', () => {
    expect(selectSpecs(['src/lib/styles/tokens.css'], areas, allSpecs)).toEqual(
      [...allSpecs].sort().map(path => ({ path, reasons: ['глобальный путь'], projects: phone }))
    );
  });

  it('combines_reasons_and_widens_projects_when_global_and_area_both_select_a_spec', () => {
    expect(selectSpecs(['src/lib/styles/tokens.css', 'src/lib/timeline/rows.ts'], areas, allSpecs)).toEqual([
      { path: 'e2e/parity/visual-language.spec.ts', reasons: ['глобальный путь'], projects: phone },
      { path: 'e2e/ui/dialog.spec.ts', reasons: ['глобальный путь', 'область «feed»'], projects: both },
      { path: 'e2e/ui/sign-in.spec.ts', reasons: ['глобальный путь'], projects: phone },
      { path: 'e2e/ui/timeline.spec.ts', reasons: ['глобальный путь', 'область «feed»'], projects: both }
    ]);
  });

  it('combines_reasons_when_a_spec_is_changed_and_its_area_is_touched', () => {
    expect(selectSpecs(['e2e/ui/dialog.spec.ts', 'src/lib/timeline/rows.ts'], areas, allSpecs)).toEqual([
      { path: 'e2e/ui/dialog.spec.ts', reasons: ['изменена спека', 'область «feed»'], projects: both },
      { path: 'e2e/ui/timeline.spec.ts', reasons: ['область «feed»'], projects: both }
    ]);
  });

  it('ignores_paths_that_are_neither_specs_nor_mapped_sources', () => {
    expect(selectSpecs(['src/lib/mystery/x.ts', 'docs/api/openapi.json'], areas, allSpecs)).toEqual([]);
  });
});

const walk = (dir: string): string[] =>
  readdirSync(dir).flatMap(name => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? walk(path) : [path];
  });

describe('checked-in area map', () => {
  const real: Areas = JSON.parse(readFileSync('e2e/areas.json', 'utf8'));

  it('covers_every_file_under_web_src', () => {
    expect(findUncovered(walk('src'), real)).toEqual([]);
  });

  it('feed_area_lists_every_spec_that_failed_the_1_39_0_gate', () => {
    const feed = real.areas.find(area => area.name.includes('лента'));
    expect(feed).toBeDefined();
    expect(feed!.specs).toEqual(
      expect.arrayContaining([
        'e2e/ui/block-fold.spec.ts',
        'e2e/ui/chat-visual-fix.spec.ts',
        'e2e/ui/design-layout.spec.ts',
        'e2e/ui/dialog.spec.ts',
        'e2e/ui/feed-jitter.spec.ts',
        'e2e/ui/lane.spec.ts',
        'e2e/ui/low-findings-queue-targets.spec.ts',
        'e2e/ui/timeline.spec.ts',
        'e2e/ui/requests.spec.ts'
      ])
    );
  });

  it('treats_generated_api_types_as_typecheck_only', () => {
    const generated = real.areas.find(area => area.sources.includes('src/lib/api/generated/**'));
    expect(generated).toBeDefined();
    expect(generated!.specs).toEqual([]);
    expect(generated!.description).toContain('тип');
    const path = 'src/lib/api/generated/openapi.ts';
    expect(real.global.some(glob => matchesGlob(glob, path))).toBe(false);
    expect(findUncovered([path], real)).toEqual([]);
  });

  it('keeps_only_the_api_client_core_global_and_maps_wrappers_to_a_small_area', () => {
    expect(real.global.some(glob => matchesGlob(glob, 'src/lib/api/client.ts'))).toBe(true);
    expect(real.global.some(glob => matchesGlob(glob, 'src/lib/api/config.ts'))).toBe(false);
    const api = real.areas.find(area => area.name === 'api/sync');
    expect(api).toBeDefined();
    expect(api!.specs.length).toBeGreaterThan(0);
    expect(api!.specs.length).toBeLessThan(20);
  });
});
