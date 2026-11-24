export type IconStyle = 'monochrome' | 'color';
export type IconArtwork = 'icon' | 'brand' | 'text' | 'text-cn';
export type IconCategory = 'model' | 'provider' | 'application';
export interface IconOptions { style?: IconStyle; variant?: string; artwork?: IconArtwork }
export interface IconArtworkFiles { readonly monochrome: string; readonly color?: string }
export interface ProviderIconTheme { readonly primaryColour: string | undefined; readonly colourTheme: readonly string[]; readonly colorPrimary: string | undefined; readonly colorTheme: readonly string[] }
export interface ProviderIconEntry extends IconArtworkFiles, ProviderIconTheme { readonly name: string; readonly fullName?: string; readonly category?: IconCategory; readonly searchTerms: readonly string[]; readonly upstreamUrl?: string; readonly alternatives: readonly string[]; readonly artworks?: Readonly<Partial<Record<Exclude<IconArtwork, 'icon'>, IconArtworkFiles>>> }
export interface ResolvedIcon { id: string; name: string; file: string; style: IconStyle; requestedStyle: IconStyle; artwork: IconArtwork; alternatives: string[] }
export const providerIconVersion: string;
export const providerIcons: Readonly<Record<string, ProviderIconEntry>>;
export const providerAliases: Readonly<Record<string, string>>;
export const PROVIDER_ICON_FILES: Readonly<Record<string, string>>;
export function resolveProviderIcon(provider: string, options?: IconOptions): ResolvedIcon | undefined;
export const providerIconComponentNames: Readonly<Record<string, string>>;
export function providerIconTheme(provider: string): ProviderIconTheme | undefined;
export const providerIconCategories: readonly IconCategory[];
export function searchProviderIcons(query?: string, options?: {category?: IconCategory | 'other'}): Readonly<ProviderIconEntry & {id: string}>[];
