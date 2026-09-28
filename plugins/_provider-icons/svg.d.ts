import type {IconOptions} from './index.js';
/** Safe static SVG markup. Use a unique prefix for each inline icon's gradient IDs. */
export function providerIconSvg(provider: string, options?: IconOptions & {prefix?: string}): string | undefined;
