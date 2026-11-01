import { test } from 'vitest';
import { runChaosSeeds } from './chaos-shared';

test('random_chaos_never_loses_or_duplicates_events_seeds_201_250', () => runChaosSeeds(201, 250), 120_000);
