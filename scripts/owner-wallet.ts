/**
 * The bonuz-id.near owner wallet, made on this Mac so nobody has to create anything on NEAR by hand.
 *
 *   node scripts/owner-wallet.ts new                  makes a 12-word wallet, prints ONLY the address to fund
 *   node scripts/owner-wallet.ts status               shows what the address holds
 *   node scripts/owner-wallet.ts claim --yes-mainnet  creates bonuz-id.near from the funded address
 *
 * Secrets stay in ~/.near-credentials/mainnet/ (0600) and are never printed:
 *   owner-wallet.seed.txt   the 12 words; import them into any NEAR wallet to see and use bonuz-id.near
 *   <implicit>.json         the funded address' key
 *   bonuz-id.near.json      the same key, as the full-access key of bonuz-id.near
 * Then: node scripts/deploy.ts --network mainnet --yes-mainnet
 */
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { base58 } from '@scure/base';
import { Account, JsonRpcProvider, KeyPair } from 'near-api-js';

const require = createRequire(import.meta.url);
const { generateSeedPhrase } = require('near-seed-phrase') as {
  generateSeedPhrase: () => { seedPhrase: string; secretKey: string; publicKey: string };
};

// Overrides exist only to rehearse on testnet (TLA testnet, owner e.g. x.testnet).
const NET = process.env.OWNER_WALLET_NETWORK ?? 'mainnet';
const OWNER = process.env.OWNER_WALLET_ACCOUNT ?? 'bonuz-id.near';
const TLA = NET === 'mainnet' ? 'near' : 'testnet';
const KEEP = 10n ** 23n; // 0.1 NEAR (prepaid gas + the address' own storage) left behind on the funded address for fees
const MIN = process.env.OWNER_WALLET_MIN_YOCTO ? BigInt(process.env.OWNER_WALLET_MIN_YOCTO) : 75n * 10n ** 23n; // 7.5 NEAR: 6 registry + 1 registrar + gas and storage
const dir = join(process.env.NEAR_CREDENTIALS_DIR ?? join(homedir(), '.near-credentials'), NET);
const seedFile = join(dir, 'owner-wallet.seed.txt');
const provider = new JsonRpcProvider({ url: process.env.NEAR_RPC ?? (NET === 'mainnet' ? 'https://rpc.mainnet.fastnear.com' : 'https://rpc.testnet.near.org') });
const fail = (msg: string): never => {
  console.error(`stop: ${msg}`);
  process.exit(1);
};
const fmt = (y: bigint) => (Number(y / 10n ** 20n) / 1e4).toFixed(4);

const write = (file: string, body: string) => {
  if (existsSync(file)) fail(`refusing to overwrite ${file}`);
  writeFileSync(file, body, { mode: 0o600 });
};
const implicitOf = (pub: string) => Buffer.from(base58.decode(pub.slice('ed25519:'.length))).toString('hex');
const load = () => {
  if (!existsSync(seedFile)) fail(`no wallet yet: run "new" first`);
  const { implicit, private_key } = JSON.parse(readFileSync(join(dir, 'owner-wallet.json'), 'utf8'));
  return { implicit: implicit as string, key: KeyPair.fromString(private_key) };
};
const balance = async (id: string) => {
  try {
    return BigInt((await provider.viewAccount({ accountId: id })).amount);
  } catch (e) {
    if (String(e).includes('does not exist') || String(e).includes('UNKNOWN_ACCOUNT')) return null;
    throw e;
  }
};

const cmd = process.argv[2];
if (cmd === 'new') {
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  if (existsSync(seedFile)) fail(`a wallet already exists (${seedFile}); "status" shows its address`);
  const w = generateSeedPhrase();
  const implicit = implicitOf(w.publicKey);
  const keyJson = (account_id: string) =>
    JSON.stringify({ account_id, public_key: w.publicKey, private_key: w.secretKey });
  write(seedFile, `${w.seedPhrase}\n`);
  write(join(dir, 'owner-wallet.json'), JSON.stringify({ implicit, public_key: w.publicKey, private_key: w.secretKey }));
  write(join(dir, `${implicit}.json`), keyJson(implicit));
  write(join(dir, `${OWNER}.json`), keyJson(OWNER));
  console.log(`address to fund: ${implicit}`);
  console.log(`public key: ${w.publicKey}`);
} else if (cmd === 'status') {
  const { implicit, key } = load();
  console.log(`address: ${implicit}`);
  console.log(`public key: ${key.getPublicKey().toString()}`);
  const a = await balance(implicit);
  console.log(`holds: ${a === null ? 'nothing yet' : `${fmt(a)} NEAR`}`);
  const o = await balance(OWNER);
  console.log(`${OWNER}: ${o === null ? 'not created yet' : `${fmt(o)} NEAR`}`);
} else if (cmd === 'claim') {
  if (!process.argv.includes('--yes-mainnet')) fail('claim needs --yes-mainnet');
  const { implicit, key } = load();
  if ((await balance(OWNER)) !== null) fail(`${OWNER} already exists`);
  const have = await balance(implicit);
  if (have === null || have < MIN) fail(`${implicit} holds ${have === null ? 0 : fmt(have)} NEAR; needs at least ${fmt(MIN)}`);
  const funder = new Account(implicit, provider, key.toString() as `ed25519:${string}`);
  const res = await funder.callFunction({
    contractId: TLA,
    methodName: 'create_account',
    args: { new_account_id: OWNER, new_public_key: key.getPublicKey().toString() },
    deposit: have - KEEP,
    gas: 50_000_000_000_000n,
  });
  console.log(`create_account returned ${JSON.stringify(res)}`);
  const o = await balance(OWNER);
  if (o === null) fail(`${OWNER} was not created; check https://${NET === 'mainnet' ? '' : 'testnet.'}nearblocks.io/address/${implicit}`);
  console.log(`${OWNER} created with ${fmt(o)} NEAR`);
} else {
  fail('usage: node scripts/owner-wallet.ts <new|status|claim --yes-mainnet>');
}
