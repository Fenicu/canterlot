import { describe, expect, it } from 'vitest';
import { steerInterruptsStep, steerLabel, steerSupported } from '#lib/timeline/QueueActions.js';

describe('timeline/steer copy', () => {
  it('names_the_action_for_a_harness_that_steers_without_interrupting', () => {
    expect(steerLabel('claude_code')).toBe('Влить');
    expect(steerLabel('codex')).toBe('Влить');
  });

  it('says_that_steering_interrupts_the_step_for_opencode_and_antigravity', () => {
    expect(steerLabel('opencode')).toBe('Влить (прервёт шаг)');
    expect(steerLabel('antigravity')).toBe('Влить (прервёт шаг)');
    expect(steerInterruptsStep('opencode')).toBe(true);
    expect(steerInterruptsStep('antigravity')).toBe(true);
    expect(steerInterruptsStep('codex')).toBe(false);
  });

  it('offers_steering_only_where_the_hub_supports_it', () => {
    expect(steerSupported('claude_code')).toBe(true);
    expect(steerSupported('opencode')).toBe(true);
    expect(steerSupported('')).toBe(false);
    expect(steerSupported('other')).toBe(false);
  });
});
