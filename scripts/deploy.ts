/**
 * Deploys the bonuz ID registry. bonuz's own script: it needs bonuz's keys in
 * ~/.near-credentials/<network>/ and never prints a key.
 *
 *   node scripts/deploy.ts --network testnet [flags]
 *   node scripts/deploy.ts --network mainnet --yes-mainnet [--upgrade-code]
 *   node scripts/deploy.ts --network sandbox        (rehearsal; env SANDBOX_RPC, NEAR_CREDENTIALS_DIR)
 *
 * Two ways to get the two accounts (registry + registrar):
 *   a) the script creates them under the owner (needs the owner's key file), or
 *   b) `--prepare-keys` makes and saves their keys, prints the public keys, and
 *      the owner creates the accounts in their own wallet with those keys. The
 *      script then never needs the owner's key.
 * Then: deploys the code and calls `new` in ONE transaction (both or neither),
 * gives the registrar a FUNCTION-CALL key (link + unlink_by_registrar, capped
 * allowance), and adds the owner's own full-access keys to both accounts as
 * backups, so the owner's wallet can always upgrade or recover them.
 * A live registry (status() answers) is never re-initialised: only flagged steps run.
 * If the registry cannot be read for any other reason, the script stops before a write.
 *
 *   --prepare-keys     make the two account keys and print their public keys; no chain writes
 *   --upgrade-code     live registry: deploy the new wasm, keep the state. From 2.1.x to
 *                      2.2.x the stored layout changes: the deploy and migrate() go out
 *                      as ONE transaction, so a failed migrate undoes the deploy too
 *   --new-registrar-key add a function-call key with this version's registrar methods.
 *                      The old key file is kept as registrar-fc/<registrar>.<n>.json and
 *                      its key stays on chain until you delete it (after the backend
 *                      switched to the new one)
 *   --demo-links       testnet/sandbox only: two demo links
 *   --demo-week-cap    testnet only: set_unlink_daily_cap(1000) until 19 Nov 2026 00:00 UTC
 *   --yes-mainnet      required for any write on mainnet
 *
 * Every run ends with an on-chain check and writes deploy/<network>.json.
 */
import { createHash, randomBytes } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, readdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { Account, JsonRpcProvider, KeyPair, KeyPairSigner, actions } from 'near-api-js';
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
  // The owner's full-access keys, pinned: they become backup full-access keys of both
  // accounts, so they are never taken from RPC alone. The chain must show exactly these.
  // Unset only for the local sandbox.
  ownerKeys?: string[];
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
    // rpc.mainnet.near.org is deprecated and heavily rate limited.
    rpc: process.env.NEAR_RPC ?? 'https://rpc.mainnet.fastnear.com',
    // bonuz-id.near, not bonuz.near: the bonuz.near seed is not at hand (13 Nov 2026). Made by
    // scripts/owner-wallet.ts; its 12 words are in ~/.near-credentials/mainnet/owner-wallet.seed.txt.
    owner: 'bonuz-id.near',
    contract: 'id.bonuz-id.near',
    registrar: 'registrar.bonuz-id.near',
    contractFunding: 6n, // ~1.9 code + 1 reserve + ~3.1 for about 630-940 links; top up any time by sending NEAR
    registrarFunding: 1n, // ~0.2 mNEAR gas per call: thousands of calls
    keyAllowance: 2n, // the backend alerts under 1 NEAR left, so it must start above that
    reserve: 1n,
    allowRegistrarChecked: false, // implicit (app) accounts only, checked by the contract itself
    // The owner wallet's one full-access key (scripts/owner-wallet.ts, 13 Nov 2026).
    ownerKeys: ['ed25519:arfYfuARc7ctKcjEKBg82SnDJQ9jq5uURxDfVi3Ht63'],
    explorer: 'https://nearblocks.io/address/id.bonuz-id.near',
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
if (network === 'mainnet' && !args.has('--yes-mainnet') && !args.has('--prepare-keys')) fail('mainnet writes need --yes-mainnet');
for (const flag of ['--demo-links', '--demo-week-cap']) {
  if (network === 'mainnet' && args.has(flag)) fail(`${flag} is not allowed on mainnet`);
}
// The post-deploy checks want a registrar key with this version's methods; without a new
// key an upgrade would land and then fail its checks. So refuse before anything is read.
if (args.has('--upgrade-code') && !args.has('--new-registrar-key')) {
  fail('--upgrade-code needs --new-registrar-key (the registrar key must carry this version\'s methods)');
}

// What the registrar's function-call key may call. 2.2.0 adds the pending-claim,
// label and migration methods; a 2.1.0 key (link + unlink_by_registrar) needs --new-registrar-key.
const REGISTRAR_METHODS = ['adopt_legacy_link', 'cancel_link_proposal', 'link', 'propose_link', 'set_handle', 'unlink_by_registrar'];
// Demo links only: a stand-in creation wallet per handle (2.2.0 keys links to it).
const demoEoa = (handle: string) => `0x${createHash('sha256').update(`bonuz-demo:${handle}`).digest('hex').slice(0, 40)}`;
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
// Only a definite "no such account" counts as missing; any other RPC error stops the script.
const exists = (accountId: string) =>
  provider.viewAccount({ accountId }).then(
    () => true,
    (e) => (/UNKNOWN_ACCOUNT|does not exist/i.test(String(e)) ? false : fail(`could not read ${accountId}: ${String(e).slice(0, 200)}`)),
  );
const tx: Record<string, string> = {};
const log = (step: string, hash?: string) => {
  if (hash) tx[step] = hash;
  console.log(`ok  ${step}${hash ? `  ${hash}` : ''}`);
};
const prefix = (sub: string) => sub.slice(0, sub.length - P.owner.length - 1);
const publicKeyIn = (accountId: string, dir = credDir) =>
  JSON.parse(readFileSync(keyFile(accountId, dir), 'utf8')).public_key as string;
type KeyList = { keys: { public_key: string; access_key: { permission: unknown } }[] };
const keysOf = async (accountId: string) =>
  ((await provider.query({ request_type: 'view_access_key_list', finality: 'optimistic', account_id: accountId })) as unknown as KeyList).keys;
const fullKeysOf = async (accountId: string) =>
  (await keysOf(accountId)).filter((k) => k.access_key.permission === 'FullAccess').map((k) => k.public_key);

// ---- --prepare-keys: the owner creates the accounts in their own wallet ----
if (args.has('--prepare-keys')) {
  console.log(`Create these two accounts from ${P.owner} in your wallet, each with exactly this one key:\n`);
  for (const [id, funding] of [[P.contract, P.contractFunding], [P.registrar, P.registrarFunding]] as const) {
    if (await exists(id)) {
      console.log(`  ${id} already exists on ${network}; nothing to prepare\n`);
      continue;
    }
    if (!existsSync(keyFile(id))) saveKey(id, KeyPair.fromRandom('ed25519'));
    console.log(`  ${id}\n    public key: ${publicKeyIn(id)}\n    send:       ${funding} NEAR\n`);
  }
  console.log(`The private keys stay in ${credDir}. Then run the deploy.`);
  process.exit(0);
}

// The owner's key is only needed to create accounts (or for --demo-week-cap / the testnet owner demo link).
const ownerAccount = () => new Account(P.owner, provider, loadKey(P.owner));
const ownerFullKeys = await fullKeysOf(P.owner);
if (P.ownerKeys && JSON.stringify([...ownerFullKeys].sort()) !== JSON.stringify([...P.ownerKeys].sort())) {
  fail(`${P.owner}'s full-access keys on chain (${ownerFullKeys.join(', ')}) differ from the pinned ones in this script; check the wallet and update ownerKeys`);
}
// SANDBOX_WASM: the rehearsal deploys the frozen 2.1.0 wasm first, then upgrades. Sandbox only.
const wasm = readFileSync(
  network === 'sandbox' && process.env.SANDBOX_WASM
    ? process.env.SANDBOX_WASM
    : new URL('../contracts/registry/res/bonuz_id_registry.wasm', import.meta.url),
);
const wasmSha = createHash('sha256').update(wasm).digest('hex');
const wasmCodeHash = base58.encode(createHash('sha256').update(wasm).digest());

// ---- pre-flight: decide everything before the first write ----
const contractExists = await exists(P.contract);
const registrarExists = await exists(P.registrar);
if (contractExists && !existsSync(keyFile(P.contract))) fail(`${P.contract} exists but its key is not in ${credDir}`);
if (registrarExists && !existsSync(keyFile(P.registrar))) fail(`${P.registrar} exists but its key is not in ${credDir}`);
const needed =
  (contractExists ? 0n : P.contractFunding) + (registrarExists ? 0n : P.registrarFunding) + (contractExists && registrarExists ? 0n : 1n);
if (needed > 0n) {
  if (!existsSync(keyFile(P.owner))) {
    fail(`${P.contract} or ${P.registrar} does not exist yet and there is no key for ${P.owner}. Run --prepare-keys and create them in the wallet.`);
  }
  const ownerBalance = BigInt((await provider.viewAccount({ accountId: P.owner })).amount);
  if (ownerBalance < near(needed)) {
    fail(`${P.owner} holds ${Number(ownerBalance / 10n ** 21n) / 1000} NEAR; this deploy needs about ${needed} NEAR`);
  }
}
// Accounts that already exist must hold only keys bonuz controls: ours, the owner's own
// full-access keys and (registrar) our function-call key. An unknown key on the registry
// account could call its private methods; one on the registrar could write links.
const fcDir = join(credDir, 'registrar-fc');
// The current function-call key is registrar-fc/<registrar>.json; earlier ones,
// kept after --new-registrar-key, are registrar-fc/<registrar>.<n>.json.
const knownFcKeys = () =>
  existsSync(fcDir)
    ? readdirSync(fcDir)
        .filter((f) => f === `${P.registrar}.json` || (f.startsWith(`${P.registrar}.`) && /\.\d+\.json$/.test(f)))
        .map((f) => JSON.parse(readFileSync(join(fcDir, f), 'utf8')).public_key as string)
    : [];
for (const [id, exists_] of [[P.contract, contractExists], [P.registrar, registrarExists]] as const) {
  if (!exists_) continue;
  const ours = publicKeyIn(id);
  const keys = await keysOf(id);
  if (!keys.some((k) => k.public_key === ours && k.access_key.permission === 'FullAccess')) {
    fail(`the key in ${keyFile(id)} is not a full-access key of ${id}`);
  }
  const fcs = id === P.registrar ? knownFcKeys() : [];
  const unknown = keys.filter((k) => k.public_key !== ours && !fcs.includes(k.public_key) && !(k.access_key.permission === 'FullAccess' && ownerFullKeys.includes(k.public_key)));
  if (unknown.length) fail(`${id} has keys bonuz does not hold: ${unknown.map((k) => k.public_key).join(', ')}. Delete them in the wallet first.`);
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
if (!alreadyLive && contractExists) {
  const have = BigInt((await provider.viewAccount({ accountId: P.contract })).amount);
  if (have < near(P.contractFunding) - near(1n) / 10n) fail(`${P.contract} holds less than ${P.contractFunding} NEAR; send the rest first`);
}
if (registrarExists && !existsSync(keyFile(P.registrar, fcDir))) {
  const have = BigInt((await provider.viewAccount({ accountId: P.registrar })).amount);
  if (have < near(P.registrarFunding) - near(1n) / 10n) fail(`${P.registrar} holds less than ${P.registrarFunding} NEAR; send the rest first`);
}
if (alreadyLive) console.log(`registry ${liveVersion} is live on ${network}: no init, only the flagged steps`);
if (!alreadyLive && hasCode) fail(`${P.contract} runs code without status(); this script will not touch it`);

// ---- accounts ----
if (!contractExists) {
  const key = KeyPair.fromRandom('ed25519');
  saveKey(P.contract, key);
  log(
    'create_contract_account',
    (await ownerAccount().createSubAccount({ accountOrPrefix: prefix(P.contract), publicKey: key.getPublicKey(), nearToTransfer: near(P.contractFunding) }))
      .transaction.hash,
  );
}
if (!registrarExists) {
  const key = KeyPair.fromRandom('ed25519');
  saveKey(P.registrar, key);
  log(
    'create_registrar_account',
    (await ownerAccount().createSubAccount({ accountOrPrefix: prefix(P.registrar), publicKey: key.getPublicKey(), nearToTransfer: near(P.registrarFunding) }))
      .transaction.hash,
  );
}
const contract = new Account(P.contract, provider, loadKey(P.contract));
const registrarFull = new Account(P.registrar, provider, loadKey(P.registrar));

// ---- backup keys: the owner's own full-access keys can always manage both accounts ----
for (const acct of [contract, registrarFull]) {
  const have = await fullKeysOf(acct.accountId);
  for (const pk of ownerFullKeys.filter((k) => !have.includes(k))) {
    log(`add_${P.owner}_key_to_${acct.accountId}`, (await acct.addFullAccessKey(pk)).transaction.hash);
  }
}

// ---- code and init ----
const addFcKey = async () => {
  const fcKey = KeyPair.fromRandom('ed25519');
  // Saved first: if the run dies after the add, the next run finds and checks it.
  console.log(`    function-call key saved to ${saveKey(P.registrar, fcKey, fcDir)}`);
  log(
    'add_registrar_function_call_key',
    (
      await registrarFull.addFunctionCallAccessKey({
        publicKey: fcKey.getPublicKey(),
        contractId: P.contract,
        methodNames: REGISTRAR_METHODS,
        allowance: near(P.keyAllowance),
      })
    ).transaction.hash,
  );
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
  // One transaction: if the init fails, the code deploy is undone too, so a rerun starts clean.
  log(
    'deploy_and_init',
    (
      await contract.signAndSendTransaction({
        receiverId: P.contract,
        actions: [
          actions.deployContract(new Uint8Array(wasm)),
          actions.functionCall('new', initArgs, 100_000_000_000_000n, 0n),
        ],
      })
    ).transaction.hash,
  );
  registrar = existsSync(keyFile(P.registrar, fcDir))
    ? new Account(P.registrar, provider, loadKey(P.registrar, fcDir))
    : await addFcKey();
} else {
  if (args.has('--upgrade-code')) {
    // The bigger wasm locks more storage. The registry must still be at or above its
    // reserve afterwards, or every link would be refused: top it up first.
    const acct = await provider.viewAccount({ accountId: P.contract });
    const oldCode = (await provider.query({ request_type: 'view_code', finality: 'final', account_id: P.contract })) as unknown as { code_base64: string };
    const extraBytes = BigInt(wasm.length - Buffer.from(oldCode.code_base64, 'base64').length);
    const byteCost = 10n ** 19n;
    const freeAfter = BigInt(acct.amount) - (BigInt(acct.storage_usage) + (extraBytes > 0n ? extraBytes : 0n) + 2_000n) * byteCost;
    if (freeAfter < near(P.reserve)) {
      fail(`after the upgrade ${P.contract} would have ${Number(freeAfter / 10n ** 21n) / 1000} NEAR free, under its ${P.reserve} NEAR reserve; send it more NEAR first`);
    }
    console.log(`    free balance after the upgrade: about ${Number(freeAfter / 10n ** 21n) / 1000} NEAR (reserve ${P.reserve})`);
    // Same major.minor = same stored layout. 2.1 -> 2.2 has migrate(); any other change is refused.
    const crate = readFileSync(new URL('../contracts/registry/Cargo.toml', import.meta.url), 'utf8').match(/^version = "(\d+\.\d+)\./m)?.[1];
    if (crate && liveVersion!.startsWith(`${crate}.`)) {
      log('upgrade_code_keep_state', (await contract.deployContract(new Uint8Array(wasm))).transaction.hash);
    } else if (crate === '2.2' && liveVersion!.startsWith('2.1.')) {
      // One transaction: code and migrate land together or not at all.
      log(
        'upgrade_code_and_migrate_2_1_to_2_2',
        (
          await contract.signAndSendTransaction({
            receiverId: P.contract,
            actions: [actions.deployContract(new Uint8Array(wasm)), actions.functionCall('migrate', {}, 100_000_000_000_000n, 0n)],
          })
        ).transaction.hash,
      );
    } else {
      fail(`live ${liveVersion} and this wasm (${crate}.x) store state differently and there is no migrate for that step`);
    }
  }
  if (args.has('--new-registrar-key') && existsSync(keyFile(P.registrar, fcDir))) {
    let n = 1;
    while (existsSync(join(fcDir, `${P.registrar}.${n}.json`))) n++;
    renameSync(keyFile(P.registrar, fcDir), join(fcDir, `${P.registrar}.${n}.json`));
    console.log(`    the old function-call key file is now registrar-fc/${P.registrar}.${n}.json; its key stays on chain`);
  }
  registrar = existsSync(keyFile(P.registrar, fcDir))
    ? new Account(P.registrar, provider, loadKey(P.registrar, fcDir))
    : await addFcKey();
}

if (args.has('--demo-week-cap')) {
  log(
    'set_unlink_daily_cap_1000_until_2026_11_19',
    (
      await ownerAccount().callFunctionRaw({
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
  // 2.2.0 links need the four-line message naming the wallet; 2.1.0 refuses it.
  const liveNow = (await viewRegistry<{ version: string }>('status', {}, { contractId: P.contract, rpcUrl: P.rpc, finality: 'optimistic' })).version;
  const walletFor = (handle: string) => (liveNow.startsWith('2.1.') ? {} : { bonuzEoa: demoEoa(handle) });
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
        bonuz_eoa: demoEoa(input.bonuzId),
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
    signLink(randomBytes(32), { bonuzId: 'bonuz_demo', network: msgNetwork as 'testnet', expires, recipient: P.contract, nonce: randomBytes(32), ...walletFor('bonuz_demo') }),
  );
  if (P.allowRegistrarChecked) {
    const ownerKey = KeyPair.fromString(loadKey(P.owner));
    const message = buildLinkMessage({ accountId: P.owner, bonuzId: 'bonuz', network: msgNetwork, expires, ...walletFor('bonuz') });
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
const contractKeys = await keysOf(P.contract);
const registrarFullKeys = await fullKeysOf(P.registrar);
const checks: [string, boolean][] = [
  ['code on chain = this wasm', chainCode === wasmCodeHash || !(args.has('--upgrade-code') || !alreadyLive)],
  ['network', status.network === initArgs.network],
  ['owner', status.owner === P.owner],
  ['registrar', status.registrar === P.registrar],
  ['named accounts setting', status.allow_registrar_checked === P.allowRegistrarChecked],
  ['not paused', status.paused === false],
  ['registrar key is function-call only', !!fc],
  ['registrar key only reaches the registry', fc?.receiver_id === P.contract],
  ['registrar key methods', JSON.stringify([...(fc?.method_names ?? [])].sort()) === JSON.stringify(REGISTRAR_METHODS)],
  ['registrar key has a capped allowance', !!fc?.allowance],
  [`${P.owner} keys can also manage ${P.contract}`, ownerFullKeys.length > 0 && ownerFullKeys.every((k) => contractKeys.some((c) => c.public_key === k))],
  [`${P.owner} keys can also manage ${P.registrar}`, ownerFullKeys.length > 0 && ownerFullKeys.every((k) => registrarFullKeys.includes(k))],
  [`${P.contract} holds only full-access keys bonuz controls`, contractKeys.every((k) => k.access_key.permission === 'FullAccess' && (k.public_key === publicKeyIn(P.contract) || ownerFullKeys.includes(k.public_key)))],
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
  backup_keys: `${P.owner}'s full-access keys are also full-access keys of ${P.contract} and ${P.registrar}`,
  registrar_key: { kind: 'function-call', methods: REGISTRAR_METHODS, allowance_near: String(P.keyAllowance) },
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
          ...(args.has('--new-registrar-key') ? { registrar_key: record.registrar_key } : {}),
          unlink_daily_cap: args.has('--demo-week-cap') ? record.unlink_daily_cap : prev.unlink_daily_cap,
          transactions: { ...prev.transactions, ...tx },
          demo_links: { ...prev.demo_links, ...demoLinks },
          measured: { ...prev.measured, ...measures },
        }
      : record;
  writeFileSync(P.record, `${JSON.stringify(out, null, 2)}\n`);
  console.log(`wrote ${P.record.pathname.split('/').slice(-2).join('/')}`);
}
