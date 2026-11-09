/**
 * Deploys the bonuz ID registry. bonuz's own script: it needs bonuz's keys in
 * ~/.near-credentials/<network>/ and never prints a key.
 *
 *   node scripts/deploy.ts --network testnet [flags]
 *   node scripts/deploy.ts --network mainnet --yes-mainnet [--upgrade-code]
 *   node scripts/deploy.ts --network sandbox        (rehearsal; env SANDBOX_RPC, NEAR_CREDENTIALS_DIR)
 *
 * Fresh account: creates it under the owner, deploys, calls `new`, creates the
 * registrar with a FUNCTION-CALL key (link + unlink_by_registrar, capped allowance).
 * A live registry (status() answers) is never re-initialised: only flagged steps run.
 * If the registry cannot be read for any other reason, the script stops before a write.
 *
 *   --upgrade-code     live registry: deploy the new wasm, keep the state
 *   --migrate-from-v1  testnet only: an account running 1.x is wiped and restarted
 *   --demo-links       testnet/sandbox only: two demo links
 *   --demo-week-cap    testnet only: set_unlink_daily_cap(1000) until 19 Nov 2026 00:00 UTC
 *   --yes-mainnet      required for any write on mainnet
 *
 * Every run ends with an on-chain check and writes deploy/<network>.json.
 */
import { createHash, randomBytes } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { Account, JsonRpcProvider, KeyPair, KeyPairSigner } from 'near-api-js';
import { linkOf, signLink, buildLinkMessage, signNep413, viewRegistry } from '../packages/near-link/src/near-link.ts';
import { base58, base64 } from '@scure/base';

type Network = 'testnet' | 'mainnet' | 'sandbox';
interface Profile {
  rpc: string;
  owner: string;
  contract: string;
  registrar: string;
  contractFunding: bigint; // NEAR: code storage (~1.9) + reserve + links
  registrarFunding: bigint; // NEAR: pays the gas the function-call key spends
  keyAllowance: bigint; // NEAR: the most a leaked registrar key can burn
  reserve: bigint; // NEAR: link refuses below it
  allowRegistrarChecked: boolean; // named accounts (key checked by bonuz)
  explorer: string;
  record: URL;
}

const argv = process.argv.slice(2);
const args = new Set(argv);
const network = argv[argv.indexOf('--network') + 1] as Network;
const PROFILES: Record<Network, () => Profile> = {
  testnet: () => ({
    rpc: 'https://rpc.testnet.near.org',
    owner: 'bonuz.testnet',
    contract: 'id.bonuz.testnet',
    registrar: 'registrar.bonuz.testnet',
    contractFunding: 5n,
    registrarFunding: 1n,
    keyAllowance: 2n,
    reserve: 1n,
    allowRegistrarChecked: true,
    explorer: 'https://testnet.nearblocks.io/address/id.bonuz.testnet',
    record: new URL('../deploy/testnet.json', import.meta.url),
  }),
  mainnet: () => ({
    rpc: 'https://rpc.mainnet.near.org',
    owner: 'bonuz.near',
    contract: 'id.bonuz.near',
    registrar: 'registrar.bonuz.near',
    contractFunding: 6n, // ~1.9 code + 1 reserve + ~3 for about 900 links once nonces are pruned
    registrarFunding: 1n,
    keyAllowance: 1n,
    reserve: 1n,
    allowRegistrarChecked: false, // implicit (app) accounts only, checked by the contract itself
    explorer: 'https://nearblocks.io/address/id.bonuz.near',
    record: new URL('../deploy/mainnet.json', import.meta.url),
  }),
  sandbox: () => ({
    rpc: process.env.SANDBOX_RPC ?? 'http://127.0.0.1:3030',
    owner: 'sandbox',
    contract: 'id.sandbox',
    registrar: 'registrar.sandbox',
    contractFunding: 6n,
    registrarFunding: 1n,
    keyAllowance: 1n,
    reserve: 1n,
    allowRegistrarChecked: false,
    explorer: '(local sandbox)',
    record: new URL(`file://${process.env.SANDBOX_RECORD ?? '/dev/null'}`),
  }),
};
if (!PROFILES[network]) {
  console.error('usage: node scripts/deploy.ts --network <testnet|mainnet|sandbox> [flags]');
  process.exit(2);
}
const P = PROFILES[network]();
const fail = (msg: string): never => {
  console.error(`STOP: ${msg}`);
  process.exit(1);
};
if (network === 'mainnet' && !args.has('--yes-mainnet')) fail('mainnet writes need --yes-mainnet');
for (const flag of ['--migrate-from-v1', '--demo-links', '--demo-week-cap']) {
  if (network === 'mainnet' && args.has(flag)) fail(`${flag} is not allowed on mainnet`);
}

const DEMO_WEEK_CAP = { cap: 1000, until: Date.UTC(2026, 10, 19, 0, 0, 0) / 1000 };
const near = (n: bigint) => n * 10n ** 24n;
const credDir = join(process.env.NEAR_CREDENTIALS_DIR ?? join(homedir(), '.near-credentials'), network);
const provider = new JsonRpcProvider({ url: P.rpc });

const keyFile = (accountId: string, dir = credDir) => join(dir, `${accountId}.json`);
const loadKey = (accountId: string, dir = credDir) => {
  const file = keyFile(accountId, dir);
  if (!existsSync(file)) fail(`no key for ${accountId} in ${dir}`);
  return JSON.parse(readFileSync(file, 'utf8')).private_key as `ed25519:${string}`;
};
const saveKey = (accountId: string, key: KeyPair, dir = credDir) => {
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  const file = keyFile(accountId, dir);
  if (existsSync(file)) fail(`refusing to overwrite the existing key file ${file}`);
  writeFileSync(
    file,
    JSON.stringify({ account_id: accountId, public_key: key.getPublicKey().toString(), private_key: key.toString() }),
    { mode: 0o600 },
  );
  chmodSync(file, 0o600);
  return file;
};
const exists = (accountId: string) => provider.viewAccount({ accountId }).then(() => true, () => false);
const tx: Record<string, string> = {};
const log = (step: string, hash?: string) => {
  if (hash) tx[step] = hash;
  console.log(`ok  ${step}${hash ? `  ${hash}` : ''}`);
};
const prefix = (sub: string) => sub.slice(0, sub.length - P.owner.length - 1);

const owner = new Account(P.owner, provider, loadKey(P.owner));
const wasm = readFileSync(new URL('../contracts/registry/res/bonuz_id_registry.wasm', import.meta.url));
const wasmSha = createHash('sha256').update(wasm).digest('hex');
const wasmCodeHash = base58.encode(createHash('sha256').update(wasm).digest());

// ---- pre-flight: decide everything before the first write ----
const contractExists = await exists(P.contract);
const registrarExists = await exists(P.registrar);
if (contractExists && !existsSync(keyFile(P.contract))) fail(`${P.contract} exists but its key is not in ${credDir}`);
if (registrarExists && !existsSync(keyFile(P.registrar))) fail(`${P.registrar} exists but its key is not in ${credDir}`);
const needed =
  (contractExists ? 0n : P.contractFunding) + (registrarExists ? 0n : P.registrarFunding) + (contractExists && registrarExists ? 0n : 1n);
const ownerBalance = BigInt((await provider.viewAccount({ accountId: P.owner })).amount);
if (ownerBalance < near(needed)) {
  fail(`${P.owner} holds ${Number(ownerBalance / 10n ** 21n) / 1000} NEAR; this deploy needs about ${needed} NEAR`);
}

let liveVersion: string | null = null;
let hasCode = false;
if (contractExists) {
  hasCode = (await provider.viewAccount({ accountId: P.contract })).code_hash !== '11111111111111111111111111111111';
  if (hasCode) {
    try {
      liveVersion = (await viewRegistry<{ version: string }>('status', {}, { contractId: P.contract, rpcUrl: P.rpc })).version;
    } catch (e) {
      if (!/MethodNotFound|MethodResolveError/.test(String(e))) {
        fail(`could not read status() from ${P.contract}, so nothing is changed: ${String(e).slice(0, 200)}`);
      }
    }
  }
}
const alreadyLive = liveVersion !== null;
if (alreadyLive) console.log(`registry ${liveVersion} is live on ${network}: no init, only the flagged steps`);
if (!alreadyLive && hasCode && !args.has('--migrate-from-v1')) {
  fail(`${P.contract} runs code without status() (version 1.x). Pass --migrate-from-v1 (testnet only) to wipe it.`);
}

// ---- accounts ----
if (!contractExists) {
  const key = KeyPair.fromRandom('ed25519');
  saveKey(P.contract, key);
  log(
    'create_contract_account',
    (await owner.createSubAccount({ accountOrPrefix: prefix(P.contract), publicKey: key.getPublicKey(), nearToTransfer: near(P.contractFunding) }))
      .transaction.hash,
  );
}
if (!registrarExists) {
  const key = KeyPair.fromRandom('ed25519');
  saveKey(P.registrar, key);
  log(
    'create_registrar_account',
    (await owner.createSubAccount({ accountOrPrefix: prefix(P.registrar), publicKey: key.getPublicKey(), nearToTransfer: near(P.registrarFunding) }))
      .transaction.hash,
  );
}
const contract = new Account(P.contract, provider, loadKey(P.contract));
const registrarFull = new Account(P.registrar, provider, loadKey(P.registrar));

// ---- code and init ----
const fcDir = join(credDir, 'registrar-fc');
const addFcKey = async () => {
  const fcKey = KeyPair.fromRandom('ed25519');
  log(
    'add_registrar_function_call_key',
    (
      await registrarFull.addFunctionCallAccessKey({
        publicKey: fcKey.getPublicKey(),
        contractId: P.contract,
        methodNames: ['link', 'unlink_by_registrar'],
        allowance: near(P.keyAllowance),
      })
    ).transaction.hash,
  );
  console.log(`    function-call key saved to ${saveKey(P.registrar, fcKey, fcDir)}`);
  return new Account(P.registrar, provider, new KeyPairSigner(fcKey));
};

let registrar: Account;
const initArgs = {
  owner: P.owner,
  registrar: P.registrar,
  network: network === 'sandbox' ? 'testnet' : network,
  allow_registrar_checked: P.allowRegistrarChecked,
  reserve: near(P.reserve).toString(),
};
if (!alreadyLive) {
  let oldKeys: string[] = [];
  if (hasCode) {
    const state = (await provider.query({ request_type: 'view_state', finality: 'final', account_id: P.contract, prefix_base64: '' })) as unknown as {
      values: { key: string }[];
    };
    oldKeys = state.values.map((v) => v.key);
  }
  log('deploy_code', (await contract.deployContract(new Uint8Array(wasm))).transaction.hash);
  log(
    oldKeys.length ? `migrate_from_v1_removing_${oldKeys.length}_old_keys` : 'init',
    (
      await contract.callFunctionRaw({
        contractId: P.contract,
        methodName: oldKeys.length ? 'migrate_from_v1' : 'new',
        args: oldKeys.length ? { old_keys: oldKeys, ...initArgs } : initArgs,
        gas: 100_000_000_000_000n,
      })
    ).transaction.hash,
  );
  registrar = existsSync(keyFile(P.registrar, fcDir))
    ? new Account(P.registrar, provider, loadKey(P.registrar, fcDir))
    : await addFcKey();
} else {
  if (args.has('--upgrade-code')) {
    log('upgrade_code_keep_state', (await contract.deployContract(new Uint8Array(wasm))).transaction.hash);
  }
  registrar = existsSync(keyFile(P.registrar, fcDir))
    ? new Account(P.registrar, provider, loadKey(P.registrar, fcDir))
    : await addFcKey();
}

if (args.has('--demo-week-cap')) {
  log(
    'set_unlink_daily_cap_1000_until_2026_11_19',
    (
      await owner.callFunctionRaw({
        contractId: P.contract,
        methodName: 'set_unlink_daily_cap',
        args: { cap: DEMO_WEEK_CAP.cap, until: String(DEMO_WEEK_CAP.until) },
        deposit: 1n,
      })
    ).transaction.hash,
  );
}

const demoLinks: Record<string, string> = {};
const measures: Record<string, string> = {};
if (args.has('--demo-links')) {
  const block = await provider.viewBlock({ finality: 'final' });
  const expires = Math.floor(Number(block.header.timestamp_nanosec) / 1e9) + 480;
  const msgNetwork = network === 'sandbox' ? 'testnet' : network;
  const send = async (step: string, input: ReturnType<typeof signLink>) => {
    const before = (await provider.viewAccount({ accountId: P.contract })).storage_usage;
    const res = await registrar.callFunctionRaw({
      contractId: P.contract,
      methodName: 'link',
      args: {
        account_id: input.accountId,
        bonuz_id: input.bonuzId,
        public_key: input.publicKey,
        nonce: input.nonce,
        signature: input.signature,
        message: input.message,
      },
      gas: 30_000_000_000_000n,
    });
    const after = (await provider.viewAccount({ accountId: P.contract })).storage_usage;
    const burnt = res.receipts_outcome.reduce((n, r) => n + Number(r.outcome.gas_burnt), Number(res.transaction_outcome.outcome.gas_burnt));
    measures[step] = `${(burnt / 1e12).toFixed(2)} Tgas, +${after - before} bytes`;
    demoLinks[input.accountId] = input.bonuzId;
    log(step, res.transaction.hash);
  };
  await send(
    'demo_link_unfunded_app_wallet_to_bonuz_demo',
    signLink(randomBytes(32), { bonuzId: 'bonuz_demo', network: msgNetwork as 'testnet', expires, recipient: P.contract, nonce: randomBytes(32) }),
  );
  if (P.allowRegistrarChecked) {
    const ownerKey = KeyPair.fromString(loadKey(P.owner));
    const message = buildLinkMessage({ accountId: P.owner, bonuzId: 'bonuz', network: msgNetwork, expires });
    const nonce = randomBytes(32);
    const seed = base58.decode(loadKey(P.owner).slice('ed25519:'.length)).slice(0, 32);
    await send('demo_link_owner_to_bonuz', {
      accountId: P.owner,
      bonuzId: 'bonuz',
      publicKey: ownerKey.getPublicKey().toString(),
      nonce: base64.encode(nonce),
      signature: base64.encode(signNep413(seed, { message, nonce, recipient: P.contract })),
      message,
    });
    console.log(`    link_of(${P.owner}) = ${JSON.stringify(await linkOf(P.owner, { contractId: P.contract, rpcUrl: P.rpc, finality: 'optimistic' }))}`);
  }
}

// ---- post-flight: check what is on chain now ----
const status = await viewRegistry<{
  version: string;
  network: string;
  registrar: string;
  owner: string;
  allow_registrar_checked: boolean;
  paused: boolean;
  available_balance: string;
}>('status', {}, { contractId: P.contract, rpcUrl: P.rpc, finality: 'optimistic' });
const chainCode = (await provider.viewAccount({ accountId: P.contract })).code_hash;
const fcPublic = KeyPair.fromString(loadKey(P.registrar, fcDir)).getPublicKey().toString();
const fcView = (await provider.query({
  request_type: 'view_access_key',
  finality: 'optimistic',
  account_id: P.registrar,
  public_key: fcPublic,
})) as unknown as { permission: { FunctionCall?: { receiver_id: string; method_names: string[]; allowance: string | null } } | string };
const fc = typeof fcView.permission === 'object' ? fcView.permission.FunctionCall : undefined;
const checks: [string, boolean][] = [
  ['code on chain = this wasm', chainCode === wasmCodeHash || !(args.has('--upgrade-code') || !alreadyLive)],
  ['network', status.network === initArgs.network],
  ['owner', status.owner === P.owner],
  ['registrar', status.registrar === P.registrar],
  ['named accounts setting', status.allow_registrar_checked === P.allowRegistrarChecked],
  ['not paused', status.paused === false],
  ['registrar key is function-call only', !!fc],
  ['registrar key only reaches the registry', fc?.receiver_id === P.contract],
  ['registrar key methods', JSON.stringify([...(fc?.method_names ?? [])].sort()) === JSON.stringify(['link', 'unlink_by_registrar'])],
  ['registrar key has a capped allowance', !!fc?.allowance],
];
for (const [name, ok] of checks) console.log(`${ok ? 'check ok ' : 'CHECK FAIL'}  ${name}`);
if (checks.some(([, ok]) => !ok)) process.exitCode = 1;

const commit = execFileSync('git', ['rev-parse', '--short', 'HEAD']).toString().trim();
const record = {
  network,
  contract_account_id: P.contract,
  contract_version: status.version,
  owner: P.owner,
  registrar: P.registrar,
  registrar_key: { kind: 'function-call', methods: ['link', 'unlink_by_registrar'], allowance_near: String(P.keyAllowance) },
  reserve_near: String(P.reserve),
  allow_registrar_checked: status.allow_registrar_checked,
  rpc: P.rpc,
  explorer: P.explorer,
  code_hash_bs58: chainCode,
  wasm_sha256_hex: wasmSha,
  built_from_commit: commit,
  build_command: 'cargo near build non-reproducible-wasm',
  toolchain: { rustc: '1.95.0', 'cargo-near': '0.23.0', 'near-sdk': '5.30.1' },
  deployed_at_utc: new Date().toISOString(),
  unlink_daily_cap: args.has('--demo-week-cap') ? { cap: DEMO_WEEK_CAP.cap, until_unix: DEMO_WEEK_CAP.until, then: 100 } : { cap: 100 },
  transactions: tx,
  demo_links: demoLinks,
  measured: measures,
};
if (network !== 'sandbox' || process.env.SANDBOX_RECORD) {
  const prev = existsSync(P.record) ? JSON.parse(readFileSync(P.record, 'utf8')) : null;
  const out =
    alreadyLive && prev
      ? {
          ...prev,
          contract_version: record.contract_version,
          code_hash_bs58: record.code_hash_bs58,
          ...(args.has('--upgrade-code') ? { wasm_sha256_hex: wasmSha, built_from_commit: commit } : {}),
          unlink_daily_cap: args.has('--demo-week-cap') ? record.unlink_daily_cap : prev.unlink_daily_cap,
          transactions: { ...prev.transactions, ...tx },
          demo_links: { ...prev.demo_links, ...demoLinks },
          measured: { ...prev.measured, ...measures },
        }
      : record;
  writeFileSync(P.record, `${JSON.stringify(out, null, 2)}\n`);
  console.log(`wrote ${P.record.pathname.split('/').slice(-2).join('/')}`);
}
