import { test } from 'vitest';
import { runChaosSeeds } from './chaos-shared';

test('random_chaos_never_loses_or_duplicates_events_seeds_51_100', () => runChaosSeeds(51, 100), 120_000);
