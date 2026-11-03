import { describe, expect, it } from "vitest";
import { RenderCache, StreamingMarkdown } from "#lib/render/streamingMarkdown.js";

describe("final markdown cache", () => {
  it("reuses_the_rendered_html_for_the_same_block_revision", () => {
    const cache = new RenderCache();
    let renders = 0;
    const renderer = {
      render(text: string): string {
        renders++;
        return `<p>${text}</p>`;
      },
    };
    const first = new StreamingMarkdown(renderer, cache);
    expect(first.update("ответ", true, "b1:7").chunks[0]!.html).toBe("<p>ответ</p>");

    const remounted = new StreamingMarkdown(renderer, cache);
    expect(remounted.update("ответ", true, "b1:7").chunks[0]!.html).toBe(
      "<p>ответ</p>",
    );
    expect(renders).toBe(1);
  });

  it("does_not_share_html_between_block_revisions", () => {
    const cache = new RenderCache();
    let renders = 0;
    const renderer = {
      render(text: string): string {
        renders++;
        return text;
      },
    };
    new StreamingMarkdown(renderer, cache).update("старое", true, "b1:6");
    new StreamingMarkdown(renderer, cache).update("новое", true, "b1:7");
    expect(renders).toBe(2);
  });

  it("bounds_what_is_kept_by_entries_and_bytes", () => {
    const cache = new RenderCache(3, 1_000);
    for (let index = 0; index < 5; index++)
      cache.set(`b${index}`, "x".repeat(100));
    expect(cache.size).toBe(3);
    expect(cache.get("b0")).toBeUndefined();
    expect(cache.get("b4")).toBe("x".repeat(100));

    const bytes = new RenderCache(10, 300);
    bytes.set("big", "x".repeat(200));
    bytes.set("small", "x".repeat(60));
    expect(bytes.bytes).toBeLessThanOrEqual(300);
    expect(bytes.get("big")).toBeUndefined();
    expect(bytes.get("small")).toBe("x".repeat(60));
  });
});
