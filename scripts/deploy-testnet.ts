/**
 * Deploys the registry to NEAR testnet as id.bonuz.testnet. bonuz's own script:
 * it needs the bonuz keys in ~/.near-credentials/testnet/ and never prints them.
 *
 *   node scripts/deploy-testnet.ts [--demo-links] [--demo-week-cap]
 *
 *   First run: deploys the code; if id.bonuz.testnet still holds 1.x state, starts with
 *   migrate_from_v1 (deletes every old key), else with new. Creates the registrar and its
 *   function-call key. A rerun against a live 2.x registry deploys nothing and only runs
 *   the flagged steps, so `--demo-week-cap` alone just sets the cap.
 *   --demo-links     link two demo pairs: a fresh unfunded app wallet -> bonuz_demo
 *                    (checked by the contract) and bonuz.testnet -> bonuz (checked by the registrar)
 *   --demo-week-cap  set_unlink_daily_cap(1000, until 19 Nov 2026 00:00 UTC). Only with
 *                    Mende's approval, logged in the bonuz-outreach record. Ends on its own.
 *
 * Writes deploy/testnet.json. The registrar's function-call key is written to
 * ~/.near-credentials/testnet/registrar-fc/registrar.bonuz.testnet.json (0600).
 */
import { createHash, randomBytes } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { Account, JsonRpcProvider, KeyPair, KeyPairSigner } from 'near-api-js';
import { linkOf, signLink, buildLinkMessage, signNep413, viewRegistry } from '../packages/near-link/src/near-link.ts';
import { base58, base64 } from '@scure/base';

const RPC = 'https://rpc.testnet.near.org';
const OWNER = 'bonuz.testnet';
const CONTRACT = 'id.bonuz.testnet';
const REGISTRAR = 'registrar.bonuz.testnet';
const CONTRACT_FUNDING = 5n; // NEAR
const REGISTRAR_FUNDING = 1n; // NEAR, pays the gas the function-call key spends
const KEY_ALLOWANCE = 2n; // NEAR, the most a leaked registrar key can burn
const RESERVE = 1n; // NEAR, link refuses below it
const DEMO_WEEK_CAP = { cap: 1000, until: Date.UTC(2026, 10, 19, 0, 0, 0) / 1000 };

const args = new Set(process.argv.slice(2));
const near = (n: bigint) => n * 10n ** 24n;
const credDir = join(homedir(), '.near-credentials', 'testnet');
const provider = new JsonRpcProvider({ url: RPC });

const loadKey = (accountId: string, dir = credDir) => {
  const file = join(dir, `${accountId}.json`);
  if (!existsSync(file)) throw new Error(`no key for ${accountId} in ${dir}`);
  return JSON.parse(readFileSync(file, 'utf8')).private_key as `ed25519:${string}`;
};
const saveKey = (accountId: string, key: KeyPair, dir: string) => {
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  const file = join(dir, `${accountId}.json`);
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

const owner = new Account(OWNER, provider, loadKey(OWNER));
const contractKey = loadKey(CONTRACT);
let contract = new Account(CONTRACT, provider, contractKey);

const wasm = readFileSync(new URL('../contracts/registry/res/bonuz_id_registry.wasm', import.meta.url));
const wasmSha = createHash('sha256').update(wasm).digest('hex');
const codeHash = base58.encode(createHash('sha256').update(wasm).digest());

if (!(await exists(CONTRACT))) {
  const pub = KeyPair.fromString(contractKey).getPublicKey();
  log(
    'create_contract_account',
    (await owner.createSubAccount({ accountOrPrefix: 'id', publicKey: pub, nearToTransfer: near(CONTRACT_FUNDING) })).transaction.hash,
  );
  contract = new Account(CONTRACT, provider, contractKey);
}

if (!(await exists(REGISTRAR))) {
  const full = KeyPair.fromRandom('ed25519');
  saveKey(REGISTRAR, full, credDir);
  log(
    'create_registrar_account',
    (await owner.createSubAccount({ accountOrPrefix: 'registrar', publicKey: full.getPublicKey(), nearToTransfer: near(REGISTRAR_FUNDING) })).transaction.hash,
  );
}
const registrarFull = new Account(REGISTRAR, provider, loadKey(REGISTRAR));

// A rerun against a live 2.x registry must never deploy, init or migrate again
// (migrate_from_v1 would delete every link). It only runs the flagged extras.
const liveVersion = await viewRegistry<{ version: string }>('status', {}, { contractId: CONTRACT, rpcUrl: RPC })
  .then((s) => s.version)
  .catch(() => null);
const alreadyLive = liveVersion?.startsWith('2.') ?? false;
if (alreadyLive) console.log(`registry ${liveVersion} is live: no deploy, no init, only the flagged steps`);

const fcDir = join(credDir, 'registrar-fc');
let registrar: Account;
if (!alreadyLive) {
  // Old state keys (version 1.x), if any. view_state works while the state is small.
  const state = (await provider.query({ request_type: 'view_state', finality: 'final', account_id: CONTRACT, prefix_base64: '' })) as unknown as {
    values: { key: string }[];
  };
  const oldKeys = state.values.map((v) => v.key);
  const initArgs = { owner: OWNER, registrar: REGISTRAR, network: 'testnet', allow_registrar_checked: true, reserve: near(RESERVE).toString() };
  log('deploy_code', (await contract.deployContract(new Uint8Array(wasm))).transaction.hash);
  log(
    oldKeys.length ? `migrate_from_v1_removing_${oldKeys.length}_old_keys` : 'init',
    (
      await contract.callFunctionRaw({
        contractId: CONTRACT,
        methodName: oldKeys.length ? 'migrate_from_v1' : 'new',
        args: oldKeys.length ? { old_keys: oldKeys, ...initArgs } : initArgs,
        gas: 100_000_000_000_000n,
      })
    ).transaction.hash,
  );

  const fcKey = KeyPair.fromRandom('ed25519');
  log(
    'add_registrar_function_call_key',
    (
      await registrarFull.addFunctionCallAccessKey({
        publicKey: fcKey.getPublicKey(),
        contractId: CONTRACT,
        methodNames: ['link', 'unlink_by_registrar'],
        allowance: near(KEY_ALLOWANCE),
      })
    ).transaction.hash,
  );
  console.log(`    function-call key saved to ${saveKey(REGISTRAR, fcKey, fcDir)}`);
  registrar = new Account(REGISTRAR, provider, new KeyPairSigner(fcKey));
} else {
  registrar = new Account(REGISTRAR, provider, loadKey(REGISTRAR, fcDir));
}

if (args.has('--demo-week-cap')) {
  log(
    'set_unlink_daily_cap_1000_until_2026_11_19',
    (
      await owner.callFunctionRaw({
        contractId: CONTRACT,
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
  const send = async (step: string, input: ReturnType<typeof signLink>) => {
    const before = (await provider.viewAccount({ accountId: CONTRACT })).storage_usage;
    const res = await registrar.callFunctionRaw({
      contractId: CONTRACT,
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
    const after = (await provider.viewAccount({ accountId: CONTRACT })).storage_usage;
    const burnt = res.receipts_outcome.reduce((n, r) => n + Number(r.outcome.gas_burnt), Number(res.transaction_outcome.outcome.gas_burnt));
    measures[step] = `${(burnt / 1e12).toFixed(2)} Tgas, +${after - before} bytes`;
    demoLinks[input.accountId] = input.bonuzId;
    log(step, res.transaction.hash);
  };

  // 1. A fresh app-style wallet, never funded. Its throwaway key is not kept.
  await send(
    'demo_link_unfunded_app_wallet_to_bonuz_demo',
    signLink(randomBytes(32), { bonuzId: 'bonuz_demo', network: 'testnet', expires, recipient: CONTRACT, nonce: randomBytes(32) }),
  );

  // 2. A named account, signed with its own full-access key (testnet allows registrar-checked links).
  const ownerKey = KeyPair.fromString(loadKey(OWNER));
  const message = buildLinkMessage({ accountId: OWNER, bonuzId: 'bonuz', network: 'testnet', expires });
  const nonce = randomBytes(32);
  const seed = base58.decode(loadKey(OWNER).slice('ed25519:'.length)).slice(0, 32);
  await send('demo_link_bonuz_testnet_to_bonuz', {
    accountId: OWNER,
    bonuzId: 'bonuz',
    publicKey: ownerKey.getPublicKey().toString(),
    nonce: base64.encode(nonce),
    signature: base64.encode(signNep413(seed, { message, nonce, recipient: CONTRACT })),
    message,
  });
  const check = await linkOf(OWNER, { contractId: CONTRACT, rpcUrl: RPC, finality: 'optimistic' });
  console.log(`    link_of(${OWNER}) = ${JSON.stringify(check)}`);
}

const commit = execFileSync('git', ['rev-parse', '--short', 'HEAD']).toString().trim();
const record = {
  network: 'testnet',
  contract_account_id: CONTRACT,
  contract_version: '2.0.0',
  owner: OWNER,
  registrar: REGISTRAR,
  registrar_key: { kind: 'function-call', methods: ['link', 'unlink_by_registrar'], allowance_near: String(KEY_ALLOWANCE) },
  reserve_near: String(RESERVE),
  allow_registrar_checked: true,
  rpc: RPC,
  explorer: `https://testnet.nearblocks.io/address/${CONTRACT}`,
  code_hash_bs58: codeHash,
  wasm_sha256_hex: wasmSha,
  built_from_commit: commit,
  build_command: 'cargo near build non-reproducible-wasm',
  toolchain: { rustc: '1.95.0', 'cargo-near': '0.23.0', 'near-sdk': '5.30.1' },
  deployed_at_utc: new Date().toISOString(),
  unlink_daily_cap: args.has('--demo-week-cap') ? { cap: DEMO_WEEK_CAP.cap, until_unix: DEMO_WEEK_CAP.until, then: 100 } : { cap: 100 },
  transactions: tx,
  demo_links: demoLinks,
  measured: measures,
  history: 'Versions 1.0.0 and 1.1.0 (self-service and sponsor-paid designs) ran on this account on 9 Nov 2026. 2.0.0 started with migrate_from_v1, which deleted all their state; their transactions stay on the explorer. sponsor.bonuz.testnet belonged to 1.1.0 and is no longer used.',
};
const recordUrl = new URL('../deploy/testnet.json', import.meta.url);
const out = alreadyLive
  ? (() => {
      const prev = JSON.parse(readFileSync(recordUrl, 'utf8'));
      return {
        ...prev,
        unlink_daily_cap: args.has('--demo-week-cap') ? record.unlink_daily_cap : prev.unlink_daily_cap,
        transactions: { ...prev.transactions, ...tx },
        demo_links: { ...prev.demo_links, ...demoLinks },
        measured: { ...prev.measured, ...measures },
      };
    })()
  : record;
writeFileSync(recordUrl, `${JSON.stringify(out, null, 2)}\n`);
console.log('wrote deploy/testnet.json');
