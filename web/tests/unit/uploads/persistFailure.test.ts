// @vitest-environment node
import "fake-indexeddb/auto";
import { describe, it, expect, vi } from "vitest";
import { UploadManager } from "#lib/uploads/manager.js";
import { openDb, purgeAllStores } from "#lib/storage/idb.js";

describe("uploads/manager: отказ записи в IndexedDB", () => {
  // Отказ записи (квота) не должен держать файл в «queued»: загрузка идёт из памяти,
  // а лоток предупреждает, что после перезагрузки её не продолжить.
  it("upload_does_not_hang_in_queued_when_the_device_write_fails", async () => {
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
      await vi.waitFor(() => expect(upload.phase).toBe("ready"), { timeout: 5000 });
      expect(upload.attachmentId).toBe("att");
      expect(upload.note).toBe("Не сохранено на устройстве: после перезагрузки загрузку не продолжить");
    } finally {
      process.off("unhandledRejection", swallow);
    }
  });
});
