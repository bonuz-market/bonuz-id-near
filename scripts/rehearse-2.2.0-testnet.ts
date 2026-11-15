/**
 * Full testnet rehearsal of the 2.1.0 -> 2.2.0 upgrade on FRESH accounts (never
 * id.bonuz.testnet, never mainnet):
 *
 *   node scripts/rehearse-2.2.0-testnet.ts --funder <testnet account with ~7 NEAR>
 *
 * 1. creates <prefix>.<funder> (registry), <prefix>-registrar.<funder> and a named
 *    user account <prefix>-alice.<funder>; keys go to ~/.near-credentials/testnet/
 *    (0600, never printed)
 * 2. deploys the frozen 2.1.0 wasm (the one live on mainnet) and writes 2.1.0 state:
 *    two implicit links and one registrar-checked named link
 * 3. upgrades to this build with DeployContract + migrate() in ONE transaction
 * 4. checks the 2.1.0 links still read, adds the 2.2.0 registrar key, adopts a
 *    legacy link, renames its handle, links a new implicit wallet the 2.2.0 way
 * 5. named account flow: propose_link -> refused accepts (wrong signer, wrong
 *    wallet) -> accept_link by the account itself -> views -> unlink_by_owner
 *
 * Every transaction hash goes to deploy/testnet-2.2.0-rehearsal.json.
 */
import { createHash, randomBytes } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { Account, JsonRpcProvider, KeyPair, KeyPairSigner, actions } from 'near-api-js';
import { base58, base64 } from '@scure/base';
import { buildLinkMessage, signLink, signNep413, viewRegistry } from '../packages/near-link/src/near-link.ts';

const argv = process.argv.slice(2);
const funderId = argv[argv.indexOf('--funder') + 1];
if (!argv.includes('--funder') || !funderId?.endsWith('.testnet')) {
  console.error('usage: node scripts/rehearse-2.2.0-testnet.ts --funder <account>.testnet [--prefix r22]');
  process.exit(2);
}
const prefix = argv.includes('--prefix') ? argv[argv.indexOf('--prefix') + 1] : 'r22';
const RPC = process.env.NEAR_TESTNET_RPC ?? 'https://rpc.testnet.fastnear.com';
const credDir = join(homedir(), '.near-credentials', 'testnet');
const provider = new JsonRpcProvider({ url: RPC });
const NEAR = 10n ** 24n;
const TGAS = 1_000_000_000_000n;

const ids = {
  contract: `${prefix}.${funderId}`,
  registrar: `${prefix}-registrar.${funderId}`,
  alice: `${prefix}-alice.${funderId}`,
};
const keyFile = (id: string, dir = credDir) => join(dir, `${id}.json`);
const loadKey = (id: string, dir = credDir) => JSON.parse(readFileSync(keyFile(id, dir), 'utf8')).private_key as `ed25519:${string}`;
const saveKey = (id: string, key: KeyPair, dir = credDir) => {
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  if (existsSync(keyFile(id, dir))) throw new Error(`refusing to overwrite ${keyFile(id, dir)}`);
  writeFileSync(keyFile(id, dir), JSON.stringify({ account_id: id, public_key: key.getPublicKey().toString(), private_key: key.toString() }), { mode: 0o600 });
  chmodSync(keyFile(id, dir), 0o600);
};
const exists = (id: string) => provider.viewAccount({ accountId: id }).then(() => true, () => false);
const eoa = (handle: string) => `0x${createHash('sha256').update(`rehearsal:${handle}`).digest('hex').slice(0, 40)}`;

let failures = 0;
const expect = (ok: boolean, what: string) => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${what}`);
  if (!ok) failures++;
};
/** For a read right after a write: a load-balanced RPC node can lag a block or two. */
const expectSoon = async (check: () => Promise<boolean>, what: string) => {
  for (let i = 0; i < 6; i++) {
    if (await check()) return expect(true, what);
    await new Promise((r) => setTimeout(r, 1500));
  }
  expect(false, what);
};
const tx: Record<string, string> = {};
const refused: Record<string, { tx: string; error: string }> = {};
const measured: Record<string, string> = {};

type Outcome = Awaited<ReturnType<Account['signAndSendTransaction']>>;
const burnt = (o: Outcome) =>
  o.receipts_outcome.reduce((n, r) => n + Number(r.outcome.gas_burnt), Number(o.transaction_outcome.outcome.gas_burnt));
const failureOf = (o: Outcome) => {
  const status = o.status as { Failure?: unknown };
  return status.Failure ? JSON.stringify(status.Failure).match(/"?ExecutionError"?:"([^"]+)"/)?.[1] ?? JSON.stringify(status.Failure).slice(0, 200) : null;
};
/** Sends one transaction and records its hash under `step`. `expectFail`: the text the refusal must contain. */
const send = async (step: string, from: Account, receiverId: string, acts: ReturnType<typeof actions.functionCall>[], expectFail?: string) => {
  const o = await from.signAndSendTransaction({ receiverId, actions: acts, throwOnFailure: false, waitUntil: 'FINAL' });
  const err = failureOf(o);
  if (expectFail) {
    expect(!!err && err.includes(expectFail), `${step}: refused (${err ?? 'ACCEPTED'})`);
    refused[step] = { tx: o.transaction.hash, error: err ?? 'accepted' };
  } else {
    expect(!err, `${step}${err ? `: ${err}` : ''}`);
    tx[step] = o.transaction.hash;
  }
  console.log(`      ${o.transaction.hash}`);
  return o;
};
const call = (method: string, args: object, deposit = 0n, gas = 30n * TGAS) => actions.functionCall(method, args, gas, deposit);
const v = <T>(method: string, args: Record<string, string> = {}) =>
  viewRegistry<T>(method as 'status', args, { contractId: ids.contract, rpcUrl: RPC, finality: 'optimistic' });

// ---- 0. pre-flight ----
for (const id of Object.values(ids)) {
  if (await exists(id)) throw new Error(`${id} already exists: pick another --prefix`);
}
const funder = new Account(funderId, provider, loadKey(funderId));
const balance = BigInt((await provider.viewAccount({ accountId: funderId })).amount);
if (balance < 7n * NEAR) throw new Error(`${funderId} holds under 7 NEAR`);
const wasm21 = readFileSync(new URL('../contracts/registry/res/bonuz_id_registry-2.1.0.wasm', import.meta.url));
const wasm22 = readFileSync(new URL('../contracts/registry/res/bonuz_id_registry.wasm', import.meta.url));
const sha = (b: Buffer) => createHash('sha256').update(b).digest('hex');

// ---- 1. accounts ----
const keys = { contract: KeyPair.fromRandom('ed25519'), registrar: KeyPair.fromRandom('ed25519'), alice: KeyPair.fromRandom('ed25519') };
for (const [name, amount] of [['contract', 5n], ['registrar', 1n], ['alice', 1n]] as const) {
  saveKey(ids[name], keys[name]);
  const o = await funder.createSubAccount({ accountOrPrefix: ids[name].split('.')[0], publicKey: keys[name].getPublicKey(), nearToTransfer: amount * NEAR });
  tx[`create_${name}_account`] = o.transaction.hash;
  console.log(`ok    create ${ids[name]} (${amount} NEAR)\n      ${o.transaction.hash}`);
}
const contract = new Account(ids.contract, provider, loadKey(ids.contract));
const registrarFull = new Account(ids.registrar, provider, loadKey(ids.registrar));
const alice = new Account(ids.alice, provider, loadKey(ids.alice));

// ---- 2. registry 2.1.0 with state ----
await send('deploy_2_1_0_and_init', contract, ids.contract, [
  actions.deployContract(new Uint8Array(wasm21)),
  call('new', { owner: funderId, registrar: ids.registrar, network: 'testnet', allow_registrar_checked: true, reserve: NEAR.toString() }, 0n, 100n * TGAS),
] as never);
expect((await v<{ version: string }>('status')).version === '2.1.0', 'registry runs 2.1.0');

const addFc = async (step: string, methods: string[]) => {
  const k = KeyPair.fromRandom('ed25519');
  const dir = join(credDir, `${prefix}-registrar-fc`);
  saveKey(`${ids.registrar}.${step}`, k, dir);
  const o = await registrarFull.addFunctionCallAccessKey({ publicKey: k.getPublicKey(), contractId: ids.contract, methodNames: methods, allowance: 2n * NEAR });
  tx[step] = o.transaction.hash;
  console.log(`ok    ${step}\n      ${o.transaction.hash}`);
  return new Account(ids.registrar, provider, new KeyPairSigner(k));
};
const reg21 = await addFc('add_registrar_fc_key_2_1_methods', ['link', 'unlink_by_registrar']);

const now = async () => Math.floor(Number((await provider.viewBlock({ finality: 'final' })).header.timestamp_nanosec) / 1e9);
const linkArgs = (i: ReturnType<typeof signLink>, bonuzEoa?: string) => ({
  account_id: i.accountId, bonuz_id: i.bonuzId, public_key: i.publicKey, nonce: i.nonce, signature: i.signature, message: i.message,
  ...(bonuzEoa ? { bonuz_eoa: bonuzEoa } : {}),
});
const implicitA = signLink(randomBytes(32), { bonuzId: 'rehearsal_a', network: 'testnet', expires: (await now()) + 480, recipient: ids.contract, nonce: randomBytes(32) });
const implicitB = signLink(randomBytes(32), { bonuzId: 'rehearsal_b', network: 'testnet', expires: (await now()) + 480, recipient: ids.contract, nonce: randomBytes(32) });
await send('v2_1_link_implicit_a', reg21, ids.contract, [call('link', linkArgs(implicitA))]);
await send('v2_1_link_implicit_b', reg21, ids.contract, [call('link', linkArgs(implicitB))]);
{
  // A 2.1.0 registrar-checked link of the named account (its own key signs the message).
  const expires = (await now()) + 480;
  const message = buildLinkMessage({ accountId: ids.alice, bonuzId: 'rehearsal_alice', network: 'testnet', expires });
  const nonce = randomBytes(32);
  const seed = base58.decode(loadKey(ids.alice).slice('ed25519:'.length)).slice(0, 32);
  await send('v2_1_link_named_alice_registrar_checked', reg21, ids.contract, [
    call('link', {
      account_id: ids.alice, bonuz_id: 'rehearsal_alice', public_key: keys.alice.getPublicKey().toString(),
      nonce: base64.encode(nonce), signature: base64.encode(signNep413(seed, { message, nonce, recipient: ids.contract })), message,
    }),
  ]);
}
await expectSoon(async () => (await v<{ links: string }>('status')).links === '3', '2.1.0 holds 3 links');
const before = {
  status: await v<Record<string, unknown>>('status'),
  a: await v<Record<string, unknown>>('link_of', { account_id: implicitA.accountId }),
  alice: await v<Record<string, unknown>>('link_of', { account_id: ids.alice }),
};

// ---- 3. upgrade: deploy + migrate in one transaction ----
const up = await send('upgrade_2_2_0_deploy_and_migrate', contract, ids.contract, [
  actions.deployContract(new Uint8Array(wasm22)),
  call('migrate', {}, 0n, 100n * TGAS),
] as never);
measured.upgrade_2_2_0_deploy_and_migrate = `${(burnt(up) / 1e12).toFixed(2)} Tgas`;

// ---- 4. 2.1.0 state after the upgrade ----
const s = await v<Record<string, unknown>>('status');
expect(s.version === '2.2.0' && s.links === '3' && s.legacy_links === '3', `status after migrate: version ${s.version}, links ${s.links}, legacy ${s.legacy_links}`);
expect(s.max_link_proposals === 200 && s.link_proposal_ttl_seconds === '604800', 'status shows the claim cap (200) and TTL (7 days)');
expect(s.owner === before.status.owner && s.registrar === before.status.registrar && s.allow_registrar_checked === true, 'owner, registrar and settings kept');
expect((await v('bonuz_of', { account_id: implicitA.accountId })) === 'rehearsal_a', 'bonuz_of(implicit A) still rehearsal_a');
expect((await v('account_of', { bonuz_id: 'rehearsal_b' })) === implicitB.accountId, 'account_of(rehearsal_b) still implicit B');
const aliceLegacy = await v<Record<string, unknown>>('link_of', { account_id: ids.alice });
expect(aliceLegacy.checked_by === 'registrar' && aliceLegacy.bonuz_eoa === null && aliceLegacy.linked_at === before.alice.linked_at, 'link_of(alice) unchanged, bonuz_eoa null');
await send('v2_2_old_key_cannot_propose', reg21, ids.contract, [call('propose_link', { account_id: ids.alice, bonuz_eoa: eoa('x'), bonuz_id: 'x' })], 'MethodNameMismatch').catch((e) => {
  // A function-call key without the method is refused before the contract runs.
  expect(/MethodNameMismatch/.test(String(e)), 'the 2.1.0 registrar key cannot call propose_link');
});

const reg = await addFc('add_registrar_fc_key_2_2_methods', ['link', 'unlink_by_registrar', 'propose_link', 'cancel_link_proposal', 'set_handle', 'adopt_legacy_link']);
await send('adopt_legacy_link_implicit_a', reg, ids.contract, [call('adopt_legacy_link', { account_id: implicitA.accountId, bonuz_eoa: eoa('a') })]);
const aAfter = await v<Record<string, unknown>>('link_of', { account_id: implicitA.accountId });
expect(aAfter.bonuz_eoa === eoa('a') && aAfter.bonuz_id === 'rehearsal_a' && aAfter.linked_at === before.a.linked_at, 'adopted: same link, now keyed to its wallet');
await send('set_handle_rename_a', reg, ids.contract, [call('set_handle', { bonuz_eoa: eoa('a'), bonuz_id: 'rehearsal_a_renamed' })]);
expect((await v('bonuz_of', { account_id: implicitA.accountId })) === 'rehearsal_a_renamed', 'rename keeps the link (bonuz_of follows the new handle)');
expect((await v('account_of', { bonuz_id: 'rehearsal_a' })) === null, 'the old handle resolves to nothing');
await send('set_handle_on_legacy_handle_refused', reg, ids.contract, [call('set_handle', { bonuz_eoa: eoa('z'), bonuz_id: 'rehearsal_b' })], '2.1.0 link still names');
const implicitC = signLink(randomBytes(32), { bonuzId: 'rehearsal_c', network: 'testnet', expires: (await now()) + 480, recipient: ids.contract, nonce: randomBytes(32) });
const linkC = await send('v2_2_link_implicit_c_with_wallet', reg, ids.contract, [call('link', linkArgs(implicitC, eoa('c')))]);
measured.v2_2_link_implicit_c_with_wallet = `${(burnt(linkC) / 1e12).toFixed(2)} Tgas`;
expect((await v('account_of_eoa', { bonuz_eoa: eoa('c') })) === implicitC.accountId, 'a 2.2.0 implicit link is keyed to the wallet');

// ---- 5. named account: propose -> accept ----
await send('unlink_by_registrar_legacy_alice', reg, ids.contract, [call('unlink_by_registrar', { account_id: ids.alice })]);
const p = await send('propose_link_alice', reg, ids.contract, [call('propose_link', { account_id: ids.alice, bonuz_eoa: eoa('alice'), bonuz_id: 'rehearsal_alice' })]);
measured.propose_link = `${(burnt(p) / 1e12).toFixed(2)} Tgas`;
const pending = await v<Record<string, unknown>>('link_proposal_of', { account_id: ids.alice });
expect(pending?.bonuz_eoa === eoa('alice') && pending?.bonuz_id === 'rehearsal_alice', 'link_proposal_of(alice) shows the claim');
expect((await v('bonuz_of', { account_id: ids.alice })) === null, 'nothing is linked before alice accepts');
await send('accept_by_wrong_signer_refused', funder, ids.contract, [call('accept_link', { bonuz_eoa: eoa('alice') }, 1n)], 'no pending link proposal');
await send('accept_wrong_wallet_refused', alice, ids.contract, [call('accept_link', { bonuz_eoa: eoa('mallory') }, 1n)], 'for another bonuz ID');
await send('accept_without_yocto_refused', alice, ids.contract, [call('accept_link', { bonuz_eoa: eoa('alice') })], '1 yoctoNEAR');
const acc = await send('accept_link_by_alice', alice, ids.contract, [call('accept_link', { bonuz_eoa: eoa('alice') }, 1n)]);
measured.accept_link = `${(burnt(acc) / 1e12).toFixed(2)} Tgas`;
const aliceLink = await v<Record<string, unknown>>('link_of', { account_id: ids.alice });
expect(aliceLink.checked_by === 'account' && aliceLink.bonuz_eoa === eoa('alice') && aliceLink.public_key === keys.alice.getPublicKey().toString(), 'link_of(alice): checked_by account, her key, her wallet');
expect((await v('account_of', { bonuz_id: 'rehearsal_alice' })) === ids.alice, 'account_of(rehearsal_alice) = alice');
expect((await v('link_proposal_of', { account_id: ids.alice })) === null, 'the claim is gone');
await send('unlink_by_owner_alice', alice, ids.contract, [call('unlink_by_owner', {}, 1n)]);
await expectSoon(async () => (await v('link_of', { account_id: ids.alice })) === null, 'alice removed her own link');
const final = await v<Record<string, unknown>>('status');
expect(final.links === '3' && final.legacy_links === '1', `final: ${final.links} links (A adopted, B legacy, C new), ${final.legacy_links} legacy`);

// ---- record ----
const commit = execFileSync('git', ['rev-parse', '--short', 'HEAD']).toString().trim();
const record = {
  purpose: 'Testnet rehearsal of the registry 2.1.0 -> 2.2.0 upgrade on fresh accounts. Not id.bonuz.testnet, not mainnet.',
  network: 'testnet',
  rpc: RPC,
  rehearsed_at_utc: new Date().toISOString(),
  built_from_commit: commit,
  accounts: { owner: funderId, ...ids },
  explorer: `https://testnet.nearblocks.io/address/${ids.contract}`,
  wasm: {
    '2.1.0': { sha256_hex: sha(wasm21), note: 'frozen copy of the wasm live on id.bonuz-id.near' },
    '2.2.0': { sha256_hex: sha(wasm22), code_hash_bs58: (await provider.viewAccount({ accountId: ids.contract })).code_hash },
  },
  test_wallets: { note: 'stand-in creation wallets, sha256("rehearsal:<name>")', a: eoa('a'), c: eoa('c'), alice: eoa('alice') },
  links_before_upgrade: { [implicitA.accountId]: 'rehearsal_a', [implicitB.accountId]: 'rehearsal_b', [ids.alice]: 'rehearsal_alice (registrar-checked)' },
  status_after_migrate: s,
  status_final: final,
  transactions: tx,
  refused_as_expected: refused,
  measured,
  result: failures ? `${failures} checks failed` : 'all checks passed',
};
writeFileSync(new URL('../deploy/testnet-2.2.0-rehearsal.json', import.meta.url), `${JSON.stringify(record, null, 2)}\n`);
console.log(failures ? `\n${failures} FAILED` : '\nall testnet rehearsal checks passed');
process.exit(failures ? 1 : 0);
