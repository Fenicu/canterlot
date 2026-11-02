import type { ColorKey } from "#lib/ui/types.js";

export const COLOR_KEYS = [
  "accent",
  "blue",
  "amber",
  "success",
  "danger",
  "neutral",
] as const satisfies readonly ColorKey[];

export const COLOR_NAMES: Record<(typeof COLOR_KEYS)[number], string> = {
  accent: "Бирюзовый",
  blue: "Синий",
  amber: "Янтарный",
  success: "Зелёный",
  danger: "Красный",
  neutral: "Серый",
};

const VARS: Record<string, string> = {
  accent: "--accent",
  blue: "--blue",
  amber: "--warning",
  success: "--success",
  danger: "--danger",
  neutral: "--muted",
};

export function colorVar(key: string | null): string {
  if (!key) return "--muted";
  return VARS[key] ?? "--muted";
}

/** Цвет точки строки без выбранного цвета: тот же тон, что у текста активности. */
export function toneVar(tone: "accent" | "default" | "warning" | "danger"): string {
  if (tone === "accent") return "--accent";
  if (tone === "warning") return "--warning";
  if (tone === "danger") return "--danger";
  return "--muted";
}
