/**
 * Rehearses scripts/deploy.ts on a local NEAR sandbox before it touches mainnet:
 *   node scripts/rehearse-deploy.ts
 * Fresh deploy (mainnet settings), rerun (must change nothing), code upgrade
 * (state kept), a real link through the function-call key, refusals. Exits 1 on
 * any surprise. Uses throwaway keys in a temp folder.
 */
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { randomBytes } from 'node:crypto';
import { Account, JsonRpcProvider, KeyPair, KeyPairSigner } from 'near-api-js';
import { DEFAULT_ACCOUNT_ID, DEFAULT_PRIVATE_KEY, DEFAULT_PUBLIC_KEY, Sandbox } from 'near-sandbox';
import { bonuzOf, signLink, viewRegistry } from '../packages/near-link/src/near-link.ts';

let failures = 0;
const expect = (ok: boolean, what: string) => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${what}`);
  if (!ok) failures++;
};

const sandbox = await Sandbox.start({});
const creds = mkdtempSync(join(tmpdir(), 'bonuz-deploy-rehearsal-'));
const record = join(creds, 'record.json');
try {
  mkdirSync(join(creds, 'sandbox'), { recursive: true });
  writeFileSync(
    join(creds, 'sandbox', `${DEFAULT_ACCOUNT_ID}.json`),
    JSON.stringify({ account_id: DEFAULT_ACCOUNT_ID, public_key: DEFAULT_PUBLIC_KEY, private_key: DEFAULT_PRIVATE_KEY }),
  );
  const env = { ...process.env, SANDBOX_RPC: sandbox.rpcUrl, NEAR_CREDENTIALS_DIR: creds, SANDBOX_RECORD: record };
  const deploy = (...flags: string[]) =>
    spawnSync('node', ['scripts/deploy.ts', '--network', 'sandbox', ...flags], { env, encoding: 'utf8' });
  const provider = new JsonRpcProvider({ url: sandbox.rpcUrl });
  const views = { contractId: 'id.sandbox', rpcUrl: sandbox.rpcUrl, finality: 'optimistic' as const };

  // 1. Fresh deploy
  const first = deploy();
  process.stdout.write(first.stdout);
  expect(first.status === 0, 'fresh deploy exits 0');
  expect(!first.stdout.includes('CHECK FAIL') && (first.stdout.match(/check ok/g)?.length ?? 0) === 13, 'all 13 post-deploy checks pass');
  expect(first.stdout.includes('deploy_and_init') && !/ok  deploy_code\b/.test(first.stdout), 'code and init go out as one transaction');
  const status = await viewRegistry<{ network: string; allow_registrar_checked: boolean }>('status', {}, views);
  expect(status.allow_registrar_checked === false, 'implicit accounts only (mainnet setting)');

  // 2. A real link through the saved function-call key
  const fc = JSON.parse(readFileSync(join(creds, 'sandbox', 'registrar-fc', 'registrar.sandbox.json'), 'utf8'));
  const registrar = new Account('registrar.sandbox', provider, new KeyPairSigner(KeyPair.fromString(fc.private_key)));
  const now = Math.floor(Number((await provider.viewBlock({ finality: 'final' })).header.timestamp_nanosec) / 1e9);
  const input = signLink(randomBytes(32), { bonuzId: 'mende', network: 'testnet', expires: now + 480, recipient: 'id.sandbox', nonce: randomBytes(32) });
  await registrar.callFunction({
    contractId: 'id.sandbox',
    methodName: 'link',
    args: { account_id: input.accountId, bonuz_id: input.bonuzId, public_key: input.publicKey, nonce: input.nonce, signature: input.signature, message: input.message },
    gas: 30_000_000_000_000n,
  });
  expect((await bonuzOf(input.accountId, views)) === 'mende', 'registrar function-call key links an unfunded app wallet');
  let ownerBlocked = false;
  try {
    await registrar.callFunction({ contractId: 'id.sandbox', methodName: 'set_paused', args: { paused: true }, deposit: 1n });
  } catch {
    ownerBlocked = true;
  }
  expect(ownerBlocked, 'function-call key cannot call owner methods');

  // 3. Rerun changes nothing
  const codeBefore = (await provider.viewAccount({ accountId: 'id.sandbox' })).code_hash;
  const rerun = deploy();
  expect(rerun.status === 0 && rerun.stdout.includes('is live') && !/ok  (deploy_code|init|create_)/.test(rerun.stdout), 'rerun deploys and creates nothing');
  expect((await bonuzOf(input.accountId, views)) === 'mende', 'link survives the rerun');
  expect((await provider.viewAccount({ accountId: 'id.sandbox' })).code_hash === codeBefore, 'code unchanged by rerun');

  // 4. Code upgrade keeps state
  const up = deploy('--upgrade-code');
  expect(up.status === 0 && up.stdout.includes('upgrade_code_keep_state'), 'upgrade runs');
  expect((await bonuzOf(input.accountId, views)) === 'mende', 'link survives the upgrade');

  // 5. Refusals
  const dangerous = spawnSync('node', ['scripts/deploy.ts', '--network', 'mainnet', '--yes-mainnet', '--demo-links'], { env, encoding: 'utf8' });
  expect(dangerous.status === 1 && dangerous.stderr.includes('not allowed on mainnet'), 'mainnet refuses --demo-links before any read');
  const noYes = spawnSync('node', ['scripts/deploy.ts', '--network', 'mainnet'], { env, encoding: 'utf8' });
  expect(noYes.status === 1 && noYes.stderr.includes('--yes-mainnet'), 'mainnet refuses without --yes-mainnet');
  const keyFiles = execFileSync('find', [creds, '-name', '*.json']).toString().trim().split('\n').length;
  expect(keyFiles >= 4, `keys saved for owner, contract, registrar and function-call key (${keyFiles} files)`);
  const out = `${first.stdout}${first.stderr}${rerun.stdout}${up.stdout}`;
  const secrets = execFileSync('find', [creds, '-name', '*.json', '-path', '*sandbox*']).toString().trim().split('\n')
    .map((f) => JSON.parse(readFileSync(f, 'utf8')).private_key as string | undefined).filter(Boolean) as string[];
  expect(secrets.every((k) => !out.includes(k.slice(8, 40))), 'no private key appears in the deploy output');

  // 6. The mainnet path: keys made by the script, accounts created by the owner's
  //    own wallet, deploy run WITHOUT the owner's key file.
  const creds2 = mkdtempSync(join(tmpdir(), 'bonuz-deploy-rehearsal-wallet-'));
  const env2 = { ...env, NEAR_CREDENTIALS_DIR: creds2, SANDBOX_RECORD: join(creds2, 'record.json') };
  // The profile names id.sandbox / registrar.sandbox, already used above, so start a second sandbox.
  const sb2 = await Sandbox.start({});
  try {
    const env2b = { ...env2, SANDBOX_RPC: sb2.rpcUrl };
    const run = (...flags: string[]) =>
      spawnSync('node', ['scripts/deploy.ts', '--network', 'sandbox', ...flags], { env: env2b, encoding: 'utf8' });
    const p2 = new JsonRpcProvider({ url: sb2.rpcUrl });
    const prep = run('--prepare-keys');
    expect(prep.status === 0 && /id\.sandbox\n\s+public key: ed25519:/.test(prep.stdout), '--prepare-keys prints the public keys');
    const noOwnerKey = run();
    expect(noOwnerKey.status === 1 && noOwnerKey.stderr.includes('--prepare-keys'), 'without the accounts and the owner key the deploy stops and says why');
    const pub = (id: string) => JSON.parse(readFileSync(join(creds2, 'sandbox', `${id}.json`), 'utf8')).public_key as string;
    const wallet = new Account(DEFAULT_ACCOUNT_ID, p2, new KeyPairSigner(KeyPair.fromString(DEFAULT_PRIVATE_KEY)));
    // A wallet-made account that is short of NEAR is refused before any write.
    await wallet.createSubAccount({ accountOrPrefix: 'id', publicKey: pub('id.sandbox'), nearToTransfer: 3n * 10n ** 24n });
    await wallet.createSubAccount({ accountOrPrefix: 'registrar', publicKey: pub('registrar.sandbox'), nearToTransfer: 10n ** 24n });
    const short = run();
    expect(short.status === 1 && short.stderr.includes('holds less than'), 'an underfunded registry account is refused');
    await wallet.transfer({ receiverId: 'id.sandbox', amount: 3n * 10n ** 24n });
    // A stranger's key on the registry account is refused.
    const stranger = KeyPair.fromRandom('ed25519');
    const idAcct = new Account('id.sandbox', p2, new KeyPairSigner(KeyPair.fromString(JSON.parse(readFileSync(join(creds2, 'sandbox', 'id.sandbox.json'), 'utf8')).private_key)));
    await idAcct.addFullAccessKey(stranger.getPublicKey());
    const strangerRun = run();
    if (strangerRun.status !== 1) console.log('--- stranger run', strangerRun.stdout, strangerRun.stderr);
    expect(strangerRun.status === 1 && strangerRun.stderr.includes('keys bonuz does not hold'), 'an unknown key on the registry account is refused');
    await idAcct.deleteKey(stranger.getPublicKey());
    const real = run();
    if (real.status) console.log('--- real run', real.stdout, real.stderr);
    process.stdout.write(real.stdout);
    expect(real.status === 0 && (real.stdout.match(/check ok/g)?.length ?? 0) === 13, 'wallet-created accounts: deploy passes all 13 checks without the owner key');
    expect(/add_sandbox_key_to_id\.sandbox/.test(real.stdout), "the owner's wallet key is added to the registry account as a backup");
    const s2 = await viewRegistry<{ owner: string; links: string; version: string }>('status', {}, { contractId: 'id.sandbox', rpcUrl: sb2.rpcUrl, finality: 'optimistic' });
    expect(s2.owner === 'sandbox' && s2.links === '0' && s2.version === '2.1.0', 'status shows owner, link count and version 2.1.0');
    const again = run();
    expect(again.status === 0 && !/ok  (add_|deploy_and)/.test(again.stdout), 'rerun after the wallet path changes nothing');
  } finally {
    await sb2.tearDown().catch(() => {});
  }
} finally {
  await sandbox.tearDown().catch(() => {});
}
console.log(failures ? `\n${failures} FAILED` : '\nall rehearsal checks passed');
process.exit(failures ? 1 : 0);
