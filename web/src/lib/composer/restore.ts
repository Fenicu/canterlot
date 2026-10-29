let restore: ((text: string) => void) | null = null;

export function bindComposerRestore(fn: (text: string) => void): () => void {
	restore = fn;
	return () => {
		if (restore === fn) restore = null;
	};
}

export function restoreComposerText(text: string): void {
	restore?.(text);
}
