export const MASKABLE_SAFE_RADIUS: number;
export const MASKABLE_SCALE: number;
export function withoutTokens(svg: string): string;
export function fullBleed(svg: string): string;
export function maskable(svg: string): string;
export function iconVariants(svg: string): { name: string; size?: number; svg: string; transparent?: boolean }[];
export function generateIcons(): Promise<void>;
