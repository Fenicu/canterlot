import type { ComponentProps } from 'svelte';
import Button from '../../src/lib/ui/Button.svelte';
type Props = ComponentProps<typeof Button>;
type Accepts<T extends Props> = T;
// @ts-expect-error Причина обязательна для отключённой кнопки.
export type MissingReason = Accepts<{ disabled: true; children: Props['children'] }>;
export type WithReason = Accepts<{ disabled: true; reason: string; children: Props['children'] }>;
