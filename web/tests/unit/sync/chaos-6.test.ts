import { test } from 'vitest';
import { runChaosSeeds } from './chaos-shared';

test('random_chaos_never_loses_or_duplicates_events_seeds_251_300', () => runChaosSeeds(251, 300), 120_000);
