/** Декодирование вложения для редактора: сначала canvas, потом <img>. */

export interface ImageSize {
  width: number;
  height: number;
}

export interface LoadedImage extends ImageSize {
  source: CanvasImageSource;
  close(): void;
}

/** createImageBitmap учитывает EXIF-поворот, обычная картинка — поведение браузера по умолчанию. */
export async function loadImage(file: File): Promise<LoadedImage | null> {
  if (typeof createImageBitmap === "function") {
    try {
      const bitmap = await createImageBitmap(file, { imageOrientation: "from-image" });
      return {
        source: bitmap,
        width: bitmap.width,
        height: bitmap.height,
        close: () => bitmap.close(),
      };
    } catch {
      // Старый браузер или неподдерживаемый формат: пробуем <img>.
    }
  }
  if (typeof URL.createObjectURL !== "function") return null;
  const url = URL.createObjectURL(file);
  try {
    const image = new Image();
    image.decoding = "sync";
    image.src = url;
    await image.decode();
    if (!image.naturalWidth || !image.naturalHeight) {
      URL.revokeObjectURL(url);
      return null;
    }
    return {
      source: image,
      width: image.naturalWidth,
      height: image.naturalHeight,
      close: () => URL.revokeObjectURL(url),
    };
  } catch {
    URL.revokeObjectURL(url);
    return null;
  }
}

/** Размер решает, будет ли у вложения карандаш: слишком большой снимок редактор не откроет. */
export async function imageSize(file: File): Promise<ImageSize | null> {
  const image = await loadImage(file);
  if (!image) return null;
  image.close();
  return { width: image.width, height: image.height };
}
