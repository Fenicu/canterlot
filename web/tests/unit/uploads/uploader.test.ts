import { describe, it, expect, vi, beforeEach } from "vitest";
import { attachmentName, uploadFile } from "#lib/uploads/uploader.js";
import { ApiError, NetworkError } from "#lib/api/http.js";

describe("uploads/uploader", () => {
  const conversationId = "conv-upload-1";
  const fileBytes = new Uint8Array(20 * 1024); // 20 KB
  const mockFile = new File([fileBytes], "document.pdf", {
    type: "application/pdf",
  });

  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("upload_sends_contiguous_offsets_and_reports_progress", async () => {
    const chunkBytes = 8 * 1024; // 8 KB chunks
    const progressReports: Array<{ uploaded: number; total: number }> = [];

    const mockApi = {
      createAttachment: vi.fn().mockResolvedValue({
        id: "att-123",
        state: "uploading",
        upload: { offset: 0, upload_chunk_bytes: chunkBytes },
      }),
      headContent: vi.fn().mockResolvedValue({ offset: 0, length: 20 * 1024 }),
      patchContent: vi
        .fn()
        .mockImplementation(
          async (_id: string, _chunk: Blob, offset: number) => {
            const nextOffset =
              offset + chunkBytes > fileBytes.length
                ? fileBytes.length
                : offset + chunkBytes;
            return { offset: nextOffset };
          },
        ),
      completeAttachment: vi.fn().mockResolvedValue({
        id: "att-123",
        state: "ready",
      }),
    };

    const res = await uploadFile(mockFile, conversationId, {
      api: mockApi,
      onProgress: (p) => progressReports.push(p),
    });

    expect(res.status).toBe("ready");
    expect(res.id).toBe("att-123");

    // Check patchContent calls: offset 0, offset 8192, offset 16384
    expect(mockApi.patchContent).toHaveBeenCalledTimes(3);
    expect(mockApi.patchContent).toHaveBeenNthCalledWith(
      1,
      "att-123",
      expect.anything(),
      0,
      { signal: undefined },
    );
    expect(mockApi.patchContent).toHaveBeenNthCalledWith(
      2,
      "att-123",
      expect.anything(),
      8192,
      { signal: undefined },
    );
    expect(mockApi.patchContent).toHaveBeenNthCalledWith(
      3,
      "att-123",
      expect.anything(),
      16384,
      { signal: undefined },
    );

    expect(progressReports.length).toBeGreaterThan(0);
    expect(progressReports.at(-1)?.uploaded).toBe(20 * 1024);
  });

  it("interrupted_upload_resumes_from_the_head_offset", async () => {
    const chunkBytes = 8 * 1024;
    // Server reports via HEAD that 8192 bytes were already uploaded
    const mockApi = {
      createAttachment: vi.fn().mockResolvedValue({
        id: "att-resumed",
        state: "uploading",
        upload: { offset: 0, upload_chunk_bytes: chunkBytes },
      }),
      headContent: vi
        .fn()
        .mockResolvedValue({ offset: 8192, length: 20 * 1024 }),
      patchContent: vi
        .fn()
        .mockImplementation(
          async (_id: string, _chunk: Blob, offset: number) => {
            return {
              offset:
                offset + chunkBytes > fileBytes.length
                  ? fileBytes.length
                  : offset + chunkBytes,
            };
          },
        ),
      completeAttachment: vi.fn().mockResolvedValue({
        id: "att-resumed",
        state: "ready",
      }),
    };

    const res = await uploadFile(mockFile, conversationId, {
      api: mockApi,
      resumeAttachmentId: "att-resumed",
    });

    expect(res.status).toBe("ready");
    expect(mockApi.headContent).toHaveBeenCalledWith("att-resumed");
    // First patch starts from offset 8192, skipping 0..8192
    expect(mockApi.patchContent).toHaveBeenNthCalledWith(
      1,
      "att-resumed",
      expect.anything(),
      8192,
      { signal: undefined },
    );
  });

  it("offset_mismatch_409_resyncs_from_the_header_offset", async () => {
    const chunkBytes = 8 * 1024;
    let patchCount = 0;

    const mockApi = {
      createAttachment: vi.fn().mockResolvedValue({
        id: "att-mismatch",
        state: "uploading",
        upload: { offset: 0, upload_chunk_bytes: chunkBytes },
      }),
      headContent: vi.fn().mockResolvedValue({ offset: 0, length: 20 * 1024 }),
      patchContent: vi
        .fn()
        .mockImplementation(
          async (_id: string, _chunk: Blob, offset: number) => {
            patchCount++;
            if (patchCount === 1) {
              // Server responds with 409 offset_mismatch and header Upload-Offset: 8192
              const err = new ApiError(
                409,
                "offset_mismatch",
                "Смещение не совпадает",
                "browser",
                "after_condition",
                "c1",
              );
              (err as ApiError & { headers: Headers }).headers = new Headers({
                "Upload-Offset": "8192",
              });
              throw err;
            }
            return {
              offset:
                offset + chunkBytes > fileBytes.length
                  ? fileBytes.length
                  : offset + chunkBytes,
            };
          },
        ),
      completeAttachment: vi.fn().mockResolvedValue({
        id: "att-mismatch",
        state: "ready",
      }),
    };

    const res = await uploadFile(mockFile, conversationId, { api: mockApi });
    expect(res.status).toBe("ready");
    // Second patch was called with resynced offset 8192
    expect(mockApi.patchContent).toHaveBeenNthCalledWith(
      2,
      "att-mismatch",
      expect.anything(),
      8192,
      { signal: undefined },
    );
  });

  it("complete_is_retried_idempotently_and_marks_ready", async () => {
    let completeCount = 0;
    const mockApi = {
      createAttachment: vi.fn().mockResolvedValue({
        id: "att-complete",
        state: "uploading",
        upload: { offset: 20 * 1024, upload_chunk_bytes: 8 * 1024 },
      }),
      headContent: vi
        .fn()
        .mockResolvedValue({ offset: 20 * 1024, length: 20 * 1024 }),
      patchContent: vi.fn(),
      completeAttachment: vi.fn().mockImplementation(async () => {
        completeCount++;
        if (completeCount === 1) {
          throw new ApiError(
            503,
            "db_unavailable",
            "Временная ошибка",
            "hub_db",
            "safe_same_key",
            "c1",
          );
        }
        return { id: "att-complete", state: "ready" };
      }),
    };

    const res = await uploadFile(mockFile, conversationId, { api: mockApi });
    expect(res.status).toBe("ready");
    expect(completeCount).toBe(2);
  });

  it("upload_in_progress_409_waits_and_rereads_head", async () => {
    let patchCount = 0;
    const mockApi = {
      createAttachment: vi.fn().mockResolvedValue({
        id: "att-in-prog",
        state: "uploading",
        upload: { offset: 0, upload_chunk_bytes: 8 * 1024 },
      }),
      headContent: vi.fn().mockImplementation(async () => {
        return { offset: 8192, length: 20 * 1024 };
      }),
      patchContent: vi
        .fn()
        .mockImplementation(
          async (_id: string, _chunk: Blob, offset: number) => {
            patchCount++;
            if (patchCount === 1) {
              throw new ApiError(
                409,
                "upload_in_progress",
                "Вложение уже загружается",
                "browser",
                "after_condition",
                "c1",
              );
            }
            return {
              offset:
                offset + 8 * 1024 > fileBytes.length
                  ? fileBytes.length
                  : offset + 8 * 1024,
            };
          },
        ),
      completeAttachment: vi
        .fn()
        .mockResolvedValue({ id: "att-in-prog", state: "ready" }),
    };

    const res = await uploadFile(mockFile, conversationId, {
      api: mockApi,
      retryDelayMs: 10, // fast retry for test
    });

    expect(res.status).toBe("ready");
    expect(mockApi.headContent).toHaveBeenCalledWith("att-in-prog");
  });

  it("each_upload_keeps_its_own_creation_key", async () => {
    const keys: string[] = [];
    const api = {
      createAttachment: vi.fn(
        async (_body: unknown, options?: { idempotencyKey?: string }) => {
          keys.push(options?.idempotencyKey ?? "");
          return {
            id: `att-${keys.length}`,
            state: "uploading",
            upload: { offset: 0, upload_chunk_bytes: 1024 },
          };
        },
      ),
      headContent: vi
        .fn()
        .mockResolvedValue({
          offset: fileBytes.length,
          length: fileBytes.length,
        }),
      patchContent: vi.fn(),
      completeAttachment: vi
        .fn()
        .mockResolvedValue({ id: "att", state: "ready" }),
    };
    await uploadFile(mockFile, conversationId, { api, retryDelayMs: 5 });
    await uploadFile(new File([fileBytes], "other.pdf"), conversationId, {
      api,
      retryDelayMs: 5,
    });
    expect(keys[0]).toBeTruthy();
    expect(keys[1]).toBeTruthy();
    expect(keys[0]).not.toBe(keys[1]);
    await uploadFile(mockFile, conversationId, {
      api,
      idempotencyKey: "same-upload",
      retryDelayMs: 5,
    });
    await uploadFile(mockFile, conversationId, {
      api,
      idempotencyKey: "same-upload",
      retryDelayMs: 5,
    });
    expect(keys[2]).toBe("same-upload");
    expect(keys[3]).toBe("same-upload");
  });

  it("patch_and_head_retry_transient_failures_and_reread_the_offset", async () => {
    let patches = 0;
    let heads = 0;
    let completes = 0;
    const api = {
      createAttachment: vi.fn().mockResolvedValue({
        id: "att-net",
        state: "uploading",
        upload: { offset: 0, upload_chunk_bytes: fileBytes.length },
      }),
      headContent: vi.fn(async () => {
        heads += 1;
        if (heads === 1) return { offset: 0, length: fileBytes.length };
        return { offset: fileBytes.length, length: fileBytes.length };
      }),
      patchContent: vi.fn(async () => {
        patches += 1;
        if (patches === 1) throw new NetworkError("dropped");
        return { offset: fileBytes.length };
      }),
      completeAttachment: vi.fn(async () => {
        completes += 1;
        if (completes === 1)
          throw new ApiError(500, "internal", "Сбой", "hub_db", "never", "c");
        return { id: "att-net", state: "ready" };
      }),
    };
    const res = await uploadFile(mockFile, conversationId, {
      api,
      retryDelayMs: 5,
    });
    expect(res.status).toBe("ready");
    expect(patches).toBe(1);
    expect(heads).toBeGreaterThan(1);
    expect(completes).toBe(2);
  });

  it("create_and_complete_back_off_on_429", async () => {
    let creates = 0;
    let completes = 0;
    const api = {
      createAttachment: vi.fn(async () => {
        creates += 1;
        if (creates === 1)
          throw new ApiError(
            429,
            "too_many_requests",
            "Позже",
            "hub_db",
            "safe_same_key",
            "c",
          );
        return {
          id: "att-429",
          state: "uploading",
          upload: { offset: 0, upload_chunk_bytes: 1024 },
        };
      }),
      headContent: vi
        .fn()
        .mockResolvedValue({
          offset: fileBytes.length,
          length: fileBytes.length,
        }),
      patchContent: vi.fn(),
      completeAttachment: vi.fn(async () => {
        completes += 1;
        if (completes === 1)
          throw new ApiError(
            429,
            "too_many_requests",
            "Позже",
            "hub_db",
            "safe_same_key",
            "c",
          );
        return { id: "att-429", state: "ready" };
      }),
    };
    const res = await uploadFile(mockFile, conversationId, {
      api,
      retryDelayMs: 5,
    });
    expect(creates).toBe(2);
    expect(completes).toBe(2);
    expect(res.status).toBe("ready");
  });

  it("default_adapter_reads_attachment_record_state", async () => {
    const record = {
      id: "att-real",
      conversation_id: conversationId,
      name: "document.pdf",
      mime: "application/pdf",
      size: fileBytes.length,
      sha256: "abc",
      state: "ready",
      fail_reason: null,
      uploaded_bytes: fileBytes.length,
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        const path = String(url);
        if (init?.method === "POST" && path === "/api/v1/attachments") {
          return new Response(
            JSON.stringify({
              ...record,
              state: "uploading",
              upload: { offset: 0, upload_chunk_bytes: 1024 },
            }),
            { status: 201, headers: { "content-type": "application/json" } },
          );
        }
        if (init?.method === "HEAD") {
          return new Response(null, {
            status: 200,
            headers: {
              "Upload-Offset": String(fileBytes.length),
              "Upload-Length": String(fileBytes.length),
            },
          });
        }
        if (init?.method === "POST" && path.endsWith("/complete")) {
          return new Response(JSON.stringify(record), {
            status: 200,
            headers: { "content-type": "application/json" },
          });
        }
        return new Response(
          JSON.stringify({
            error: {
              code: "internal",
              message: "нет",
              scope: "hub_db",
              retry: "never",
              correlation_id: "c",
            },
          }),
          {
            status: 500,
            headers: { "content-type": "application/json" },
          },
        );
      }),
    );
    const res = await uploadFile(mockFile, conversationId, {
      retryDelayMs: 5,
      hasher: { sha256: async () => "abc" },
    });
    expect(res.id).toBe("att-real");
    expect(res.status).toBe("ready");
  });

  it("attachment_names_keep_the_last_component_within_255_bytes", () => {
    expect(attachmentName("dir/sub\\notes.txt")).toBe("notes.txt");
    const exact = "a".repeat(255);
    expect(attachmentName(exact)).toBe(exact);
    const long = `я${"я".repeat(200)}`;
    const cut = attachmentName(long);
    expect(new TextEncoder().encode(cut).length).toBeLessThanOrEqual(255);
    expect(cut.endsWith("я")).toBe(true);
  });
});
