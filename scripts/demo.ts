/**
 * One-command demo on a local NEAR sandbox: `npm run demo`.
 *
 * 1. starts a sandbox node and deploys the registry (contracts/registry/res/*.wasm)
 * 2. creates the bonuz registrar and gives it a FUNCTION-CALL key only
 *    (this contract, `link` + `unlink_by_registrar`, 2 NEAR gas allowance)
 * 3. makes a brand-new ed25519 key: an app-style implicit account, never funded
 * 4. signs the link message with it (NEP-413, @bonuz/near-link), like the bonuz app does
 * 5. the registrar submits `link` with 0 deposit; the contract checks the signature itself
 * 6. reads it back through the registry's public views
 * 7. shows two refusals: a replayed signature, and another wallet taking the same handle
 *
 * Nothing here touches testnet or mainnet.
 */
import { randomBytes } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { Account, JsonRpcProvider, KeyPair, KeyPairSigner } from 'near-api-js';
import { DEFAULT_ACCOUNT_ID, DEFAULT_PRIVATE_KEY, Sandbox } from 'near-sandbox';
import {
  accountOf,
  bonuzOf,
  checkLink,
  linkOf,
  REGISTRY,
  signLink,
  type LinkInput,
} from '../packages/near-link/src/near-link.ts';

const WASM = new URL('../contracts/registry/res/bonuz_id_registry.wasm', import.meta.url);
if (!existsSync(WASM)) {
  console.error('Missing contracts/registry/res/bonuz_id_registry.wasm. Build it first: npm run build:contract');
  process.exit(1);
}

const near = (n: number) => BigInt(n) * 10n ** 24n;
const step = (n: number, text: string) => console.log(`\n[${n}] ${text}`);
const ok = (text: string) => console.log(`    ok  ${text}`);
const linkArgs = (i: LinkInput) => ({
  account_id: i.accountId,
  bonuz_id: i.bonuzId,
  public_key: i.publicKey,
  nonce: i.nonce,
  signature: i.signature,
  message: i.message,
});
const failure = (e: unknown) => {
  const text = e instanceof Error ? e.message : JSON.stringify(e);
  return text.match(/Smart contract panicked: ([^"\\]+)/)?.[1] ?? text.slice(0, 160);
};

console.log('Starting a local NEAR sandbox (the first run downloads the node, about a minute)...');
const sandbox = await Sandbox.start({});
try {
  const provider = new JsonRpcProvider({ url: sandbox.rpcUrl });
  const root = new Account(DEFAULT_ACCOUNT_ID, provider, DEFAULT_PRIVATE_KEY as `ed25519:${string}`);
  const sub = async (name: string, balance: bigint) => {
    const key = KeyPair.fromRandom('ed25519');
    await root.createSubAccount({ accountOrPrefix: name, publicKey: key.getPublicKey(), nearToTransfer: balance });
    return new Account(`${name}.${DEFAULT_ACCOUNT_ID}`, provider, new KeyPairSigner(key));
  };

  step(1, 'Deploy the registry to a local sandbox');
  const registry = await sub('registry', near(10));
  const owner = await sub('owner', near(2));
  const registrarFull = await sub('registrar', near(3));
  await registry.deployContract(new Uint8Array(readFileSync(WASM)));
  await registry.callFunction({
    contractId: registry.accountId,
    methodName: 'new',
    args: {
      owner: owner.accountId,
      registrar: registrarFull.accountId,
      network: 'testnet',
      allow_registrar_checked: false, // implicit accounts only
      reserve: near(1).toString(),
    },
  });
  ok(`registry ${registry.accountId}, owner ${owner.accountId}, registrar ${registrarFull.accountId}`);
  const views = { contractId: registry.accountId, rpcUrl: sandbox.rpcUrl, finality: 'optimistic' as const };

  step(2, 'Give the registrar a function-call key (cannot move funds or attach deposits)');
  const fcKey = KeyPair.fromRandom('ed25519');
  await registrarFull.addFunctionCallAccessKey({
    publicKey: fcKey.getPublicKey(),
    contractId: registry.accountId,
    methodNames: ['link', 'unlink_by_registrar'],
    allowance: near(2),
  });
  const registrar = new Account(registrarFull.accountId, provider, new KeyPairSigner(fcKey));
  ok('function-call key added: methods link + unlink_by_registrar, 2 NEAR gas allowance');

  step(3, 'Make a brand-new app wallet (NEAR implicit account, never funded)');
  const block = await provider.viewBlock({ finality: 'final' });
  const nowS = Math.floor(Number(block.header.timestamp_nanosec) / 1e9);
  const input = signLink(randomBytes(32), {
    bonuzId: 'demo_user',
    network: 'testnet',
    expires: nowS + 480,
    recipient: registry.accountId,
    nonce: randomBytes(32),
  });
  const exists = await provider.viewAccount({ accountId: input.accountId }).then(
    () => true,
    () => false,
  );
  ok(`account ${input.accountId}`);
  ok(`exists on chain: ${exists}`);

  step(4, 'The wallet signs the link message (NEP-413: no transaction, no gas)');
  console.log(input.message.replace(/^/gm, '        | '));
  const pre = checkLink(input, {
    recipient: registry.accountId,
    network: 'testnet',
    nowNs: BigInt(nowS) * 1_000_000_000n,
    allowRegistrarChecked: false,
  });
  ok(`backend pre-check with @bonuz/near-link: ${JSON.stringify(pre)}`);

  step(5, 'The registrar submits link() with 0 deposit; the contract re-checks the signature');
  const before = (await provider.viewAccount({ accountId: registry.accountId })).storage_usage;
  const tx = await registrar.callFunctionRaw({
    contractId: registry.accountId,
    methodName: 'link',
    args: linkArgs(input),
    gas: 30_000_000_000_000n,
  });
  const after = (await provider.viewAccount({ accountId: registry.accountId })).storage_usage;
  const logs = tx.receipts_outcome.flatMap((r) => r.outcome.logs);
  const burnt = tx.receipts_outcome.reduce(
    (n, r) => n + Number(r.outcome.gas_burnt),
    Number(tx.transaction_outcome.outcome.gas_burnt),
  );
  ok(`event: ${logs.find((l) => l.startsWith('EVENT_JSON'))}`);
  ok(`gas burnt ${(burnt / 1e12).toFixed(2)} Tgas; the contract paid ${after - before} bytes of storage`);

  step(6, 'Read it back, like any app or agent can');
  ok(`bonuz_of(${input.accountId.slice(0, 10)}...) = ${JSON.stringify(await bonuzOf(input.accountId, views))}`);
  ok(`account_of("demo_user") = ${JSON.stringify(await accountOf('demo_user', views))}`);
  ok(`link_of(...).checked_by = ${(await linkOf(input.accountId, views))?.checked_by}`);
  const stillAbsent = await provider.viewAccount({ accountId: input.accountId }).then(
    () => false,
    () => true,
  );
  ok(`the wallet still does not exist on chain: ${stillAbsent}`);

  step(7, 'Refusals');
  let refused = 0;
  try {
    await registrar.callFunction({ contractId: registry.accountId, methodName: 'link', args: linkArgs(input) });
    ok('same signature again -> ACCEPTED (this would be a bug)');
  } catch (e) {
    refused++;
    ok(`same signature again -> refused: ${failure(e)}`);
  }
  const other = signLink(randomBytes(32), {
    bonuzId: 'demo_user',
    network: 'testnet',
    expires: nowS + 480,
    recipient: registry.accountId,
    nonce: randomBytes(32),
  });
  try {
    await registrar.callFunction({ contractId: registry.accountId, methodName: 'link', args: linkArgs(other) });
    ok('another wallet takes "demo_user" -> ACCEPTED (this would be a bug)');
  } catch (e) {
    refused++;
    ok(`another wallet takes "demo_user" -> refused: ${failure(e)}`);
  }
  if (refused !== 2) process.exitCode = 1;

  console.log(`\nDone. The same contract runs on NEAR testnet at ${REGISTRY.testnet?.contractId} (see the README).`);
} finally {
  await sandbox.tearDown().catch(() => {});
}
