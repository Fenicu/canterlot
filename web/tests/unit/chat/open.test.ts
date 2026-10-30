import { afterEach, expect, test, vi } from "vitest";
import type { ChatStore } from "#lib/stores/chat.svelte.js";

interface FakeSub {
  scope: string;
  released: boolean;
  readonly status: "live";
  readonly cursor: string;
  release(): void;
}

function fakeRuntime(failures = 0) {
  const subs: FakeSub[] = [];
  let failing = failures;
  const connection = {
    subscribe: vi.fn((scope: string) => {
      if (failing > 0) {
        failing -= 1;
        throw new Error("too_many_subscriptions");
      }
      const sub: FakeSub = {
        scope,
        released: false,
        status: "live",
        cursor: "0",
        release() {
          sub.released = true;
        },
      };
      subs.push(sub);
      return sub;
    }),
  };
  const runtime = {
    db: {},
    journal: { ops: new Map(), settle() {}, onOrphanRefused: () => () => undefined },
    drafts: {},
    connection,
  };
  vi.doMock("#lib/app/runtime.js", () => ({
    getRuntime: () => runtime,
    createRuntime: async () => runtime,
  }));
  return { subs, connection };
}

async function load() {
  return import("../../../src/lib/screens/chat/open");
}

afterEach(() => {
  vi.doUnmock("#lib/app/runtime.js");
  vi.resetModules();
});

test("failed_open_is_not_cached_and_the_next_lease_subscribes_again", async () => {
  const { connection, subs } = fakeRuntime(1);
  const { leaseChatSession } = await load();
  const first = leaseChatSession("c1");
  await expect(first.chat).rejects.toThrow("too_many_subscriptions");
  first.release();
  const second = leaseChatSession("c1");
  const chat = await second.chat;
  expect(chat.status).toBe("live");
  expect(connection.subscribe).toHaveBeenCalledTimes(2);
  expect(subs.filter((sub) => !sub.released)).toHaveLength(1);
  second.release();
  expect(subs.every((sub) => sub.released)).toBe(true);
});

test("released_chats_unsubscribe_and_only_a_few_stay_warm", async () => {
  const { subs } = fakeRuntime();
  const { leaseChatSession } = await load();
  const seen = new Map<string, ChatStore>();
  for (let n = 0; n < 10; n++) {
    const lease = leaseChatSession(`c${n}`);
    seen.set(`c${n}`, await lease.chat);
    lease.release();
  }
  expect(subs.every((sub) => sub.released)).toBe(true);
  const recent = leaseChatSession("c9");
  expect(await recent.chat).toBe(seen.get("c9"));
  const evicted = leaseChatSession("c0");
  const reopened = await evicted.chat;
  expect(reopened).not.toBe(seen.get("c0"));
  expect(reopened.status).toBe("live");
  recent.release();
  evicted.release();
});

test("held_chat_is_never_evicted_while_others_come_and_go", async () => {
  fakeRuntime();
  const { leaseChatSession } = await load();
  const held = leaseChatSession("main");
  const main = await held.chat;
  for (let n = 0; n < 10; n++) {
    const lease = leaseChatSession(`c${n}`);
    await lease.chat;
    lease.release();
  }
  const again = leaseChatSession("main");
  expect(await again.chat).toBe(main);
  expect(main.status).toBe("live");
  again.release();
  held.release();
});
