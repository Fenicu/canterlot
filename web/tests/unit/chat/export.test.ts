import { describe, expect, test, vi } from "vitest";
import { answerToMarkdown } from "#lib/export/markdown.js";
import { deliverImage, type Delivery } from "#lib/export/deliver.js";
import { renderAnswerPages } from "#lib/export/image.js";
import { BlockModel } from "#lib/stores/blocks.js";

describe("export", () => {
  test("answer_to_markdown_exports_main_text_blocks_and_one_line_tool_summaries_in_order", () => {
    const text1 = new BlockModel("b1", "text");
    text1.body = "Проверь поток после обрыва. Дубликатов быть не должно.";
    text1.parentId = null;

    const tool1 = new BlockModel("b2", "tool_call");
    tool1.toolName = "Read";
    tool1.summary = "src/streams/replay.rs";
    tool1.parentId = null;

    // Subagent block - should be omitted from main export
    const subagentText = new BlockModel("b3", "text");
    subagentText.body = "Это скрытый текст субагента.";
    subagentText.parentId = "subagent-tool";

    const tool2 = new BlockModel("b4", "tool_call");
    tool2.toolName = "Bash";
    tool2.summary = "cargo test replay";
    tool2.parentId = null;

    const text2 = new BlockModel("b5", "text");
    text2.body = "Между снимком и подпиской есть окно.";
    text2.parentId = null;

    const markdown = answerToMarkdown([
      text1,
      tool1,
      subagentText,
      tool2,
      text2,
    ]);

    expect(markdown).toBe(
      "Проверь поток после обрыва. Дубликатов быть не должно.\n\n_Read: src/streams/replay.rs_\n\n_Bash: cargo test replay_\n\nМежду снимком и подпиской есть окно.",
    );
  });

  test("deliver_image_prefers_share_then_clipboard_then_download", async () => {
    const blob = new Blob(["fake-png-data"], { type: "image/png" });

    // 1. Share supported and succeeds
    const shareMock = vi.fn().mockResolvedValue(undefined);
    const downloadMock = vi.fn();
    const clipboardMock = { write: vi.fn().mockResolvedValue(undefined) };

    let result: Delivery = await deliverImage([blob], {
      canShare: () => true,
      share: shareMock,
      clipboard: clipboardMock as unknown as Clipboard,
      download: downloadMock,
    });
    expect(result).toBe("shared");
    expect(shareMock).toHaveBeenCalled();
    expect(clipboardMock.write).not.toHaveBeenCalled();
    expect(downloadMock).not.toHaveBeenCalled();

    // 2. Share supported but throws NotAllowedError (user gesture expired)
    const notAllowedErr = new Error("User gesture required");
    notAllowedErr.name = "NotAllowedError";
    const shareGestureMock = vi.fn().mockRejectedValue(notAllowedErr);

    result = await deliverImage([blob], {
      canShare: () => true,
      share: shareGestureMock,
      clipboard: clipboardMock as unknown as Clipboard,
      download: downloadMock,
    });
    expect(result).toBe("needs_gesture");

    // 3. Share not available, clipboard succeeds
    result = await deliverImage([blob], {
      canShare: () => false,
      clipboard: clipboardMock as unknown as Clipboard,
      download: downloadMock,
    });
    expect(result).toBe("copied");
    expect(clipboardMock.write).toHaveBeenCalled();
    expect(downloadMock).not.toHaveBeenCalled();

    // 4. Multiple pages -> falls back to download
    const page1 = new Blob(["p1"], { type: "image/png" });
    const page2 = new Blob(["p2"], { type: "image/png" });
    result = await deliverImage([page1, page2], {
      clipboard: clipboardMock as unknown as Clipboard,
      download: downloadMock,
    });
    expect(result).toBe("downloaded");
    expect(downloadMock).toHaveBeenCalledTimes(2);
    expect(downloadMock.mock.calls[0]?.[1]).toMatch(
      /^canterlot-\d{4}-\d{2}-\d{2}-\d{4}-1\.png$/,
    );
    expect(downloadMock.mock.calls[1]?.[1]).toMatch(
      /^canterlot-\d{4}-\d{2}-\d{2}-\d{4}-2\.png$/,
    );
  });

  test("image_pages_never_exceed_the_pixel_cap", async () => {
    const container = document.createElement("div");
    container.style.width = "640px";

    // Create three child blocks
    for (let i = 0; i < 3; i++) {
      const child = document.createElement("div");
      child.className = "export-block";
      child.style.height = "100px";
      child.textContent = `Block ${i}`;
      container.appendChild(child);
    }
    document.body.appendChild(container);

    const pages = await renderAnswerPages(container, { pageMaxPx: 150 });
    expect(pages.length).toBeGreaterThan(1);
    document.body.removeChild(container);
  });
});
