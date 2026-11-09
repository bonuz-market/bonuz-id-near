/**
 * Copies @bonuz/near-link into another repo as two files, the way bonuz's
 * private app, backend and MCP server carry it:
 *
 *   node scripts/copy-near-link.ts <target-dir>
 *
 * Writes <target-dir>/near-link.ts and <target-dir>/vectors.json. The header of
 * near-link.ts names this repo's commit and the sha256 of both original files
 * (LF bytes). A test in the target repo recomputes them
 * (rule below), so a hand edit fails CI. To update, run this again; never edit.
 *
 * Rule for the target's test: the header is every leading line starting with
 * "// "; sha256(the rest of near-link.ts) must equal `near-link-sha256`, and
 * sha256(vectors.json) must equal `vectors-sha256`.
 */
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

const target = process.argv[2];
if (!target) {
  console.error('usage: node scripts/copy-near-link.ts <target-dir>');
  process.exit(1);
}
const dir = resolve(target);
const status = execFileSync('git', ['status', '--porcelain', 'packages/near-link']).toString().trim();
if (status) {
  console.error('packages/near-link has uncommitted changes; commit first so the header names a real commit');
  process.exit(1);
}
const commit = execFileSync('git', ['rev-parse', 'HEAD']).toString().trim();
const sha = (b: Buffer | string) => createHash('sha256').update(b).digest('hex');

const src = readFileSync(new URL('../packages/near-link/src/near-link.ts', import.meta.url), 'utf8');
const vectors = readFileSync(new URL('../packages/near-link/vectors.json', import.meta.url), 'utf8');
if (src.includes('\r') || vectors.includes('\r')) throw new Error('source files must be LF only');

const header = `// COPIED from https://github.com/bonuz-market/bonuz-id-near packages/near-link. Do not edit.
// Re-copy with: node scripts/copy-near-link.ts <this dir>   (run in bonuz-id-near)
// near-link-source: ${commit}
// near-link-sha256: ${sha(src)}
// vectors-sha256: ${sha(vectors)}
`;
mkdirSync(dir, { recursive: true });
writeFileSync(join(dir, 'near-link.ts'), header + src);
writeFileSync(join(dir, 'vectors.json'), vectors);
console.log(`copied near-link @ ${commit.slice(0, 8)} to ${dir}`);
