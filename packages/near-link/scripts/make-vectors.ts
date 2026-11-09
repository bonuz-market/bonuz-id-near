/**
 * Writes vectors.json: the shared cases the TypeScript tests and the Rust
 * contract tests both run. Keys, nonces and times are fixed, so the file is
 * reproducible byte for byte (ed25519 signatures are deterministic).
 *
 * The expected outcome of every case is written here BY HAND, never computed
 * by the code under test. Run: node scripts/make-vectors.ts
 */
import { writeFileSync } from 'node:fs';
import { base64 } from '@scure/base';
import nacl from 'tweetnacl';
import {
  buildLinkMessage,
  implicitAccountId,
  publicKeyToString,
  signNep413,
  type LinkInput,
} from '../src/near-link.ts';

const RECIPIENT = 'id.bonuz.testnet';
const NOW = 1_763_000_000; // unix seconds, the "block time" of every case
const seed = (b: number) => new Uint8Array(32).fill(b);
const nonce = (b: number) => new Uint8Array(32).fill(b);
const A = seed(1);
const B = seed(2);
const pk = (s: Uint8Array) => nacl.sign.keyPair.fromSeed(s).publicKey;
const accA = implicitAccountId(pk(A));
const accB = implicitAccountId(pk(B));

interface Case {
  name: string;
  why: string;
  input: LinkInput;
  context: { recipient: string; network: string; now_ns: string; allow_registrar_checked: boolean };
  expect: { ok: true; checked_by: 'contract' | 'registrar' } | { ok: false; error: string };
}

/** Signs `message` with `key` for `recipient`, then lets a case tamper with the call args. */
const make = (o: {
  name: string;
  why: string;
  key?: Uint8Array;
  account?: string;
  argAccount?: string;
  handle?: string;
  argHandle?: string;
  network?: string;
  expires?: number;
  recipient?: string;
  message?: string;
  nonceByte?: number;
  flipSignature?: boolean;
  allowNamed?: boolean;
  expect: Case['expect'];
}): Case => {
  const key = o.key ?? A;
  const account = o.account ?? implicitAccountId(pk(key));
  const handle = o.handle ?? 'mende';
  const message =
    o.message ??
    buildLinkMessage({ accountId: account, bonuzId: handle, network: o.network ?? 'testnet', expires: o.expires ?? NOW + 480 });
  const n = nonce(o.nonceByte ?? 7);
  const sig = signNep413(key, { message, nonce: n, recipient: o.recipient ?? RECIPIENT });
  if (o.flipSignature) sig[0] ^= 1;
  return {
    name: o.name,
    why: o.why,
    input: {
      accountId: o.argAccount ?? account,
      bonuzId: o.argHandle ?? handle,
      publicKey: publicKeyToString(pk(key)),
      nonce: base64.encode(n),
      signature: base64.encode(sig),
      message,
    },
    context: {
      recipient: RECIPIENT,
      network: 'testnet',
      now_ns: (BigInt(NOW) * 1_000_000_000n).toString(),
      allow_registrar_checked: o.allowNamed ?? true,
    },
    expect: o.expect,
  };
};

const err = (error: string) => ({ ok: false as const, error });
const upperA = accA.toUpperCase();

const cases: Case[] = [
  make({ name: 'valid_implicit', why: 'app wallet, 8-minute expiry', expect: { ok: true, checked_by: 'contract' } }),
  make({
    name: 'valid_named_on_testnet',
    why: 'named account, key checked by the backend',
    account: 'alice.testnet',
    expect: { ok: true, checked_by: 'registrar' },
  }),
  make({
    name: 'valid_expiry_exactly_ten_minutes_ahead',
    why: 'the ceiling itself is allowed',
    expires: NOW + 600,
    expect: { ok: true, checked_by: 'contract' },
  }),
  make({
    name: 'valid_backend_clock_90s_ahead',
    why: 'Expires = backend now + 8 min, backend 90 s ahead of the chain',
    expires: NOW + 90 + 480,
    expect: { ok: true, checked_by: 'contract' },
  }),
  make({
    name: 'valid_handle_with_dot_and_dash',
    why: 'bonuz handles may contain . and -',
    handle: 'm.en-de_1',
    expect: { ok: true, checked_by: 'contract' },
  }),
  make({
    name: 'named_refused_in_implicit_only_mode',
    why: 'mainnet starts with allow_registrar_checked off',
    account: 'alice.near',
    allowNamed: false,
    expect: err('this registry only links implicit accounts'),
  }),
  make({
    name: 'wrong_recipient',
    why: 'signature made for another contract',
    recipient: 'evil.testnet',
    expect: err('invalid signature (NEP-413, recipient = this contract)'),
  }),
  make({
    name: 'flipped_signature_bit',
    why: 'tampered signature',
    flipSignature: true,
    expect: err('invalid signature (NEP-413, recipient = this contract)'),
  }),
  make({
    name: 'wrong_network',
    why: 'mainnet message sent to the testnet registry',
    network: 'mainnet',
    expect: err('the signed message is for a different network'),
  }),
  make({
    name: 'expired_at_exactly_expires',
    why: 'valid only while now < Expires',
    expires: NOW,
    expect: err('the signed message has expired'),
  }),
  make({
    name: 'expires_too_far_ahead',
    why: 'one second over the 10-minute ceiling',
    expires: NOW + 601,
    expect: err('the signed message expires more than 10 minutes ahead'),
  }),
  make({
    name: 'different_handle',
    why: 'signed for mende, submitted for another handle',
    argHandle: 'alice',
    expect: err('the signed message names a different bonuz ID'),
  }),
  make({
    name: 'handle_prefix',
    why: '@men must never match @mende',
    argHandle: 'men',
    expect: err('the signed message names a different bonuz ID'),
  }),
  make({
    name: 'handle_case_in_message',
    why: 'message names @Mende, call names mende',
    handle: 'Mende',
    argHandle: 'mende',
    expect: err('the signed message names a different bonuz ID'),
  }),
  make({
    name: 'handle_case_in_args',
    why: 'handles are canonical lowercase',
    handle: 'Mende',
    expect: err('bonuz ID may only contain a-z, 0-9, _, . and - (lowercase)'),
  }),
  make({
    name: 'different_account',
    why: 'message names account B, call names account A (key A)',
    account: accB,
    argAccount: accA,
    expect: err('the signed message names a different NEAR account'),
  }),
  make({
    name: 'implicit_account_with_another_key',
    why: 'key A signs for implicit account B',
    account: accB,
    expect: err('the key does not belong to this implicit account'),
  }),
  make({
    name: 'uppercase_implicit_hex',
    why: 'implicit ids are lowercase only',
    account: upperA,
    expect: err('implicit accounts must be lowercase hex'),
  }),
  make({
    name: 'eth_implicit_account',
    why: '0x accounts use secp256k1, not ed25519',
    account: '0x1234567890abcdef1234567890abcdef12345678',
    expect: err('Ethereum-style NEAR accounts (0x...) cannot be linked yet'),
  }),
  make({
    name: 'extra_line',
    why: 'nothing after the three lines',
    message: `${buildLinkMessage({ accountId: accA, bonuzId: 'mende', network: 'testnet', expires: NOW + 480 })}\nNote: hi`,
    expect: err('message must have exactly three lines'),
  }),
  make({
    name: 'trailing_newline',
    why: 'no trailing newline',
    message: `${buildLinkMessage({ accountId: accA, bonuzId: 'mende', network: 'testnet', expires: NOW + 480 })}\n`,
    expect: err('message must have exactly three lines'),
  }),
  make({
    name: 'crlf_line_endings',
    why: 'LF only',
    message: buildLinkMessage({ accountId: accA, bonuzId: 'mende', network: 'testnet', expires: NOW + 480 }).replaceAll(
      '\n',
      '\r\n',
    ),
    expect: err('message contains a control character'),
  }),
  make({
    name: 'leading_zero_expiry',
    why: 'Expires is plain digits, no padding',
    message: buildLinkMessage({ accountId: accA, bonuzId: 'mende', network: 'testnet', expires: NOW + 480 }).replace(
      'Expires: ',
      'Expires: 0',
    ),
    expect: err('Expires must be unix seconds in plain digits'),
  }),
  make({
    name: 'fullwidth_at_sign',
    why: 'Unicode look-alike of @',
    message: buildLinkMessage({ accountId: accA, bonuzId: 'mende', network: 'testnet', expires: NOW + 480 }).replace(
      ' @mende',
      ' ＠mende',
    ),
    expect: err("line 1 must read 'Link NEAR account <account> to bonuz ID @<handle>'"),
  }),
  make({
    name: 'extra_text_after_handle',
    why: 'one value per line',
    message: buildLinkMessage({ accountId: accA, bonuzId: 'mende', network: 'testnet', expires: NOW + 480 }).replace(
      '@mende',
      '@mende please',
    ),
    expect: err('line 1 must name one account and one handle'),
  }),
];

const names = new Set(cases.map((c) => c.name));
if (names.size !== cases.length) throw new Error('duplicate case name');

const out = {
  about:
    'Shared cases for @bonuz/near-link (TypeScript) and the registry contract (Rust). Both must give exactly `expect`. Generated by scripts/make-vectors.ts; expectations are written by hand.',
  nep413: { tag: 2 ** 31 + 413, callback_url: null },
  cases,
};
writeFileSync(new URL('../vectors.json', import.meta.url), `${JSON.stringify(out, null, 2)}\n`);
console.log(`wrote ${cases.length} cases`);
