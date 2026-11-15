/**
 * Rehearses scripts/deploy.ts on a local NEAR sandbox before it touches mainnet:
 *   node scripts/rehearse-deploy.ts
 * Fresh deploy (mainnet settings), rerun (must change nothing), code upgrade
 * (state kept), a real link through the function-call key, refusals. Exits 1 on
 * any surprise. Uses throwaway keys in a temp folder.
 */
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
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
  const wallet = `0x${'ab'.repeat(20)}`;
  const input = signLink(randomBytes(32), { bonuzId: 'mende', network: 'testnet', expires: now + 480, recipient: 'id.sandbox', nonce: randomBytes(32), bonuzEoa: wallet });
  await registrar.callFunction({
    contractId: 'id.sandbox',
    methodName: 'link',
    args: { account_id: input.accountId, bonuz_id: input.bonuzId, public_key: input.publicKey, nonce: input.nonce, signature: input.signature, message: input.message, bonuz_eoa: wallet },
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
  const bare = deploy('--upgrade-code');
  expect(bare.status === 1 && bare.stderr.includes('needs --new-registrar-key'), '--upgrade-code alone is refused');
  const up = deploy('--upgrade-code', '--new-registrar-key');
  expect(up.status === 0 && up.stdout.includes('upgrade_code_keep_state'), 'upgrade runs');
  expect((await bonuzOf(input.accountId, views)) === 'mende', 'link survives the upgrade');

  // 5. Refusals
  // The refusals must come before any network call: NEAR_RPC points nowhere, and the
  // preload throws on any fetch, so a refusal that came after a read would show that error.
  const offline = { ...env, NEAR_RPC: 'http://127.0.0.1:9', NODE_OPTIONS: `--import ${new URL('./no-network.mjs', import.meta.url).pathname}` };
  const offlineRun = (...flags: string[]) => spawnSync('node', ['scripts/deploy.ts', '--network', 'mainnet', ...flags], { env: offline, encoding: 'utf8' });
  const noNet = (r: ReturnType<typeof spawnSync>) => !String(r.stderr).includes('NETWORK CALL');
  const dangerous = offlineRun('--yes-mainnet', '--demo-links');
  expect(dangerous.status === 1 && String(dangerous.stderr).includes('not allowed on mainnet') && noNet(dangerous), 'mainnet refuses --demo-links before any network call');
  const noYes = offlineRun();
  expect(noYes.status === 1 && String(noYes.stderr).includes('--yes-mainnet') && noNet(noYes), 'mainnet refuses without --yes-mainnet, before any network call');
  const bareMainnet = offlineRun('--yes-mainnet', '--upgrade-code');
  expect(bareMainnet.status === 1 && String(bareMainnet.stderr).includes('needs --new-registrar-key') && noNet(bareMainnet), 'mainnet --upgrade-code without --new-registrar-key is refused before any network call');
  const probe = spawnSync('node', ['-e', "fetch('http://127.0.0.1:9').catch(()=>{})"], { env: offline, encoding: 'utf8' });
  expect(String(probe.stderr).includes('NETWORK CALL'), 'the no-network guard itself catches a fetch');
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
    expect(s2.owner === 'sandbox' && s2.links === '0' && s2.version === '2.2.0', 'status shows owner, link count and version 2.2.0');
    const again = run();
    expect(again.status === 0 && !/ok  (add_|deploy_and)/.test(again.stdout), 'rerun after the wallet path changes nothing');
  } finally {
    await sb2.tearDown().catch(() => {});
  }

  // 7. The 2.2.0 upgrade of a live 2.1.0 registry: deploy + migrate in one
  //    transaction, then a new registrar key with the 2.2.0 methods.
  const sb3 = await Sandbox.start({});
  const creds3 = mkdtempSync(join(tmpdir(), 'bonuz-deploy-rehearsal-upgrade-'));
  try {
    mkdirSync(join(creds3, 'sandbox'), { recursive: true });
    writeFileSync(
      join(creds3, 'sandbox', `${DEFAULT_ACCOUNT_ID}.json`),
      JSON.stringify({ account_id: DEFAULT_ACCOUNT_ID, public_key: DEFAULT_PUBLIC_KEY, private_key: DEFAULT_PRIVATE_KEY }),
    );
    const env3 = { ...env, SANDBOX_RPC: sb3.rpcUrl, NEAR_CREDENTIALS_DIR: creds3, SANDBOX_RECORD: join(creds3, 'record.json') };
    const run3 = (wasm: string | null, ...flags: string[]) =>
      spawnSync('node', ['scripts/deploy.ts', '--network', 'sandbox', ...flags], {
        env: wasm ? { ...env3, SANDBOX_WASM: wasm } : env3,
        encoding: 'utf8',
      });
    const p3 = new JsonRpcProvider({ url: sb3.rpcUrl });
    const v3 = { contractId: 'id.sandbox', rpcUrl: sb3.rpcUrl, finality: 'optimistic' as const };
    const old = run3('contracts/registry/res/bonuz_id_registry-2.1.0.wasm');
    if (old.status) console.log('--- 2.1.0 run', old.stdout, old.stderr);
    const s21 = await viewRegistry<{ version: string }>('status', {}, v3);
    expect(old.status === 0 && s21.version === '2.1.0', 'a 2.1.0 registry deploys from the frozen wasm');
    const fcFile = join(creds3, 'sandbox', 'registrar-fc', 'registrar.sandbox.json');
    const reg3 = new Account('registrar.sandbox', p3, new KeyPairSigner(KeyPair.fromString(JSON.parse(readFileSync(fcFile, 'utf8')).private_key)));
    const now3 = Math.floor(Number((await p3.viewBlock({ finality: 'final' })).header.timestamp_nanosec) / 1e9);
    const l3 = signLink(randomBytes(32), { bonuzId: 'mende', network: 'testnet', expires: now3 + 480, recipient: 'id.sandbox', nonce: randomBytes(32) });
    await reg3.callFunction({
      contractId: 'id.sandbox',
      methodName: 'link',
      args: { account_id: l3.accountId, bonuz_id: l3.bonuzId, public_key: l3.publicKey, nonce: l3.nonce, signature: l3.signature, message: l3.message },
      gas: 30_000_000_000_000n,
    });
    // An upgrade that would leave the registry under its reserve is refused before sending.
    const id3 = new Account('id.sandbox', p3, new KeyPairSigner(KeyPair.fromString(JSON.parse(readFileSync(join(creds3, 'sandbox', 'id.sandbox.json'), 'utf8')).private_key)));
    const root3 = new Account(DEFAULT_ACCOUNT_ID, p3, new KeyPairSigner(KeyPair.fromString(DEFAULT_PRIVATE_KEY)));
    await id3.transfer({ receiverId: DEFAULT_ACCOUNT_ID, amount: 35n * 10n ** 23n });
    const codeBefore3 = (await p3.viewAccount({ accountId: 'id.sandbox' })).code_hash;
    const poor = run3(null, '--upgrade-code', '--new-registrar-key');
    expect(
      poor.status === 1 && poor.stderr.includes('under its 1 NEAR reserve') && (await p3.viewAccount({ accountId: 'id.sandbox' })).code_hash === codeBefore3,
      'an upgrade that would leave the registry under its reserve is refused, nothing sent',
    );
    await root3.transfer({ receiverId: 'id.sandbox', amount: 35n * 10n ** 23n });
    const up3 = run3(null, '--upgrade-code', '--new-registrar-key');
    if (up3.status) console.log('--- upgrade run', up3.stdout, up3.stderr);
    expect(up3.status === 0 && up3.stdout.includes('upgrade_code_and_migrate_2_1_to_2_2'), '2.1.0 -> 2.2.0 goes out as deploy + migrate in one transaction');
    expect((up3.stdout.match(/check ok/g)?.length ?? 0) === 13, 'all 13 checks pass after the upgrade and the new registrar key');
    const s22 = await viewRegistry<{ version: string; links: string; legacy_links: string }>('status', {}, v3);
    expect(s22.version === '2.2.0' && s22.links === '1' && s22.legacy_links === '1', 'status: 2.2.0, the 2.1.0 link kept and counted as legacy');
    expect((await bonuzOf(l3.accountId, v3)) === 'mende', 'the 2.1.0 link still reads after the migration');
    expect(existsSync(join(creds3, 'sandbox', 'registrar-fc', 'registrar.sandbox.1.json')), 'the old registrar key file is kept as .1.json');
    const again3 = run3(null, '--upgrade-code', '--new-registrar-key');
    expect(again3.status === 0 && again3.stdout.includes('upgrade_code_keep_state'), 'a second 2.2.0 upgrade is a plain code deploy');
  } finally {
    await sb3.tearDown().catch(() => {});
  }
} finally {
  await sandbox.tearDown().catch(() => {});
}
console.log(failures ? `\n${failures} FAILED` : '\nall rehearsal checks passed');
process.exit(failures ? 1 : 0);
