import { hashFileStreaming } from "#lib/uploads/hasher.js";

let cancelled = false;

self.onmessage = async (event: MessageEvent<{ type: string; file?: Blob }>) => {
  if (event.data.type === "cancel") {
    cancelled = true;
    return;
  }
  if (event.data.type !== "hash" || !event.data.file) return;
  cancelled = false;
  const file = event.data.file;
  try {
    const result = await hashFileStreaming(file, {
      onProgress: (done) => self.postMessage({ type: "progress", done }),
      signal: {
        get aborted() {
          return cancelled;
        },
      } as AbortSignal,
    });
    if (cancelled) {
      self.postMessage({ type: "cancelled" });
      return;
    }
    self.postMessage({
      type: "done",
      sha256: result.sha256,
      size: result.size,
    });
  } catch (err) {
    self.postMessage({
      type: "error",
      message: err instanceof Error ? err.message : "hash_failed",
    });
  }
};
