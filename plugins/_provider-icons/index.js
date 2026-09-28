// Browser, Node and GJS compatible. Generated catalogue; no network or filesystem access.
import {catalog} from './manifest.js';
export const providerIcons = Object.freeze(catalog.icons);
export const providerAliases = Object.freeze(catalog.aliases);

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
    const style = options.style ?? 'monochrome';
    if (!['monochrome', 'color'].includes(style)) return undefined;
    const actualStyle = style === 'color' && icon.color ? 'color' : 'monochrome';
    return {id: alternative, name: icon.name, file: icon[actualStyle], style: actualStyle,
        requestedStyle: style, alternatives: [...base.alternatives]};
}

// Legacy-friendly filenames, generated from the same catalogue, including aliases.
export const PROVIDER_ICON_FILES = Object.freeze(Object.fromEntries(
    [...Object.keys(providerIcons), ...Object.keys(providerAliases)].map(id => [id, resolveProviderIcon(id).file]),
));
