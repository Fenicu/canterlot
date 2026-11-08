import type { FakeHubApi } from '../fake-hub/api';
import { seedConversation, seedHistory, seedInput } from '../fake-hub/seed';
import type { HistoryBlock, HistoryInput } from '../../src/lib/stores/chat.svelte';

const CLAUDE = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };
const CODEX = { id: 'codex', title: 'Codex', provider: 'codex' };
const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

export const DEEP_TEXT = 'Индекс GIN по ответам готов: миграция создаёт его до старта хаба, поиск идёт по свежим первыми';

export interface SearchSeed {
	/** Сессия с длинной лентой: совпадение глубоко в истории, прыжок идёт через окно вокруг seq. */
	migration: string;
	deepBlockId: string;
	deepSeq: number;
	ownerInputId: string;
	/** Архивная сессия с одним ответом про индекс. */
	archived: string;
	/** Оркестратор: итог делегата и сообщение сессии. */
	orchestrator: string;
	/** Сессия с 45 ответами «проверка N» для подгрузки страниц. */
	journal: string;
}

function textBlock(api: FakeHubApi, seq: number, text: string, createdAt: string): HistoryBlock {
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
		input_command_id: null,
		line_count: 1
	};
}

function input(api: FakeHubApi, seq: number, text: string, createdAt: string, origin: HistoryInput['origin'] = null): HistoryInput {
	return {
		kind: 'input',
		id: api.id(),
		idempotency_key: `search-${seq}-${api.id()}`,
		state: 'accepted',
		content: [{ type: 'text', text }],
		attachments: [],
		created_seq: String(seq),
		feed_seq: String(seq),
		created_at: createdAt,
		retry_of: null,
		origin,
		revision: 1
	};
}

/**
 * Сессии для поиска по истории: совпадения разного происхождения и давности,
 * архивная сессия и длинная лента, где цель лежит далеко от хвоста.
 */
export function seedSearch(api: FakeHubApi): SearchSeed {
	const migration = seedConversation(api, { title: 'Миграция поиска', harness: CLAUDE, model: 'opus-5.5', last_activity_at: ago(2) });
	const history = seedHistory(api, migration, 160);
	history.forEach((block, index) => {
		block.created_at = ago(160 - index);
	});
	const deep = history[11]!;
	deep.content = DEEP_TEXT;
	deep.content_bytes = DEEP_TEXT.length;
	deep.created_at = ago(150);
	api.blocks.set(deep.id, { text: DEEP_TEXT, rev: 1, state: 'final' });
	const owner = input(api, 161, 'Проверь, что индекс строится до старта и не держит деплой', ago(1));
	seedInput(api, migration, owner);

	const archived = seedConversation(api, { title: 'Ревью схемы', harness: CODEX, model: 'gpt-5.5-codex', archived: true, last_activity_at: ago(3 * 24 * 60) });
	api.histories.set(archived, [textBlock(api, 1, 'Частичный индекс по блокам текста закрывает стриминг: правка на финализации одна', ago(3 * 24 * 60))]);
	api.bindConversation(archived);

	const orchestrator = seedConversation(api, { title: 'Оркестратор поиска', harness: CLAUDE, model: 'sonnet-5', last_activity_at: ago(20 * 24 * 60) });
	api.histories.set(orchestrator, [
		input(api, 1, 'Собери план: индекс, запрос, экран поиска', ago(20 * 24 * 60 + 5), { kind: 'spawn', from: migration, title: 'Миграция поиска', chain_depth: 1 }),
		input(api, 2, 'Итог: индекс и запрос готовы, экран поиска на проверке', ago(20 * 24 * 60), { kind: 'event', from: migration, title: 'Миграция поиска', chain_depth: 1, event: { kind: 'session_result', state: 'idle', session_id: migration, commits: [] } as never }),
		input(api, 3, 'Сессия сообщает: индекс перестроен после правки функции', ago(20 * 24 * 60 - 5), { kind: 'message', from: migration, title: 'Миграция поиска', chain_depth: 1 })
	]);
	api.bindConversation(orchestrator);

	const journal = seedConversation(api, { title: 'Журнал проверок', harness: CLAUDE, model: 'haiku', last_activity_at: ago(30) });
	api.histories.set(
		journal,
		Array.from({ length: 45 }, (_, index) => textBlock(api, index + 1, `Проверка ${index + 1}: гейт зелёный, замечаний нет`, ago(400 - index * 5)))
	);
	api.bindConversation(journal);

	return { migration, deepBlockId: deep.id, deepSeq: Number(deep.created_seq), ownerInputId: owner.id, archived, orchestrator, journal };
}
