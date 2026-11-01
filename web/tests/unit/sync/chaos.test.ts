import { test } from 'vitest';
import { runChaosSeeds } from './chaos-shared';

test('random_chaos_never_loses_or_duplicates_events_seeds_1_50', () => runChaosSeeds(1, 50), 120_000);
