// Browser, Node and GJS compatible. Generated catalogue; no network or filesystem access.
import {catalog} from './manifest.js';
export const providerIcons = Object.freeze(Object.fromEntries(Object.entries(catalog.icons).map(([id, entry]) => {
    const palette = Object.freeze([...entry.colourTheme]);
    const artworks = entry.artworks && Object.freeze(Object.fromEntries(
        Object.entries(entry.artworks).map(([artwork, files]) => [artwork, Object.freeze({...files})]),
    ));
    return [id, Object.freeze({...entry,
        alternatives: Object.freeze([...entry.alternatives]),
        ...(artworks && {artworks}),
        searchTerms: Object.freeze([...entry.searchTerms]), colourTheme: palette, colorTheme: palette, colorPrimary: entry.primaryColour})];
})));
export const providerAliases = Object.freeze(catalog.aliases);
export const providerIconVersion = catalog.packageVersion;
export const providerIconCategories = Object.freeze(['model', 'provider', 'application']);

function normaliseSearch(value) {
    return value.replace(/([a-z0-9])([A-Z])/g, '$1 $2').normalize('NFKD')
        .replace(/\p{M}/gu, '').toLowerCase().replace(/[^\p{L}\p{N}]+/gu, ' ').trim();
}
const searchIndex = Object.entries(providerIcons).map(([id, entry]) => {
    const names = [id, entry.name, ...entry.searchTerms,
        ...Object.entries(providerAliases).filter(([, target]) => target === id).map(([alias]) => alias)];
    const exact = names.map(normaliseSearch);
    return {id, entry, exact, text: exact.join(' '), compact: exact.join(' ').replaceAll(' ', '')};
});

/** Search source names, localized names and aliases; categories describe artwork, not SDK support. */
export function searchProviderIcons(query = '', options = {}) {
    if (typeof query !== 'string' || (options.category !== undefined && ![...providerIconCategories, 'other'].includes(options.category))) return [];
    const normalised = normaliseSearch(query), terms = normalised.split(' ').filter(Boolean);
    const compact = normalised.replaceAll(' ', '');
    return searchIndex.filter(({entry, text, compact: haystack}) =>
        (!options.category || (entry.category ?? 'other') === options.category) &&
        (terms.every(term => text.includes(term)) || (compact && haystack.includes(compact))))
        .sort((a, b) => Number(b.exact.some(name => name === normalised)) - Number(a.exact.some(name => name === normalised)) || a.entry.name.localeCompare(b.entry.name, 'en'))
        .map(({id, entry}) => Object.freeze({id, ...entry}));
}

export function resolveProviderIcon(provider, options = {}) {
    if (typeof provider !== 'string') return undefined;
    const key = provider.toLowerCase();
    const id = Object.hasOwn(providerAliases, key) ? providerAliases[key] : key;
    if (!Object.hasOwn(providerIcons, id)) return undefined;
    const base = providerIcons[id];
    const requested = options.variant ?? id;
    const alternative = Object.hasOwn(providerAliases, requested) ? providerAliases[requested] : requested;
    if (!base.alternatives.includes(alternative)) return undefined;
    const icon = providerIcons[alternative];
    const artwork = options.artwork ?? 'icon';
    const files = artwork === 'icon' ? icon :
        (icon.artworks && Object.hasOwn(icon.artworks, artwork) ? icon.artworks[artwork] : undefined);
    if (!files) return undefined;
    const style = options.style ?? 'monochrome';
    if (!['monochrome', 'color'].includes(style)) return undefined;
    const actualStyle = style === 'color' && files.color ? 'color' : 'monochrome';
    return {id: alternative, name: icon.name, file: files[actualStyle], style: actualStyle, artwork,
        requestedStyle: style, alternatives: [...base.alternatives]};
}

// Legacy-friendly filenames, generated from the same catalogue, including aliases.
export const PROVIDER_ICON_FILES = Object.freeze(Object.fromEntries(
    [...Object.keys(providerIcons), ...Object.keys(providerAliases)].map(id => [id, resolveProviderIcon(id).file]),
));

export {providerIconComponentNames} from './react-names.js';

/** Source-defined colour values; no artwork sampling or runtime requests. */
export function providerIconTheme(provider) {
    const resolved = resolveProviderIcon(provider);
    if (!resolved) return undefined;
    const {primaryColour, colorPrimary, colourTheme, colorTheme} = providerIcons[resolved.id];
    return Object.freeze({primaryColour, colorPrimary, colourTheme, colorTheme});
}
