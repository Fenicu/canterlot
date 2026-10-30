export interface SyntheticStep {
  [key: string]: unknown;
}

export const synthetic = {
  delta(append: string): SyntheticStep {
    return { delta: append };
  },
  delayMs(ms: number): SyntheticStep {
    return { delay_ms: ms };
  },
  tool(name: string, summary?: string): SyntheticStep {
    return { tool: summary ? { name, summary } : { name } };
  },
  model(name: string): SyntheticStep {
    return { model: name };
  },
  todo(items: { text: string; status: 'pending' | 'in_progress' | 'completed' }[]): SyntheticStep {
    return { todo: items };
  },
  subagent(name: string, model: string, task: string, steps: SyntheticStep[]): SyntheticStep {
    return { subagent: { name, model, task, steps } };
  },
  usage(opts: { model?: string; agent?: string; input: number; output: number; cache_read?: number; cache_write?: number }): SyntheticStep {
    return {
      usage: {
        model: opts.model ?? 'synthetic-small',
        agent: opts.agent ?? 'main',
        input: opts.input,
        output: opts.output,
        cache_read: opts.cache_read ?? 0,
        cache_write: opts.cache_write ?? 0
      }
    };
  },
  limitObserved(window: 'five_hour' | 'weekly', used_permille: number, resets_in_ms: number): SyntheticStep {
    return { limit_observed: { window, used_permille, resets_in_ms } };
  },
  limitReached(window: 'five_hour' | 'weekly', resets_in_ms: number): SyntheticStep {
    return { limit_reached: { window, resets_in_ms } };
  },
  readAttachment(index: number): SyntheticStep {
    return { read_attachment: index };
  },
  hang(): SyntheticStep {
    return { hang: true };
  },
  script(steps: SyntheticStep[]): string {
    return JSON.stringify({ synthetic: steps });
  }
};
