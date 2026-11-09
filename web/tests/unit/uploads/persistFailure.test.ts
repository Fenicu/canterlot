// @vitest-environment node
import "fake-indexeddb/auto";
import { describe, it, expect, vi } from "vitest";
import { UploadManager } from "#lib/uploads/manager.js";
import { openDb, purgeAllStores } from "#lib/storage/idb.js";

describe("uploads/manager: отказ записи в IndexedDB", () => {
  // bug: UploadManager.add/retry ставят загрузку в очередь только после успешной записи в IndexedDB;
  // отказ записи (квота) оставляет файл в «queued» навсегда, и отправка блокируется «Файлы ещё загружаются».
  it.skip("bug: upload_does_not_hang_in_queued_when_the_device_write_fails", async () => {
    await purgeAllStores();
    const real = await openDb("persist-failure-owner");
    // Квота устройства исчерпана: запись файла в журнал загрузок отклонена.
    const db = Object.create(real) as typeof real;
    db.put = () => Promise.reject(new DOMException("quota", "QuotaExceededError"));
    const api = {
      createAttachment: vi.fn().mockResolvedValue({ id: "att", state: "uploading", upload: { offset: 0, upload_chunk_bytes: 1024 } }),
      headContent: vi.fn().mockResolvedValue({ offset: 0, length: 4 }),
      patchContent: vi.fn().mockResolvedValue({ offset: 4 }),
      completeAttachment: vi.fn().mockResolvedValue({ id: "att", state: "ready" }),
      deleteAttachment: vi.fn(),
    };
    const swallow = () => undefined;
    process.on("unhandledRejection", swallow);
    try {
      const manager = new UploadManager({ db, api });
      const upload = manager.add("c-1", new File(["data"], "a.txt"), 0);
      await new Promise((resolve) => setTimeout(resolve, 200));
      // Лоток должен либо загрузить файл, либо показать сбой; «загружается» навсегда блокирует отправку.
      expect(["ready", "failed"]).toContain(upload.phase);
    } finally {
      process.off("unhandledRejection", swallow);
    }
  });
});
