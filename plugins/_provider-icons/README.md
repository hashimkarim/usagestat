# AgenticDriver provider icons

One maintained catalogue for AgenticDriver and UsageStat-Bar: 155 provider and
product marks, 262 SVG files, monochrome and original colour, with explicit
product alternatives. No runtime dependencies, remote requests or UI framework.

```js
import {resolveProviderIcon} from '@agenticdriver/provider-icons';
resolveProviderIcon('codex', {style: 'color', variant: 'chatgpt'});
// {id: 'openai', file: 'openai.svg', style: 'monochrome', ...}
resolveProviderIcon('claude', {style: 'color', variant: 'claude-code'});
```

`style` is `monochrome` or `color`. Marks without a colour variant return the
monochrome artwork and report the actual style. `variant` selects a related
product's mark, never an account, model, provider runtime or billing route.
Unknown providers or unrelated alternatives return `undefined` for an app's
own fallback. ChatGPT/OpenAI and Codex are alternatives; Anthropic, Claude and
Claude Code are alternatives; Copilot and GitHub Copilot are alternatives.

For inline browser SVG, import `providerIconSvg` from the `/svg` entrypoint.
Give each placement a unique `prefix` to isolate gradients. This static markup
has no scripts, external references or inline styles. It is decorative; label
the surrounding UI. Monochrome artwork inherits `currentColor` inline; an
external `<img>` cannot inherit text colour. Render or use a mask for that case.

For Python, Go, Rust, GTK or other applications, read `manifest.json` and serve
or bundle `assets/*.svg`. GJS can import `index.js` directly. Non-npm apps can
vendor an immutable npm-format release tarball using `scripts/vendor.py` with
its required SHA-256. The copied catalogue and assets are generated dependency
files: update the version/digest and run the vendor script, never edit copies.

Maintain artwork here, then run `npm run build`, `npm test` and `npm pack`.
Commit source and generated catalogue together. Consumers pin releases; they
never download icons while displaying provider settings. See `NOTICE`,
`licenses/` and `provenance.json` for original sources and trademark attribution.
