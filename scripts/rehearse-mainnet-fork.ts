/**
 * Rehearses the 2.1.0 -> 2.2.0 mainnet upgrade on a local fork of mainnet:
 *   node scripts/rehearse-mainnet-fork.ts
 * Reads id.bonuz-id.near's live code, every state record and the three accounts' keys from
 * mainnet (read only), loads them into a local sandbox with sandbox_patch_state, then runs
 * the runbook's real step 4 command, deploy.ts --network mainnet --yes-mainnet --upgrade-code
 * --new-registrar-key, with NEAR_RPC at the fork. The deploy key and registrar keys are
 * replaced by throwaway ones in a temp folder, and a preload blocks any request that is not
 * to 127.0.0.1, so nothing can reach mainnet. Then the runbook's step 5 checks, the
 * backfill count, a 2.2.0 link, refusals, the pause rollback path and a named-account claim.
 * Exits 1 on any surprise.
 */
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash, randomBytes } from 'node:crypto';
import { Account, JsonRpcProvider, KeyPair, KeyPairSigner } from 'near-api-js';
import { Sandbox } from 'near-sandbox';
import { base58 } from '@scure/base';
import { signLink, viewRegistry } from '../packages/near-link/src/near-link.ts';

const MAINNET = 'https://rpc.mainnet.fastnear.com';
const OWNER = 'bonuz-id.near';
const CONTRACT = 'id.bonuz-id.near';
const REGISTRAR = 'registrar.bonuz-id.near';
const ALICE = 'rehearsal-alice.near';
const NEAR = 10n ** 24n;

let failures = 0;
const expect = (ok: boolean, what: string) => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${what}`);
  if (!ok) failures++;
};
const refused = async (call: () => Promise<unknown>, pattern?: RegExp) => {
  try {
    await call();
    return false;
  } catch (e) {
    const text = `${String(e)} ${JSON.stringify(e)}`;
    const why = text.match(/(?:Smart contract panicked|ExecutionError)[^"\\]*:?\s*([^"\\]{0,140})/)?.[0] ?? text.slice(0, 140);
    console.log(`      refused: ${why.replace(/\s+/g, ' ')}`);
    return pattern ? pattern.test(text) : true;
  }
};

// ---- 1. read mainnet (view calls only) ----
const rpc = async (url: string, method: string, params: unknown) => {
  const res = await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }) });
  const body = (await res.json()) as { result?: any; error?: unknown };
  if (body.error) throw new Error(`${method}: ${JSON.stringify(body.error)}`);
  return body.result;
};
const query = (url: string, params: Record<string, unknown>) => rpc(url, 'query', { finality: url === MAINNET ? 'final' : 'optimistic', ...params });
const live = {
  accounts: {} as Record<string, any>,
  keys: {} as Record<string, { public_key: string; access_key: { permission: any } }[]>,
  code: (await query(MAINNET, { request_type: 'view_code', account_id: CONTRACT })).code_base64 as string,
  state: (await query(MAINNET, { request_type: 'view_state', account_id: CONTRACT, prefix_base64: '' })).values as { key: string; value: string }[],
};
for (const id of [OWNER, CONTRACT, REGISTRAR]) {
  live.accounts[id] = await query(MAINNET, { request_type: 'view_account', account_id: id });
  live.keys[id] = (await query(MAINNET, { request_type: 'view_access_key_list', account_id: id })).keys;
}
const view = async <T>(url: string, method: string, args: unknown = {}): Promise<T> => {
  const r = await query(url, { request_type: 'call_function', account_id: CONTRACT, method_name: method, args_base64: Buffer.from(JSON.stringify(args)).toString('base64') });
  return JSON.parse(Buffer.from(r.result).toString());
};
const liveStatus = await view<Record<string, any>>(MAINNET, 'status');
const frozen = readFileSync(new URL('../contracts/registry/res/bonuz_id_registry-2.1.0.wasm', import.meta.url));
const codeHash = (b: Uint8Array) => base58.encode(createHash('sha256').update(b).digest());
console.log(`mainnet at block ${live.accounts[CONTRACT].block_height}: ${CONTRACT} ${liveStatus.version}, ${liveStatus.links} links, ${live.state.length} state records, ${Number(BigInt(live.accounts[CONTRACT].amount) / 10n ** 21n) / 1000} NEAR`);
expect(live.accounts[CONTRACT].code_hash === codeHash(frozen), 'mainnet code is the frozen 2.1.0 wasm in res/');
expect(liveStatus.version === '2.1.0', 'mainnet status() says 2.1.0 (runbook step 3)');

// ---- 2. the fork ----
const sandbox = await Sandbox.start({});
const creds = mkdtempSync(join(tmpdir(), 'bonuz-mainnet-fork-'));
const recordFile = new URL('../deploy/mainnet.json', import.meta.url);
const recordBackup = join(creds, 'mainnet.json.orig');
copyFileSync(recordFile, recordBackup);
try {
  const fork = sandbox.rpcUrl;
  if (!/^http:\/\/(127\.0\.0\.1|localhost):/.test(fork)) throw new Error(`the fork must be local, got ${fork}`);
  const provider = new JsonRpcProvider({ url: fork });

  // Throwaway keys stand in for the deploy key, the registrar's full key and its
  // function-call key. The owner's real public key stays: deploy.ts pins it.
  const deployKey = KeyPair.fromRandom('ed25519');
  const registrarKey = KeyPair.fromRandom('ed25519');
  const oldFcKey = KeyPair.fromRandom('ed25519');
  const aliceKey = KeyPair.fromRandom('ed25519');
  const localKey: Record<string, string> = {};
  const swap = (id: string, k: { public_key: string; access_key: { permission: any } }) => {
    const p = k.access_key.permission;
    if (p !== 'FullAccess') return { pk: oldFcKey.getPublicKey().toString(), perm: p };
    if (live.keys[OWNER].some((o) => o.public_key === k.public_key)) return { pk: k.public_key, perm: p };
    const kp = id === CONTRACT ? deployKey : registrarKey;
    localKey[id] = kp.getPublicKey().toString();
    return { pk: kp.getPublicKey().toString(), perm: p };
  };
  const records: unknown[] = [];
  for (const id of [OWNER, CONTRACT, REGISTRAR]) {
    const a = live.accounts[id];
    records.push({ Account: { account_id: id, account: { amount: a.amount, locked: a.locked, code_hash: a.code_hash, storage_usage: a.storage_usage } } });
    for (const k of live.keys[id]) {
      const { pk, perm } = swap(id, k);
      records.push({ AccessKey: { account_id: id, public_key: pk, access_key: { nonce: 0, permission: perm } } });
    }
  }
  records.push({ Contract: { account_id: CONTRACT, code: live.code } });
  for (const { key, value } of live.state) records.push({ Data: { account_id: CONTRACT, data_key: key, value } });
  records.push({ Account: { account_id: ALICE, account: { amount: String(2n * NEAR), locked: '0', code_hash: '11111111111111111111111111111111', storage_usage: 182 } } });
  records.push({ AccessKey: { account_id: ALICE, public_key: aliceKey.getPublicKey().toString(), access_key: { nonce: 0, permission: 'FullAccess' } } });
  await rpc(fork, 'sandbox_patch_state', { records });
  // The patch lands with the next blocks: wait until the fork shows the registry.
  for (let i = 0; i < 50; i++) {
    const seen = await query(fork, { request_type: 'view_account', account_id: CONTRACT }).then(() => true, () => false);
    if (seen) break;
    await new Promise((r) => setTimeout(r, 200));
  }

  const forkStatus = await view<Record<string, any>>(fork, 'status');
  const { available_balance: _a, ...liveRest } = liveStatus;
  const { available_balance: _b, ...forkRest } = forkStatus;
  expect(JSON.stringify(forkRest) === JSON.stringify(liveRest), 'the fork answers status() exactly like mainnet');
  expect((await provider.viewAccount({ accountId: CONTRACT })).code_hash === live.accounts[CONTRACT].code_hash, 'the fork runs the mainnet code');

  // deploy.ts reads keys from <dir>/mainnet: throwaway keys only.
  const dir = join(creds, 'mainnet');
  mkdirSync(join(dir, 'registrar-fc'), { recursive: true, mode: 0o700 });
  const save = (file: string, id: string, kp: KeyPair) =>
    writeFileSync(join(dir, file), JSON.stringify({ account_id: id, public_key: kp.getPublicKey().toString(), private_key: kp.toString() }), { mode: 0o600 });
  save(`${CONTRACT}.json`, CONTRACT, deployKey);
  save(`${REGISTRAR}.json`, REGISTRAR, registrarKey);
  save(`registrar-fc/${REGISTRAR}.json`, REGISTRAR, oldFcKey);

  // ---- 3. runbook step 4, the real command, against the fork ----
  const env = { ...process.env, NEAR_RPC: fork, NEAR_CREDENTIALS_DIR: creds, NODE_OPTIONS: `--import ${new URL('./local-only.mjs', import.meta.url).pathname}` };
  const deploy = (...flags: string[]) => spawnSync('node', ['scripts/deploy.ts', '--network', 'mainnet', ...flags], { env, encoding: 'utf8' });
  const storageBefore = (await provider.viewAccount({ accountId: CONTRACT })).storage_usage;
  const up = deploy('--yes-mainnet', '--upgrade-code', '--new-registrar-key');
  process.stdout.write(up.stdout.replace(/^/gm, '    | '));
  if (up.status) process.stdout.write(up.stderr.replace(/^/gm, '    ! '));
  expect(up.status === 0, 'deploy.ts --network mainnet --yes-mainnet --upgrade-code --new-registrar-key exits 0');
  expect(!up.stderr.includes('NETWORK CALL BLOCKED'), 'deploy.ts reached nothing but the local fork');
  expect(up.stdout.includes('upgrade_code_and_migrate_2_1_to_2_2'), 'code and migrate() went out as one transaction');
  expect((up.stdout.match(/check ok/g)?.length ?? 0) === 13 && !up.stdout.includes('CHECK FAIL'), 'all 13 post-deploy checks pass');
  const secrets = [deployKey, registrarKey, oldFcKey].map((k) => k.toString().slice(8, 40));
  const freshFc = JSON.parse(readFileSync(join(dir, 'registrar-fc', `${REGISTRAR}.json`), 'utf8'));
  secrets.push(String(freshFc.private_key).slice(8, 40));
  expect(secrets.every((s) => !`${up.stdout}${up.stderr}`.includes(s)), 'no private key appears in the deploy output');

  // ---- 4. runbook step 5 ----
  const s = await view<Record<string, any>>(fork, 'status');
  expect(s.version === '2.2.0', `status() says 2.2.0 (got ${s.version})`);
  expect(s.links === liveStatus.links, `links unchanged (${s.links})`);
  expect(String(s.legacy_links) === String(liveStatus.links), `legacy_links = links (${s.legacy_links}): backfill has ${s.legacy_links} links to adopt`);
  expect(s.owner === OWNER && s.registrar === REGISTRAR && s.network === 'mainnet' && s.paused === false && s.allow_registrar_checked === false, 'owner, registrar, network, not paused, implicit only: unchanged');
  expect(BigInt(s.available_balance) >= BigInt(s.reserve), `free balance after the upgrade ${Number(BigInt(s.available_balance) / 10n ** 21n) / 1000} NEAR is at or above the ${Number(BigInt(s.reserve) / NEAR)} NEAR reserve`);
  const storageAfter = (await provider.viewAccount({ accountId: CONTRACT })).storage_usage;
  console.log(`    storage ${storageBefore} -> ${storageAfter} bytes (${((storageAfter - storageBefore) / 1e5).toFixed(3)} NEAR more locked)`);

  const contract = new Account(CONTRACT, provider, new KeyPairSigner(deployKey));
  expect(await refused(() => contract.callFunction({ contractId: CONTRACT, methodName: 'migrate', args: {}, gas: 100_000_000_000_000n })), 'a second migrate() is refused');

  // ---- 5. a 2.2.0 implicit link through the NEW function-call key ----
  const fc = new Account(REGISTRAR, provider, new KeyPairSigner(KeyPair.fromString(freshFc.private_key)));
  const oldFc = new Account(REGISTRAR, provider, new KeyPairSigner(oldFcKey));
  const now = Math.floor(Number((await provider.viewBlock({ finality: 'final' })).header.timestamp_nanosec) / 1e9);
  const wallet = `0x${randomBytes(20).toString('hex')}`;
  const linkArgs = (i: ReturnType<typeof signLink>, bonuz_eoa?: string) => ({
    account_id: i.accountId, bonuz_id: i.bonuzId, public_key: i.publicKey, nonce: i.nonce, signature: i.signature, message: i.message, ...(bonuz_eoa ? { bonuz_eoa } : {}),
  });
  const sign = (extra: { bonuzEoa?: string; bonuzId?: string } = {}) =>
    signLink(randomBytes(32), { bonuzId: extra.bonuzId ?? 'mende', network: 'mainnet', expires: now + 480, recipient: CONTRACT, nonce: randomBytes(32), ...('bonuzEoa' in extra ? { bonuzEoa: extra.bonuzEoa } : { bonuzEoa: wallet }) });
  const link = (who: Account, args: unknown) => who.callFunction({ contractId: CONTRACT, methodName: 'link', args: args as Record<string, unknown>, gas: 30_000_000_000_000n });

  const three = sign({ bonuzEoa: undefined });
  expect(await refused(() => link(fc, linkArgs(three))), 'a three-line (2.1.0) link message is refused after the upgrade');
  const four = sign();
  expect(await refused(() => link(fc, linkArgs(four, `0x${'cd'.repeat(20)}`))), 'bonuz_eoa that differs from the signed wallet line is refused');
  await link(fc, linkArgs(four, wallet));
  const opts = { contractId: CONTRACT, rpcUrl: fork, finality: 'optimistic' as const };
  expect((await viewRegistry<string | null>('bonuz_of', { account_id: four.accountId }, opts)) === 'mende', 'a four-line link through the new key links an unfunded app wallet');
  expect((await viewRegistry<string | null>('account_of_eoa', { bonuz_eoa: wallet }, opts)) === four.accountId, 'account_of_eoa finds it by the creation wallet');
  expect(await refused(() => oldFc.callFunction({ contractId: CONTRACT, methodName: 'propose_link', args: { account_id: ALICE, bonuz_eoa: wallet, bonuz_id: 'x' }, gas: 30_000_000_000_000n })), 'the old 2.1.0 function-call key cannot reach the new methods');
  const again = sign({ bonuzId: 'other' });
  expect(await refused(() => link(fc, linkArgs(again, wallet))), 'a second account for the same creation wallet is refused');

  // ---- 6. a named account claims through propose_link / accept_link ----
  const alice = new Account(ALICE, provider, new KeyPairSigner(aliceKey));
  const aliceWallet = `0x${randomBytes(20).toString('hex')}`;
  await fc.callFunction({ contractId: CONTRACT, methodName: 'propose_link', args: { account_id: ALICE, bonuz_eoa: aliceWallet, bonuz_id: 'alice' }, gas: 30_000_000_000_000n });
  expect((await viewRegistry<string | null>('bonuz_of', { account_id: ALICE }, opts)) === null, 'a proposal alone links nothing');
  await alice.callFunction({ contractId: CONTRACT, methodName: 'accept_link', args: { bonuz_eoa: aliceWallet }, deposit: 1n, gas: 30_000_000_000_000n });
  expect((await viewRegistry<string | null>('bonuz_of', { account_id: ALICE }, opts)) === 'alice', 'the named account is linked once it accepts itself');

  // ---- 7. the rollback path: the owner pauses, links stop, unpause ----
  const ownerKey = KeyPair.fromRandom('ed25519');
  await rpc(fork, 'sandbox_patch_state', { records: [{ AccessKey: { account_id: OWNER, public_key: ownerKey.getPublicKey().toString(), access_key: { nonce: 0, permission: 'FullAccess' } } }] });
  const owner = new Account(OWNER, provider, new KeyPairSigner(ownerKey));
  await owner.callFunction({ contractId: CONTRACT, methodName: 'set_paused', args: { paused: true }, deposit: 1n, gas: 30_000_000_000_000n });
  expect((await view<Record<string, any>>(fork, 'status')).paused === true, 'the owner can pause 2.2.0 (rollback step)');
  expect(await refused(() => link(fc, linkArgs(sign({ bonuzId: 'paused', bonuzEoa: `0x${randomBytes(20).toString('hex')}` })))), 'links are refused while paused');
  await owner.callFunction({ contractId: CONTRACT, methodName: 'set_paused', args: { paused: false }, deposit: 1n, gas: 30_000_000_000_000n });
  expect((await view<Record<string, any>>(fork, 'status')).paused === false, 'and unpause it');
  const end = await view<Record<string, any>>(fork, 'status');
  console.log(`    after the run: ${end.links} links, ${end.legacy_links} legacy, ${Number(BigInt(end.available_balance) / 10n ** 21n) / 1000} NEAR free`);
} finally {
  copyFileSync(recordBackup, recordFile);
  await sandbox.tearDown();
}
console.log(failures ? `\n${failures} FAILED` : '\nall passed');
process.exit(failures ? 1 : 0);
