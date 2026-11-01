import { test } from 'vitest';
import { runChaosSeeds } from './chaos-shared';

test('random_chaos_never_loses_or_duplicates_events_seeds_151_200', () => runChaosSeeds(151, 200), 120_000);
