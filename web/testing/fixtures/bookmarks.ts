import type { FakeHubApi } from '../fake-hub/api';
import { seedConversation, seedHistory } from '../fake-hub/seed';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';

const CLAUDE = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };
const CODEX = { id: 'codex', title: 'Codex', provider: 'codex' };
const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

export const DEEP_BOOKMARK_TEXT =
  'Глубокий абзац про индекс: он строится до старта хаба и не держит деплой';
export const CARD_FIRST_TEXT = 'Первый абзац ответа: проверяем миграцию';
export const CARD_LAST_TEXT =
  'Последний абзац ответа: закладка встаёт именно на этот блок';
export const OWNER_BOOKMARK_TEXT =
  'Проверь, что индекс строится до старта и не держит деплой';

export interface BookmarksSeed {
	/** Разговор с длинной лентой, карточкой из двух блоков и сообщением владельца. */
	main: string;
	deepBlockId: string;
	deepSeq: number;
	ownerInputId: string;
	cardFirstId: string;
	cardLastId: string;
	cardRun: string;
	/** Архивная сессия для общего списка и перехода. */
	archived: string;
	archivedBlockId: string;
	/** Сессия с двадцатью двумя закладками: вторая страница общего списка. */
	journal: string;
	journalBlockIds: string[];
}

function block(
	api: FakeHubApi,
	seq: number,
	text: string,
	createdAt: string,
	run: string | null = null
): HistoryBlock {
	const id = api.id();
	api.blocks.set(id, { text, rev: 1, state: 'final' });
	return {
		kind: 'block',
		id,
		block_kind: 'text',
		content: text,
		content_bytes: text.length,
		content_truncated: false,
		created_seq: String(seq),
		created_at: createdAt,
		rev: 1,
		state: 'final',
		agent: 'agent',
		model: 'opus-5.5',
		tool_name: null,
		summary: null,
		parent_block_id: null,
		input_command_id: run,
		line_count: 1
	};
}

function ownerInput(api: FakeHubApi, seq: number, text: string, createdAt: string): HistoryInput {
	return {
		kind: 'input',
		id: api.id(),
		idempotency_key: `bookmark-${seq}-${api.id()}`,
		state: 'accepted',
		content: [{ type: 'text', text }],
		attachments: [],
		created_seq: String(seq),
		feed_seq: String(seq),
		created_at: createdAt,
		retry_of: null,
		origin: null,
		revision: 1
	};
}

/**
 * Лента для избранного: длинная история, чтобы закладка далеко наверху открывалась
 * через окно вокруг места, карточка ответа из двух верхних блоков и архивная сессия.
 * Сами закладки в основном разговоре ставят тесты: так пустое состояние и первое
 * нажатие проверяются на чистой ленте.
 */
export function seedBookmarks(api: FakeHubApi, options: { global?: boolean } = {}): BookmarksSeed {
	const main = seedConversation(api, {
		title: 'Миграция поиска',
		harness: CLAUDE,
		model: 'opus-5.5',
		last_activity_at: ago(2)
	});
	const history = seedHistory(api, main, 160);
	history.forEach((item, index) => {
		item.created_at = ago(160 - index);
	});
	const deep = history[11]!;
	deep.content = DEEP_BOOKMARK_TEXT;
	deep.content_bytes = DEEP_BOOKMARK_TEXT.length;
	deep.created_at = ago(150);
	api.blocks.set(deep.id, { text: DEEP_BOOKMARK_TEXT, rev: 1, state: 'final' });
	const owner = ownerInput(api, 161, OWNER_BOOKMARK_TEXT, ago(1));
	const run = 'bookmark-card';
	const first = block(api, 170, CARD_FIRST_TEXT, ago(0.5), run);
	const last = block(api, 171, CARD_LAST_TEXT, ago(0.4), run);
	api.histories.set(main, [...history, owner, first, last]);
	api.bindConversation(main);

	const archived = seedConversation(api, {
		title: 'Ревью схемы',
		harness: CODEX,
		model: 'gpt-5.5-codex',
		archived: true,
		last_activity_at: ago(3 * 24 * 60)
	});
	const archivedBlock = block(api, 1, 'Архивный ответ про индекс и схему', ago(3 * 24 * 60));
	api.histories.set(archived, [archivedBlock]);
	api.bindConversation(archived);

	const journal = seedConversation(api, {
		title: 'Журнал закладок',
		harness: CLAUDE,
		model: 'haiku',
		last_activity_at: ago(30)
	});
	const journalBlocks = seedHistory(api, journal, 22);
	journalBlocks.forEach((item, index) => {
		const text = `Запись ${index + 1}: журнал проверок и закладок`;
		item.content = text;
		item.content_bytes = text.length;
		item.created_at = ago(200 + index);
		api.blocks.set(item.id, { text, rev: 1, state: 'final' });
	});

	if (options.global !== false) {
		journalBlocks.forEach((item, index) => {
			api.putBookmark(journal, 'block', item.id, { at: ago(200 + index), emit: false });
		});
		api.putBookmark(archived, 'block', archivedBlock.id, { at: ago(2000), emit: false });
	}

	return {
		main,
		deepBlockId: deep.id,
		deepSeq: Number(deep.created_seq),
		ownerInputId: owner.id,
		cardFirstId: first.id,
		cardLastId: last.id,
		cardRun: run,
		archived,
		archivedBlockId: archivedBlock.id,
		journal,
		journalBlockIds: journalBlocks.map((item) => item.id)
	};
}
