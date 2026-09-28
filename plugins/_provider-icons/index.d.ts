export type IconStyle = 'monochrome' | 'color';
export interface IconOptions { style?: IconStyle; variant?: string }
export interface ProviderIconEntry { name: string; monochrome: string; color?: string; alternatives: string[] }
export interface ResolvedIcon { id: string; name: string; file: string; style: IconStyle; requestedStyle: IconStyle; alternatives: string[] }
export const providerIcons: Readonly<Record<string, ProviderIconEntry>>;
export const providerAliases: Readonly<Record<string, string>>;
export const PROVIDER_ICON_FILES: Readonly<Record<string, string>>;
export function resolveProviderIcon(provider: string, options?: IconOptions): ResolvedIcon | undefined;
