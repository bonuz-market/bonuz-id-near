/**
 * Saves a NEAR account key for the deploy scripts, after proving it is a
 * full-access key of that account on chain. Reads the key or seed phrase from
 * stdin (never argv), never prints it.
 *
 *   node scripts/import-account-key.ts <account-id> <mainnet|testnet> < secret
 *
 * Writes ~/.near-credentials/<network>/<account-id>.json with mode 0600.
 * Exit codes: 0 saved, 2 bad input, 3 key does not control the account, 4 RPC error.
 */
import { chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { KeyPair } from 'near-api-js';
import { parseSeedPhrase } from 'near-api-js/seed-phrase';

const [accountId, network] = process.argv.slice(2);
if (!accountId || (network !== 'mainnet' && network !== 'testnet')) {
  console.error('usage: node scripts/import-account-key.ts <account-id> <mainnet|testnet> < secret');
  process.exit(2);
}
const rpc = network === 'mainnet' ? 'https://rpc.mainnet.near.org' : 'https://rpc.testnet.near.org';

const raw = readFileSync(0, 'utf8').trim();
let key: KeyPair;
try {
  if (/^ed25519:[1-9A-HJ-NP-Za-km-z]{60,100}$/.test(raw)) {
    key = KeyPair.fromString(raw as `ed25519:${string}`);
  } else {
    const words = raw.toLowerCase().split(/\s+/).filter(Boolean);
    if (words.length !== 12 && words.length !== 24) throw new Error('shape');
    key = parseSeedPhrase(words.join(' '));
  }
} catch {
  console.error('That does not look like a NEAR private key (ed25519:...) or a 12/24-word seed phrase. Nothing was saved.');
  process.exit(2);
}

const publicKey = key.getPublicKey().toString();
let permission: unknown;
try {
  const res = await fetch(rpc, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 1,
      method: 'query',
      params: { request_type: 'view_access_key', finality: 'final', account_id: accountId, public_key: publicKey },
    }),
  });
  // NEAR answers an unknown key either with `error` or with `result.error`.
  const body = (await res.json()) as { result?: { permission?: unknown; error?: string }; error?: unknown };
  if (!body.result || body.result.error || body.result.permission === undefined) {
    console.error(`This key is not a key of ${accountId} on NEAR ${network}. Nothing was saved.`);
    process.exit(3);
  }
  permission = body.result.permission;
} catch {
  console.error('Could not reach NEAR to check the key. Nothing was saved. Try again in a minute.');
  process.exit(4);
}
if (permission !== 'FullAccess') {
  console.error(`This key belongs to ${accountId} but is a limited (function-call) key. A full-access key is needed. Nothing was saved.`);
  process.exit(3);
}

const dir = join(process.env.NEAR_CREDENTIALS_DIR ?? join(homedir(), '.near-credentials'), network);
mkdirSync(dir, { recursive: true, mode: 0o700 });
const file = join(dir, `${accountId}.json`);
if (existsSync(file)) {
  const previous = JSON.parse(readFileSync(file, 'utf8')) as { public_key?: string };
  if (previous.public_key && previous.public_key !== publicKey) {
    writeFileSync(`${file}.previous-${Date.now()}`, readFileSync(file), { mode: 0o600 });
  }
}
writeFileSync(file, JSON.stringify({ account_id: accountId, public_key: publicKey, private_key: key.toString() }), { mode: 0o600 });
chmodSync(file, 0o600);
console.log(`Checked on NEAR ${network}: this is a full-access key of ${accountId} (${publicKey.slice(0, 16)}...). Saved.`);
