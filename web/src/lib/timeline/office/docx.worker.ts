import mammoth from 'mammoth';
import type { DocxImage, OfficeReply, OfficeRequest } from './protocol.js';

// Только растровые форматы, которые браузер покажет сам; EMF и WMF из Word он не умеет.
const IMAGE_TYPES: ReadonlySet<string> = new Set(['image/png', 'image/jpeg', 'image/gif', 'image/webp', 'image/bmp', 'image/avif']);

const reply = (message: OfficeReply, transfer: Transferable[] = []) => self.postMessage(message, { transfer });

self.onmessage = async (event: MessageEvent<OfficeRequest>) => {
  const images: DocxImage[] = [];
  try {
    const result = await mammoth.convertToHtml(
      { arrayBuffer: event.data.bytes },
      {
        // Связанные файлы по путям из документа не читаем: в браузере их и нет.
        externalFileAccess: false,
        // Картинка уходит в просмотрщик байтами, а в разметке остаётся меткой: адрес blob: даст главный поток.
        convertImage: mammoth.images.imgElement(async (image) => {
          if (!IMAGE_TYPES.has(image.contentType)) return { src: '' };
          // Здесь бывает и ArrayBuffer, и Uint8Array поверх буфера архива: копия отдаётся целиком.
          const data = new Uint8Array(await image.readAsArrayBuffer()).slice().buffer;
          images.push({ type: image.contentType, data });
          return { src: `docx-image:${images.length - 1}` };
        })
      }
    );
    reply({ kind: 'docx', html: result.value, images }, images.map((image) => image.data));
  } catch {
    reply({ kind: 'failed', reason: 'corrupt' });
  }
};
