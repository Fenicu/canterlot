/** Подсказки клиента для подписи устройства в списке сеансов: только для показа, хаб им не доверяет. */
type Brand = { brand: string; version: string };
type HighEntropy = { model?: unknown; platform?: unknown; platformVersion?: unknown; fullVersionList?: unknown };
type UaData = { getHighEntropyValues(hints: string[]): Promise<HighEntropy> };

const LIMIT = 2048;
const text = (value: unknown): string | undefined =>
  typeof value === "string" ? value.slice(0, 128) : undefined;

export async function deviceHints(): Promise<Record<string, unknown>> {
  const standalone =
    typeof matchMedia === "function" && matchMedia("(display-mode: standalone)").matches;
  const device: Record<string, unknown> = { standalone };
  const ua = (navigator as Navigator & { userAgentData?: UaData }).userAgentData;
  if (!ua?.getHighEntropyValues) return device;
  try {
    const values = await ua.getHighEntropyValues(["model", "platform", "platformVersion", "fullVersionList"]);
    const brands = Array.isArray(values.fullVersionList)
      ? (values.fullVersionList as unknown[])
          .filter((item): item is Brand => typeof (item as Brand)?.brand === "string" && typeof (item as Brand)?.version === "string")
          .slice(0, 8)
          .map(({ brand, version }) => ({ brand: brand.slice(0, 64), version: version.slice(0, 32) }))
      : undefined;
    const full = {
      model: text(values.model),
      platform: text(values.platform),
      platformVersion: text(values.platformVersion),
      fullVersionList: brands,
      standalone,
    };
    return new TextEncoder().encode(JSON.stringify(full)).length <= LIMIT ? full : device;
  } catch {
    return device;
  }
}
