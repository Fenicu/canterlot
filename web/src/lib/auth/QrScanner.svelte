<script lang="ts">
  import { onMount } from 'svelte';
  import Button from '#lib/ui/Button.svelte';
  import { cameraErrorText, qrSecret } from './scan.js';

  let { onsecret, oncancel }: { onsecret: (secret: string) => void; oncancel: () => void } = $props();

  type Detector = { detect(source: CanvasImageSource): Promise<{ rawValue: string }[]> };
  type DetectorClass = new (options: { formats: string[] }) => Detector;

  let video = $state<HTMLVideoElement | null>(null);
  let error = $state('');
  let hint = $state('');
  let ready = $state(false);

  onMount(() => {
    let alive = true;
    let stream: MediaStream | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const stop = () => { stream?.getTracks().forEach((track) => track.stop()); stream = null; };

    async function decoder(): Promise<(frame: HTMLVideoElement) => Promise<string | null>> {
      const Native = (globalThis as { BarcodeDetector?: DetectorClass }).BarcodeDetector;
      if (Native) {
        try {
          const detector = new Native({ formats: ['qr_code'] });
          return async (frame) => (await detector.detect(frame))[0]?.rawValue ?? null;
        } catch {
          // Конструктор есть, а формата QR нет: ниже разбор кадра на JS.
        }
      }
      const { default: jsQR } = await import('jsqr');
      const canvas = document.createElement('canvas');
      const context = canvas.getContext('2d', { willReadFrequently: true })!;
      return async (frame) => {
        const scale = Math.min(1, 640 / Math.max(frame.videoWidth, frame.videoHeight));
        canvas.width = Math.round(frame.videoWidth * scale);
        canvas.height = Math.round(frame.videoHeight * scale);
        if (!canvas.width || !canvas.height) return null;
        context.drawImage(frame, 0, 0, canvas.width, canvas.height);
        const image = context.getImageData(0, 0, canvas.width, canvas.height);
        return jsQR(image.data, image.width, image.height, { inversionAttempts: 'dontInvert' })?.data ?? null;
      };
    }

    async function start(): Promise<void> {
      if (!navigator.mediaDevices?.getUserMedia) { error = 'Браузер не даёт доступа к камере на этой странице'; return; }
      try {
        stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: 'environment' }, audio: false });
      } catch (err) { if (alive) error = cameraErrorText(err); return; }
      if (!alive || !video) { stop(); return; }
      video.srcObject = stream;
      await video.play().catch(() => undefined);
      ready = true;
      const read = await decoder();
      const tick = async () => {
        if (!alive || !video) return;
        let text: string | null = null;
        try { if (video.readyState >= 2) text = await read(video); } catch { text = null; }
        if (!alive) return;
        if (text) {
          const secret = qrSecret(text, location.origin);
          if (secret) { stop(); onsecret(secret); return; }
          hint = 'Это не код входа canterlot';
        }
        timer = setTimeout(() => void tick(), 250);
      };
      void tick();
    }

    void start();
    return () => { alive = false; clearTimeout(timer); stop(); };
  });
</script>

<section class="scanner" aria-labelledby="scan-title" data-scanner>
  <h2 id="scan-title">Сканировать код</h2>
  <p class="note">Наведите камеру на код входа, который показан на другом устройстве</p>
  {#if error}<p class="error" role="alert">{error}</p>
  {:else}
    <video bind:this={video} playsinline muted class:ready aria-label="Изображение с камеры"></video>
    <p role="status" class="hint" class:bad={!!hint}>{hint || (ready ? 'Ищу код…' : 'Включаю камеру…')}</p>
  {/if}
  <Button variant="text" onclick={oncancel}>Отмена</Button>
</section>

<style>
  .scanner { display: flex; flex-direction: column; gap: var(--s6); }
  h2 { margin: var(--s0); font-size: var(--fs-title); }
  p { margin: var(--s0); }
  .note { color: var(--secondary); font-size: var(--fs-meta); }
  video { width: 100%; aspect-ratio: 1; object-fit: cover; border-radius: var(--radius-card); background: var(--raised); }
  .hint { min-height: calc(var(--fs-body) * var(--lh-body)); color: var(--secondary); }
  .bad, .error { color: var(--danger); }
</style>
