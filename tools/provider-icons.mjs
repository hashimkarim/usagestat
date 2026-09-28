#!/usr/bin/env node
// Materialize the pinned catalogue into the existing native/bar icon contract.
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {resolveProviderIcon} from '../plugins/_provider-icons/index.js';

const plugins = fileURLToPath(new URL('../plugins/', import.meta.url));
const aliases = {
  'abacus-ai': 'abacus', 'alibaba-token-plan': 'bailian',
  'antigravity-cli': 'antigravity', 'antigravity-ide': 'antigravity',
  'aws-bedrock': 'bedrock', 'azure-openai': 'azureai',
  'command-code': 'commandcode', crofai: 'crof', 'cursor-nightly': 'cursor',
  droid: 'factory', 'fireworks-ai': 'fireworks', 'gemini-apps': 'gemini',
  groqcloud: 'groq', nous: 'nousresearch', qwencloud: 'qwen', 'vertex-ai': 'vertexai',
};
const check = process.argv.includes('--check');
let count = 0;
for (const id of fs.readdirSync(plugins).sort()) {
  const dir = path.join(plugins, id);
  if (!fs.existsSync(path.join(dir, 'plugin.json'))) continue;
  const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'plugin.json'), 'utf8'));
  // Explicit, provider-owned icon mappings take precedence over shared defaults.
  if (manifest.icon && manifest.icon !== 'icon.svg') continue;
  const mono = resolveProviderIcon(aliases[id] || id);
  if (!mono) continue;
  for (const [name, icon] of [['icon.svg', mono], ['icon-color.svg', resolveProviderIcon(aliases[id] || id, {style: 'color'})]]) {
    const bytes = fs.readFileSync(path.join(plugins, '_provider-icons', icon.file));
    const target = path.join(dir, name);
    if (check) {
      if (!fs.existsSync(target) || !bytes.equals(fs.readFileSync(target))) throw new Error(`Outdated icon: ${id}/${name}`);
    } else fs.writeFileSync(target, bytes);
  }
  count++;
}
console.log(`${check ? 'Checked' : 'Updated'} ${count} providers from @agenticdriver/provider-icons 0.1.0-alpha.1`);
