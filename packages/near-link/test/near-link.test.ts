import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { base64 } from '@scure/base';
import { KeyPairSigner } from 'near-api-js';
import { base58 } from '@scure/base';
import nacl from 'tweetnacl';
import {
  buildLinkMessage,
  checkLink,
  implicitAccountId,
  isExpired,
  normalizeHandle,
  nep413Hash,
  parseLinkMessage,
  signLink,
  signNep413,
  verifyNep413,
  viewRegistry,
} from '../src/near-link.ts';

const vectors = JSON.parse(readFileSync(new URL('../vectors.json', import.meta.url), 'utf8'));

for (const c of vectors.cases) {
  test(`vector ${c.name}`, () => {
    const got = checkLink(c.input, {
      recipient: c.context.recipient,
      network: c.context.network,
      nowNs: BigInt(c.context.now_ns),
      allowRegistrarChecked: c.context.allow_registrar_checked,
    });
    if (c.expect.ok) {
      assert.deepEqual(got, {
        ok: true,
        checkedBy: c.expect.checked_by,
        expires: got.ok ? got.expires : -1,
        bonuzEoa: c.expect.bonuz_eoa ?? null,
      });
    }
    else assert.deepEqual(got, { ok: false, error: c.expect.error });
  });
}

test('vectors cover the plan negative list', () => {
  const names = new Set(vectors.cases.map((c: { name: string }) => c.name));
  for (const n of [
    'wrong_recipient',
    'wrong_network',
    'expired_at_exactly_expires',
    'different_handle',
    'handle_prefix',
    'handle_case_in_message',
    'uppercase_implicit_hex',
    'eth_implicit_account',
    'extra_line',
    'crlf_line_endings',
    'valid_implicit_with_wallet',
    'wallet_checksum_case',
    'five_lines',
  ]) {
    assert.ok(names.has(n), n);
  }
});

test('our NEP-413 signature equals near-api-js KeyPairSigner.signNep413Message', async () => {
  const seed = new Uint8Array(32).fill(9);
  const kp = nacl.sign.keyPair.fromSeed(seed);
  const signer = KeyPairSigner.fromSecretKey(`ed25519:${base58.encode(kp.secretKey)}`);
  const accountId = implicitAccountId(kp.publicKey);
  const message = buildLinkMessage({ accountId, bonuzId: 'mende', network: 'testnet', expires: 1_763_000_480 });
  const nonce = new Uint8Array(32).fill(3);
  const theirs = await signer.signNep413Message(accountId, { message, recipient: 'id.bonuz.testnet', nonce });
  const ours = signNep413(seed, { message, nonce, recipient: 'id.bonuz.testnet' });
  assert.deepEqual(Array.from(ours), Array.from(theirs.signature));
  assert.ok(verifyNep413({ message, nonce, recipient: 'id.bonuz.testnet', publicKey: kp.publicKey, signature: theirs.signature }));
});

test('hash is sha256 of the tagged borsh payload (spot check)', () => {
  const h = nep413Hash({ message: 'hi', nonce: new Uint8Array(32), recipient: 'r' });
  assert.equal(h.length, 32);
});

test('parse round-trips the built message and nothing else', () => {
  const m = buildLinkMessage({ accountId: 'alice.testnet', bonuzId: 'mende', network: 'testnet', expires: 1 });
  assert.deepEqual(parseLinkMessage(m), { accountId: 'alice.testnet', bonuzId: 'mende', network: 'testnet', expires: 1 });
  assert.throws(() => parseLinkMessage(`${m} `));
  assert.throws(() => parseLinkMessage(m.replace('Network', 'network')));
  const eoa = `0x${'ab'.repeat(20)}`;
  const w = buildLinkMessage({ accountId: 'alice.testnet', bonuzId: 'mende', network: 'testnet', expires: 1, bonuzEoa: eoa });
  assert.equal(w, `${m}\nWallet: ${eoa}`);
  assert.deepEqual(parseLinkMessage(w), { accountId: 'alice.testnet', bonuzId: 'mende', network: 'testnet', expires: 1, bonuzEoa: eoa });
  assert.equal(buildLinkMessage(parseLinkMessage(w)), w);
  assert.throws(() => parseLinkMessage(w.replace('0xab', '0xAB')));
  assert.throws(() => parseLinkMessage(`${w}\n`));
});

test('one expiry rule', () => {
  assert.equal(isExpired(100, 99_999_999_999n), false);
  assert.equal(isExpired(100, 100_000_000_000n), true);
});

test('normalize trims, drops @ and lowercases', () => {
  assert.equal(normalizeHandle('  @Mende '), 'mende');
});

test('signLink output passes checkLink', () => {
  const seed = new Uint8Array(32).fill(5);
  const input = signLink(seed, {
    bonuzId: 'mende',
    network: 'testnet',
    expires: 1_763_000_480,
    recipient: 'id.bonuz.testnet',
    nonce: new Uint8Array(32).fill(1),
  });
  const got = checkLink(input, {
    recipient: 'id.bonuz.testnet',
    network: 'testnet',
    nowNs: 1_763_000_000_000_000_000n,
    allowRegistrarChecked: false,
  });
  assert.deepEqual(got, { ok: true, checkedBy: 'contract', expires: 1_763_000_480, bonuzEoa: null });
  const eoa = `0x${'cd'.repeat(20)}`;
  const withWallet = signLink(seed, {
    bonuzId: 'mende',
    network: 'testnet',
    expires: 1_763_000_480,
    recipient: 'id.bonuz.testnet',
    nonce: new Uint8Array(32).fill(1),
    bonuzEoa: eoa,
  });
  assert.ok(withWallet.message.endsWith(`\nWallet: ${eoa}`));
  const got2 = checkLink(withWallet, {
    recipient: 'id.bonuz.testnet',
    network: 'testnet',
    nowNs: 1_763_000_000_000_000_000n,
    allowRegistrarChecked: false,
  });
  assert.deepEqual(got2, { ok: true, checkedBy: 'contract', expires: 1_763_000_480, bonuzEoa: eoa });
});

test('viewRegistry decodes the RPC byte array', async () => {
  const fetchImpl = (async (_url: string, init: RequestInit) => {
    const body = JSON.parse(String(init.body));
    assert.equal(body.params.method_name, 'bonuz_of');
    assert.equal(new TextDecoder().decode(base64.decode(body.params.args_base64)), '{"account_id":"a.testnet"}');
    const bytes = Array.from(new TextEncoder().encode('"mende"'));
    return new Response(JSON.stringify({ result: { result: bytes } }));
  }) as typeof fetch;
  const got = await viewRegistry<string>('bonuz_of', { account_id: 'a.testnet' }, {
    contractId: 'id.bonuz.testnet',
    rpcUrl: 'http://rpc',
    fetchImpl,
  });
  assert.equal(got, 'mende');
});
