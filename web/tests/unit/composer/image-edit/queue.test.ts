import { describe, expect, it, vi } from 'vitest';
import type { Size } from '#lib/composer/imageEdit/model.js';
import {
	AttachmentQueue,
	type AttachmentQueueDeps,
	type EditChoice
} from '#lib/composer/imageEdit/queue.js';

const size = { width: 100, height: 100 };

function png(name = 'screen.png'): File {
	return new File(['mock'], name, { type: 'image/png' });
}

function pdf(): File {
	return new File(['mock'], 'notes.pdf', { type: 'application/pdf' });
}

function harness(edit: AttachmentQueueDeps['edit'], decode: (file: File) => Promise<Size | null> = async () => size) {
	const add = vi.fn();
	const note = vi.fn();
	const queue = new AttachmentQueue({ decode, edit, add, note });
	return { queue, add, note };
}

async function flush(): Promise<void> {
	for (let i = 0; i < 6; i += 1) await Promise.resolve();
}

describe('attachment queue', () => {
	it('attaches non-images without opening the editor', async () => {
		const edit = vi.fn(async () => ({ choice: 'cancel' }) as EditChoice);
		const { queue, add } = harness(edit);
		const file = pdf();
		queue.enqueue([file]);
		await queue.idle();
		expect(edit).not.toHaveBeenCalled();
		expect(add).toHaveBeenCalledExactlyOnceWith(file);
	});

	it('drops an image when the editor is cancelled', async () => {
		const edit = vi.fn(async () => ({ choice: 'cancel' }) as EditChoice);
		const { queue, add } = harness(edit);
		queue.enqueue([png()]);
		await queue.idle();
		expect(edit).toHaveBeenCalledOnce();
		expect(add).not.toHaveBeenCalled();
	});

	it('passes the original file through when no edits are wanted', async () => {
		const edit = vi.fn(async () => ({ choice: 'original' }) as EditChoice);
		const { queue, add } = harness(edit);
		const file = png();
		queue.enqueue([file]);
		await queue.idle();
		expect(add).toHaveBeenCalledExactlyOnceWith(file);
	});

	it('passes the edited file through with its own name and type', async () => {
		const edited = new File(['edited'], 'screen.png', { type: 'image/png' });
		const edit = vi.fn(async () => ({ choice: 'edited', file: edited }) as EditChoice);
		const { queue, add } = harness(edit);
		queue.enqueue([png()]);
		await queue.idle();
		expect(add).toHaveBeenCalledExactlyOnceWith(edited);
	});

	it('attaches an undecodable image as is and notes why', async () => {
		const edit = vi.fn(async () => ({ choice: 'cancel' }) as EditChoice);
		const { queue, add, note } = harness(edit, async () => null);
		const file = png('broken.png');
		queue.enqueue([file]);
		await queue.idle();
		expect(edit).not.toHaveBeenCalled();
		expect(add).toHaveBeenCalledExactlyOnceWith(file);
		expect(note).toHaveBeenCalledOnce();
		expect(note.mock.calls[0]?.[0]).toContain('broken.png');
		expect(note.mock.calls[0]?.[0]).toContain('открыть');
	});

	it('attaches an oversized image as is and notes the pixel limit', async () => {
		const edit = vi.fn(async () => ({ choice: 'cancel' }) as EditChoice);
		const { queue, add, note } = harness(edit, async () => ({ width: 4096, height: 4097 }));
		queue.enqueue([png('huge.png')]);
		await queue.idle();
		expect(edit).not.toHaveBeenCalled();
		expect(add).toHaveBeenCalledOnce();
		expect(note.mock.calls[0]?.[0]).toContain('пикселей');
	});

	it('edits images one after another while attaching the rest of the batch', async () => {
		let release: (choice: EditChoice) => void = () => undefined;
		const first = new Promise<EditChoice>((resolve) => {
			release = resolve;
		});
		const edit = vi.fn(async (file: File) =>
			file.name === 'a.png' ? first : ({ choice: 'original' } as EditChoice)
		);
		const { queue, add } = harness(edit);
		const doc = pdf();
		queue.enqueue([png('a.png'), doc, png('b.png')]);
		await flush();
		expect(edit.mock.calls.map(([file]) => file.name)).toEqual(['a.png']);
		expect(add).toHaveBeenCalledExactlyOnceWith(doc);
		release({ choice: 'cancel' });
		await queue.idle();
		expect(edit.mock.calls.map(([file]) => file.name)).toEqual(['a.png', 'b.png']);
	});
});
