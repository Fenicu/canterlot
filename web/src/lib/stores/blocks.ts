import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import type { HubEvent } from "#lib/sync/wire.js";
import { cell } from "./cell.svelte";

export type BlockKind =
  "text" | "reasoning" | "tool_call" | "tool_result" | "warning" | "error";
export type BlockStateName = "streaming" | "stale" | "final" | "incomplete";

const encoder = new TextEncoder();

export function utf8Length(value: string): number {
  for (let i = 0; i < value.length; i++) {
    if (value.charCodeAt(i) > 0x7f) return encoder.encode(value).length;
  }
  return value.length;
}

export function utf8Slice(value: string, offset: number): string {
  if (offset === 0) return value;
  const bytes = encoder.encode(value);
  if (offset > bytes.length) throw new Error("invalid_offset");
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(
      bytes.subarray(0, offset),
    );
  } catch {
    throw new Error("invalid_offset");
  }
  return value.slice(text.length);
}

export function sha256Hex(text: string): Promise<string> {
  return Promise.resolve(bytesToHex(sha256(encoder.encode(text))));
}

export interface BlockPage {
  content: string;
  offset: number;
  next_offset?: number | null;
  rev: number;
  state: string;
  total_bytes: number;
  checksum?: string | null;
}

export interface BlockIO {
  fetchContent(blockId: string, offsetBytes: number): Promise<BlockPage>;
  sha256Hex(text: string): Promise<string>;
}

export type BlockEffect =
  | { type: "refetch"; blockId: string; fromBytes: number }
  | { type: "verify"; blockId: string }
  | {
      type: "diag";
      kind:
        | "block_revision_gap"
        | "block_checksum_mismatch"
        | "delta_after_final"
        | "block_fetch_failed";
      blockId: string;
    };

export interface PendingDelta {
  base: number;
  rev: number;
  append: string;
}

export interface BlockMedia {
  media_id: string;
  mime: string;
  width: number;
  height: number;
  ready: boolean;
  failed?: boolean;
}

export class BlockModel {
  readonly id: string;
  readonly kind: BlockKind;
  parentId: string | null = null;
  agent = "";
  model = "";
  toolName: string | null = null;
  summary: string | null = null;
  inputCommandId: string | null = null;
  createdSeq = "0";
  createdAt = 0;
  startedAt: number | null = null;
  finishedAt: number | null = null;
  lineCount: number | null = null;
  contentBytes: number | null = null;
  checksum: string | null = null;
  bytes = 0;
  body = "";
  sawGap = false;
  ticket = 0;
  pending: PendingDelta[] = [];

  #text = cell("");
  #rev = cell(0);
  #state = cell<BlockStateName>("streaming");
  #reason = cell<string | null>(null);
  #truncated = cell(false);
  #media = cell<BlockMedia[]>([]);

  constructor(id: string, kind: BlockKind) {
    this.id = id;
    this.kind = kind;
  }

  get text(): string {
    return this.#text.get();
  }
  set text(value: string) {
    this.#text.set(value);
    this.lineCount = countLines(value);
  }
  get rev(): number {
    return this.#rev.get();
  }
  set rev(value: number) {
    this.#rev.set(value);
  }
  get state(): BlockStateName {
    return this.#state.get();
  }
  set state(value: BlockStateName) {
    this.#state.set(value);
  }
  get incompleteReason(): string | null {
    return this.#reason.get();
  }
  set incompleteReason(value: string | null) {
    this.#reason.set(value);
  }
  get truncated(): boolean {
    return this.#truncated.get();
  }
  set truncated(value: boolean) {
    this.#truncated.set(value);
  }
  get media(): BlockMedia[] {
    return this.#media.get();
  }
  set media(value: BlockMedia[]) {
    this.#media.set(value);
  }
}

/** Как `str::lines` хаба: перевод строки в конце новой строки не открывает. */
function countLines(value: string): number {
  if (value.length === 0) return 0;
  const breaks = value.split("\n").length;
  return value.endsWith("\n") ? breaks - 1 : breaks;
}

function display(block: BlockModel): string {
  if (block.kind === "tool_call" && block.state !== "final")
    return block.summary ?? "";
  return block.body;
}

function publish(block: BlockModel): void {
  block.text = display(block);
}

function appendBody(block: BlockModel, chunk: string): void {
  block.body += chunk;
  block.bytes += utf8Length(chunk);
  publish(block);
}

function replaceBody(block: BlockModel, content: string): void {
  block.body = content;
  block.bytes = utf8Length(content);
  publish(block);
}

export function applyBlockEvent(
  blocks: Map<string, BlockModel>,
  ev: HubEvent,
): BlockEffect[] {
  const kind = ev.kind;
  if (
    kind !== "block_started" &&
    kind !== "block_delta" &&
    kind !== "block_final" &&
    kind !== "block_incomplete"
  )
    return [];
  if (kind === "block_started") return startBlock(blocks, ev);
  if (kind === "block_delta") return applyDelta(blocks, ev);
  if (kind === "block_final") return applyFinal(blocks, ev);
  return applyIncomplete(blocks, ev);
}

function at(ev: HubEvent): number {
  const parsed = Date.parse(ev.committed_at);
  return Number.isNaN(parsed) ? 0 : parsed;
}

function startBlock(
  blocks: Map<string, BlockModel>,
  ev: HubEvent,
): BlockEffect[] {
  if (ev.kind !== "block_started") return [];
  const payload = ev.payload;
  if (blocks.has(payload.block_id)) return [];
  const block = new BlockModel(payload.block_id, payload.kind);
  block.parentId = payload.parent_block_id;
  block.agent = payload.agent;
  block.model = payload.model;
  block.toolName = payload.tool_name;
  block.summary = payload.summary;
  block.inputCommandId = payload.input_command_id;
  block.createdSeq = ev.scope_seq;
  block.createdAt = at(ev);
  block.startedAt = at(ev);
  block.rev = payload.rev;
  publish(block);
  blocks.set(block.id, block);
  return [];
}

function ensure(blocks: Map<string, BlockModel>, id: string): BlockModel {
  let block = blocks.get(id);
  if (!block) {
    block = new BlockModel(id, "text");
    blocks.set(id, block);
  }
  return block;
}

function applyDelta(
  blocks: Map<string, BlockModel>,
  ev: HubEvent,
): BlockEffect[] {
  if (ev.kind !== "block_delta") return [];
  const payload = ev.payload;
  const block = blocks.get(payload.block_id);
  if (!block) return [];
  if (block.state === "final")
    return [{ type: "diag", kind: "delta_after_final", blockId: block.id }];
  if (block.state === "incomplete") return [];
  const delta: PendingDelta = {
    base: payload.base_rev,
    rev: payload.rev,
    append: payload.append,
  };
  if (block.state === "stale") {
    block.pending.push(delta);
    return [];
  }
  if (payload.rev <= block.rev) return [];
  if (payload.base_rev === block.rev) {
    block.rev = payload.rev;
    appendBody(block, payload.append);
    return [];
  }
  block.pending.push(delta);
  block.state = "stale";
  block.sawGap = true;
  const fromBytes = payload.base_rev > block.rev ? 0 : block.bytes;
  const effects: BlockEffect[] = [
    { type: "refetch", blockId: block.id, fromBytes },
  ];
  if (payload.base_rev > block.rev)
    effects.push({
      type: "diag",
      kind: "block_revision_gap",
      blockId: block.id,
    });
  return effects;
}

function applyFinal(
  blocks: Map<string, BlockModel>,
  ev: HubEvent,
): BlockEffect[] {
  if (ev.kind !== "block_final") return [];
  const payload = ev.payload;
  const block = ensure(blocks, payload.block_id);
  block.finishedAt = at(ev);
  block.checksum = payload.checksum;
  // Хаб оставляет сводку начала блока, если финал своей не принёс.
  block.summary = payload.summary ?? block.summary;
  if (payload.text_omitted) {
    block.rev = payload.rev;
    if (block.sawGap || block.state === "stale") {
      block.state = "stale";
      block.sawGap = true;
      return [{ type: "refetch", blockId: block.id, fromBytes: 0 }];
    }
    return [{ type: "verify", blockId: block.id }];
  }
  ++block.ticket;
  block.pending.length = 0;
  block.sawGap = false;
  block.rev = payload.rev;
  block.state = "final";
  replaceBody(block, payload.text);
  return [{ type: "verify", blockId: block.id }];
}

function applyIncomplete(
  blocks: Map<string, BlockModel>,
  ev: HubEvent,
): BlockEffect[] {
  if (ev.kind !== "block_incomplete") return [];
  const block = ensure(blocks, ev.payload.block_id);
  ++block.ticket;
  block.rev = ev.payload.rev;
  block.state = "incomplete";
  block.incompleteReason = ev.payload.reason;
  block.finishedAt = at(ev);
  publish(block);
  return [];
}

export function installPage(
  block: BlockModel,
  page: BlockPage,
  fromBytes: number,
): void {
  if (block.state === "final") return;
  if (block.state === "incomplete" && page.state !== "final") return;
  if (fromBytes === 0) replaceBody(block, page.content);
  else appendBody(block, page.content);
  block.rev = page.rev;
  block.bytes =
    fromBytes === 0
      ? utf8Length(page.content)
      : fromBytes + utf8Length(page.content);
  if (page.checksum) block.checksum = page.checksum;
  if (
    page.state === "final" ||
    page.state === "incomplete" ||
    page.state === "streaming" ||
    page.state === "stale"
  ) {
    block.state = page.state;
  } else if (block.state === "stale") block.state = "streaming";
  if (fromBytes === 0) block.sawGap = false;
  publish(block);
}

type Schedule = (fn: () => void, ms: number) => () => void;

const defaultSchedule: Schedule = (fn, ms) => {
  const timer = setTimeout(fn, ms);
  return () => clearTimeout(timer);
};

export class BlockSync {
  readonly diagnostics: BlockEffect[] = [];
  #chain: Promise<void> = Promise.resolve();
  #schedule: Schedule;
  #attempts = new Map<string, number>();
  #timers = new Set<() => void>();
  #stopped = false;

  constructor(
    private blocks: Map<string, BlockModel>,
    private io: BlockIO,
    opts: { schedule?: Schedule } = {},
  ) {
    this.#schedule = opts.schedule ?? defaultSchedule;
  }

  handle(effects: BlockEffect[]): void {
    if (effects.length === 0 || this.#stopped) return;
    this.#chain = this.#chain.then(() => this.#handle(effects));
  }

  stop(): void {
    this.#stopped = true;
    for (const cancel of this.#timers) cancel();
    this.#timers.clear();
    this.#attempts.clear();
  }

  get settled(): Promise<void> {
    return this.#chain;
  }

  #diag(effect: BlockEffect): void {
    this.diagnostics.push(effect);
    if (this.diagnostics.length > 200)
      this.diagnostics.splice(0, this.diagnostics.length - 200);
  }

  async #handle(effects: BlockEffect[]): Promise<void> {
    for (const effect of effects) {
      if (effect.type === "diag") {
        this.#diag(effect);
        continue;
      }
      try {
        if (effect.type === "verify") await this.#verify(effect.blockId);
        else await this.#refetch(effect.blockId, effect.fromBytes);
        this.#attempts.delete(effect.blockId);
      } catch {
        this.#retry(effect);
      }
    }
  }

  /** Сбой загрузки не останавливает цепочку: блок повторяется позже, остальные идут своим ходом. */
  #retry(effect: Exclude<BlockEffect, { type: "diag" }>): void {
    if (this.#stopped || !this.blocks.has(effect.blockId)) return;
    const attempt = (this.#attempts.get(effect.blockId) ?? 0) + 1;
    this.#attempts.set(effect.blockId, attempt);
    this.#diag({ type: "diag", kind: "block_fetch_failed", blockId: effect.blockId });
    const cancel = this.#schedule(
      () => {
        this.#timers.delete(cancel);
        this.handle([effect]);
      },
      Math.min(1000 * 2 ** (attempt - 1), 30_000),
    );
    this.#timers.add(cancel);
  }

  async #verify(blockId: string): Promise<void> {
    const block = this.blocks.get(blockId);
    if (!block || block.checksum === null) return;
    const hex = await this.io.sha256Hex(block.body);
    if (hex === block.checksum) {
      block.state = "final";
      publish(block);
      return;
    }
    this.#diag({
      type: "diag",
      kind: "block_checksum_mismatch",
      blockId,
    });
    block.state = "stale";
    block.sawGap = true;
    await this.#refetch(blockId, 0);
  }

  async #refetch(blockId: string, fromBytes: number): Promise<void> {
    const block = this.blocks.get(blockId);
    if (!block) return;
    const mine = ++block.ticket;
    let offset = fromBytes;
    let first = true;
    for (;;) {
      const page = await this.io.fetchContent(blockId, offset);
      if (block.ticket !== mine) return;
      installPage(block, page, first ? fromBytes : offset);
      first = false;
      const next = page.next_offset ?? offset + utf8Length(page.content);
      if (next <= offset || next >= page.total_bytes) break;
      offset = next;
    }
    if (block.ticket !== mine) return;
    await this.#replay(block);
  }

  async #replay(block: BlockModel): Promise<void> {
    const pending = block.pending.splice(0).sort((a, b) => a.rev - b.rev);
    for (let i = 0; i < pending.length; i++) {
      const delta = pending[i];
      if (!delta || delta.rev <= block.rev) continue;
      if (block.state === "final") {
        this.#diag({
          type: "diag",
          kind: "delta_after_final",
          blockId: block.id,
        });
        continue;
      }
      if (delta.base !== block.rev) {
        block.pending.push(...pending.slice(i));
        block.state = "stale";
        block.sawGap = true;
        const from = delta.base > block.rev ? 0 : block.bytes;
        await this.#refetch(block.id, from);
        return;
      }
      block.rev = delta.rev;
      appendBody(block, delta.append);
    }
    if (block.state === "stale") {
      block.state = "streaming";
      publish(block);
    }
  }
}

export const BLOCK_PAGE_BYTES = 256 * 1024;

export async function* pagesOf(
  blockId: string,
  text: string,
  io: BlockIO,
): AsyncIterable<BlockPage> {
  let offset = 0;
  const total = utf8Length(text);
  while (offset < total) {
    const page = await io.fetchContent(blockId, offset);
    yield page;
    const next = page.next_offset ?? offset + utf8Length(page.content);
    if (next <= offset) break;
    offset = next;
  }
}
