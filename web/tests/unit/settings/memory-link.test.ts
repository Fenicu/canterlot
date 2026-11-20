import { describe, expect, it } from 'vitest';
import { ApiError } from '#lib/api/http.js';
import { LINK_NOTICE, resolveLinkedEntry, type MemoryEntry } from '#lib/screens/settings/memory.js';

const ID = '01a1b3b9-0000-7000-8000-00000000bb01';

function entry(domain: string, status = 'active'): MemoryEntry {
  return { id: ID, domain, status, subject: 'Тема', body: 'Текст', kind: 'gotcha', level: 'domain', expired: false, version: 1, created_at: '', created_by: {}, updated_at: '', updated_by: {} } as MemoryEntry;
}

const missing = () => new ApiError(404, 'not_found' as never, 'нет', 'browser', 'never', 'c-1');

/** Хранилище на два домена: запись лежит в одном из них. */
function store(domain: string, status = 'active') {
  const calls: string[] = [];
  const get = async (_id: string, asked: string) => {
    calls.push(asked);
    if (asked === domain) return entry(domain, status);
    throw missing();
  };
  return { get, calls };
}

describe('resolveLinkedEntry', () => {
  it('opens_an_entry_of_the_hinted_domain', async () => {
    const { get, calls } = store('personal');
    expect(await resolveLinkedEntry(get, ID, 'personal')).toMatchObject({ kind: 'open', domain: 'personal' });
    expect(calls).toEqual(['personal']);
  });

  it('without_a_hint_looks_in_work_then_in_personal', async () => {
    const { get, calls } = store('personal');
    expect(await resolveLinkedEntry(get, ID, null)).toMatchObject({ kind: 'open', domain: 'personal' });
    expect(calls).toEqual(['work', 'personal']);
  });

  it('a_wrong_hint_is_a_notice_and_does_not_peek_into_the_other_domain', async () => {
    const { get, calls } = store('personal');
    expect(await resolveLinkedEntry(get, ID, 'work')).toEqual({ kind: 'notice', notice: LINK_NOTICE.missing });
    expect(calls).toEqual(['work']);
  });

  it('an_unknown_id_is_a_notice', async () => {
    const { get } = store('nowhere');
    expect(await resolveLinkedEntry(get, ID, null)).toEqual({ kind: 'notice', notice: LINK_NOTICE.missing });
  });

  it('an_archived_entry_is_a_notice_that_names_the_archive', async () => {
    const { get } = store('work', 'archived');
    expect(await resolveLinkedEntry(get, ID, null)).toEqual({ kind: 'notice', notice: LINK_NOTICE.archived, domain: 'work' });
  });

  it('a_broken_hub_is_a_failure_notice_not_a_throw', async () => {
    const get = async () => {
      throw new ApiError(500, 'internal_error' as never, 'сбой', 'browser', 'never', 'c-2');
    };
    expect(await resolveLinkedEntry(get, ID, null)).toEqual({ kind: 'notice', notice: LINK_NOTICE.failed });
  });
});
