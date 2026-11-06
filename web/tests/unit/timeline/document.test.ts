import { afterEach, describe, expect, it, vi } from "vitest";
import { DOCUMENT_VIEW_MAX_BYTES } from "#lib/timeline/attachments.js";
import { DocumentTooLarge, decodeDocument, fetchDocumentText } from "#lib/timeline/document.js";

function response(bytes: Uint8Array, length?: number) {
  return {
    ok: true,
    headers: { get: (name: string) => (name === "content-length" ? (length === undefined ? null : String(length)) : null) },
    arrayBuffer: async () => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength)
  };
}

describe("timeline/document", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("decodes_utf8_and_strips_the_byte_order_mark", () => {
    const text = new TextEncoder().encode("Привет, мир");
    const withBom = new Uint8Array([0xef, 0xbb, 0xbf, ...text]);
    expect(decodeDocument(withBom)).toBe("Привет, мир");
    expect(decodeDocument(text)).toBe("Привет, мир");
  });

  it("requests_the_attachment_with_the_site_cookie", async () => {
    const fetchMock = vi.fn(async () => response(new TextEncoder().encode("# Отчёт")));
    vi.stubGlobal("fetch", fetchMock);
    await expect(fetchDocumentText("/api/v1/attachments/a/content")).resolves.toBe("# Отчёт");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/attachments/a/content",
      expect.objectContaining({ credentials: "same-origin" })
    );
  });

  it("refuses_what_the_length_header_already_calls_too_big", async () => {
    const fetchMock = vi.fn(async () => response(new TextEncoder().encode("x"), DOCUMENT_VIEW_MAX_BYTES + 1));
    vi.stubGlobal("fetch", fetchMock);
    await expect(fetchDocumentText("/a")).rejects.toBeInstanceOf(DocumentTooLarge);
  });

  it("refuses_a_body_that_grew_past_the_limit_without_a_length_header", async () => {
    const bytes = new Uint8Array(DOCUMENT_VIEW_MAX_BYTES + 1);
    vi.stubGlobal("fetch", vi.fn(async () => response(bytes)));
    await expect(fetchDocumentText("/a")).rejects.toBeInstanceOf(DocumentTooLarge);
  });

  it("fails_on_an_unsuccessful_response", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ ...response(new Uint8Array()), ok: false, status: 404 })));
    await expect(fetchDocumentText("/a")).rejects.toThrow("document_http_404");
  });
});
