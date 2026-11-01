import { test } from 'vitest';
import { runChaosSeeds } from './chaos-shared';

test('random_chaos_never_loses_or_duplicates_events_seeds_101_150', () => runChaosSeeds(101, 150), 120_000);
