/**
 * @bonuz/near-link: the one implementation of the bonuz ID link message.
 *
 * Used by the bonuz app (to build and sign), the bonuz backend (to check before
 * it calls the registry) and the tests. The rules here match the registry
 * contract's `proof.rs` line for line, and `vectors.json` holds the shared
 * positive and negative cases both test suites run, so the two cannot drift.
 *
 * Plain TypeScript, no framework. Runs in Node 22.18+ (type stripping), React
 * Native and bundlers. Depends only on @noble/hashes, @scure/base and tweetnacl.
 */
import { sha256 } from '@noble/hashes/sha2.js';
import { base58, base64 } from '@scure/base';
import nacl from 'tweetnacl';

export type Network = 'testnet' | 'mainnet';

/** NEP-413 tag, 2^31 + 413, written little-endian before the payload. */
export const NEP413_TAG = 2 ** 31 + 413;
/** How far ahead of block time a message may expire. */
export const MAX_EXPIRY_AHEAD_SECONDS = 600;
/** Expiry the backend puts on a fresh challenge: 8 minutes, 2 under the contract's ceiling. */
export const CHALLENGE_EXPIRY_SECONDS = 480;
export const MAX_HANDLE_LEN = 64;

/** The deployed registries. */
export const REGISTRY: Record<Network, { contractId: string; rpcUrl: string } | undefined> = {
  testnet: { contractId: 'id.bonuz.testnet', rpcUrl: 'https://rpc.testnet.near.org' },
  mainnet: undefined,
};

const LINE1_PREFIX = 'Link NEAR account ';
const LINE1_MIDDLE = ' to bonuz ID @';
const LINE2_PREFIX = 'Network: ';
const LINE3_PREFIX = 'Expires: ';
// Rust's char::is_control (Unicode Cc) minus \n.
const CONTROL = /[\u0000-\u0009\u000B-\u001F\u007F-\u009F]/;
const utf8 = new TextEncoder();

/** Canonical form for both sides: trimmed, no leading `@`, lowercase. Sign and compare only this. */
export const normalizeHandle = (value: string): string => value.trim().replace(/^@/, '').toLowerCase();
export const normalizeAccountId = (value: string): string => value.trim().replace(/^@/, '').toLowerCase();

export interface LinkMessage {
  accountId: string;
  bonuzId: string;
  network: string;
  expires: number;
}

/** The text the app shows in the biometric prompt and signs. Inputs must be canonical. */
export const buildLinkMessage = ({ accountId, bonuzId, network, expires }: LinkMessage): string =>
  `${LINE1_PREFIX}${accountId}${LINE1_MIDDLE}${bonuzId}\n${LINE2_PREFIX}${network}\n${LINE3_PREFIX}${expires}`;

export class LinkError extends Error {}

/**
 * Parses the exact three-line message. `\n` only; `\r`, other control
 * characters, a trailing newline, extra lines or extra text are refused.
 */
export const parseLinkMessage = (message: string): LinkMessage => {
  if (CONTROL.test(message)) throw new LinkError('message contains a control character');
  const lines = message.split('\n');
  if (lines.length !== 3) throw new LinkError('message must have exactly three lines');
  const [l1, l2, l3] = lines as [string, string, string];
  if (!l1.startsWith(LINE1_PREFIX)) throw new LinkError("line 1 must start with 'Link NEAR account '");
  const rest = l1.slice(LINE1_PREFIX.length);
  const at = rest.indexOf(LINE1_MIDDLE);
  if (at < 0) throw new LinkError("line 1 must read 'Link NEAR account <account> to bonuz ID @<handle>'");
  const accountId = rest.slice(0, at);
  const bonuzId = rest.slice(at + LINE1_MIDDLE.length);
  if (!l2.startsWith(LINE2_PREFIX)) throw new LinkError("line 2 must start with 'Network: '");
  if (!l3.startsWith(LINE3_PREFIX)) throw new LinkError("line 3 must start with 'Expires: '");
  const expiresText = l3.slice(LINE3_PREFIX.length);
  if (!/^(0|[1-9][0-9]{0,18})$/.test(expiresText)) {
    throw new LinkError('Expires must be unix seconds in plain digits');
  }
  if (!accountId || accountId.includes(' ') || !bonuzId || bonuzId.includes(' ')) {
    throw new LinkError('line 1 must name one account and one handle');
  }
  return { accountId, bonuzId, network: l2.slice(LINE2_PREFIX.length), expires: Number(expiresText) };
};

/** Refusal text for a handle, or null. Canonical form: 1 to 64 of `a-z 0-9 _ . -`. */
export const handleError = (bonuzId: string): string | null => {
  const bytes = utf8.encode(bonuzId).length;
  if (bytes === 0 || bytes > MAX_HANDLE_LEN) return 'bonuz ID must be 1 to 64 characters';
  if (!/^[a-z0-9_.-]+$/.test(bonuzId)) return 'bonuz ID may only contain a-z, 0-9, _, . and - (lowercase)';
  return null;
};

/** One rule for "this message expired" and "this nonce may be pruned": valid while now < expires. */
export const isExpired = (expires: number | bigint, nowNs: bigint): boolean =>
  nowNs >= BigInt(expires) * 1_000_000_000n;

/** NEAR's account id rule: 2 to 64 chars, lowercase, parts of a-z 0-9 split by single - or _, joined by dots. */
export const isValidAccountId = (accountId: string): boolean =>
  accountId.length >= 2 &&
  accountId.length <= 64 &&
  /^(([a-z\d]+[-_])*[a-z\d]+\.)*([a-z\d]+[-_])*[a-z\d]+$/.test(accountId);

export const isImplicitAccountId = (accountId: string): boolean => /^[0-9a-f]{64}$/.test(accountId);

/** The implicit account of an ed25519 public key: its 32 bytes in lowercase hex. */
export const implicitAccountId = (publicKey: Uint8Array): string =>
  Array.from(publicKey, (b) => b.toString(16).padStart(2, '0')).join('');

export const publicKeyToString = (publicKey: Uint8Array): string => `ed25519:${base58.encode(publicKey)}`;

export const parsePublicKey = (text: string): Uint8Array => {
  if (!text.startsWith('ed25519:')) throw new LinkError('public key must be ed25519:<base58>');
  let raw: Uint8Array;
  try {
    raw = base58.decode(text.slice('ed25519:'.length));
  } catch {
    throw new LinkError('public key is not valid base58');
  }
  if (raw.length !== 32) throw new LinkError('public key must be 32 bytes');
  return raw;
};

const u32le = (n: number): Uint8Array => {
  const out = new Uint8Array(4);
  new DataView(out.buffer).setUint32(0, n, true);
  return out;
};

const concat = (parts: Uint8Array[]): Uint8Array => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
};

export interface Nep413Payload {
  message: string;
  /** 32 bytes */
  nonce: Uint8Array;
  /** The registry contract id. */
  recipient: string;
}

/** The bytes NEP-413 hashes: tag, then borsh(message, nonce, recipient, callbackUrl = None). */
export const nep413Preimage = ({ message, nonce, recipient }: Nep413Payload): Uint8Array => {
  if (nonce.length !== 32) throw new LinkError('nonce must be 32 bytes');
  const m = utf8.encode(message);
  const r = utf8.encode(recipient);
  return concat([u32le(NEP413_TAG), u32le(m.length), m, nonce, u32le(r.length), r, new Uint8Array([0])]);
};

export const nep413Hash = (payload: Nep413Payload): Uint8Array => sha256(nep413Preimage(payload));

/** Signs like a NEAR wallet does for NEP-413. `secretSeed` is the 32-byte ed25519 seed. */
export const signNep413 = (secretSeed: Uint8Array, payload: Nep413Payload): Uint8Array =>
  nacl.sign.detached(nep413Hash(payload), nacl.sign.keyPair.fromSeed(secretSeed).secretKey);

export const verifyNep413 = (
  payload: Nep413Payload & { publicKey: Uint8Array; signature: Uint8Array },
): boolean =>
  payload.signature.length === 64 &&
  payload.publicKey.length === 32 &&
  nacl.sign.detached.verify(nep413Hash(payload), payload.signature, payload.publicKey);

/** What `link` on the registry takes, and what the backend's verify route receives. */
export interface LinkInput {
  accountId: string;
  bonuzId: string;
  /** `ed25519:<base58>` */
  publicKey: string;
  /** base64, 32 bytes */
  nonce: string;
  /** base64, 64 bytes */
  signature: string;
  message: string;
}

export interface LinkContext {
  /** The registry contract id (NEP-413 recipient). */
  recipient: string;
  network: Network | string;
  /** Block time (contract) or the backend's clock, in nanoseconds. */
  nowNs: bigint;
  /** Named accounts allowed (their key is checked by the backend over RPC). */
  allowRegistrarChecked: boolean;
}

export type LinkCheck =
  | { ok: true; checkedBy: 'contract' | 'registrar'; expires: number }
  | { ok: false; error: string };

const decodeB64 = (text: string, what: string): Uint8Array => {
  try {
    return base64.decode(text);
  } catch {
    throw new LinkError(`${what} is not valid base64`);
  }
};

/**
 * Every check the registry contract makes that needs no contract state, in
 * the same order and with the same refusal texts. Nonce reuse and the
 * one-to-one rules stay with the contract (and the backend's challenge store).
 */
export const checkLink = (input: LinkInput, ctx: LinkContext): LinkCheck => {
  try {
    const bad = handleError(input.bonuzId);
    if (bad) return { ok: false, error: bad };
    if (input.accountId.startsWith('0x')) {
      return { ok: false, error: 'Ethereum-style NEAR accounts (0x...) cannot be linked yet' };
    }
    if (/^[0-9a-fA-F]{64}$/.test(input.accountId) && /[A-F]/.test(input.accountId)) {
      return { ok: false, error: 'implicit accounts must be lowercase hex' };
    }
    if (!isValidAccountId(input.accountId)) return { ok: false, error: 'this is not a valid NEAR account id' };
    const nonce = decodeB64(input.nonce, 'nonce');
    if (nonce.length !== 32) return { ok: false, error: 'nonce must be 32 bytes' };
    const signature = decodeB64(input.signature, 'signature');
    if (signature.length !== 64) return { ok: false, error: 'signature must be 64 bytes' };
    const publicKey = parsePublicKey(input.publicKey);

    if (!verifyNep413({ message: input.message, nonce, recipient: ctx.recipient, publicKey, signature })) {
      return { ok: false, error: 'invalid signature (NEP-413, recipient = this contract)' };
    }

    const msg = parseLinkMessage(input.message);
    if (msg.accountId !== input.accountId) {
      return { ok: false, error: 'the signed message names a different NEAR account' };
    }
    if (msg.bonuzId !== input.bonuzId) return { ok: false, error: 'the signed message names a different bonuz ID' };
    if (msg.network !== ctx.network) return { ok: false, error: 'the signed message is for a different network' };
    const expires = BigInt((input.message.split('\n')[2] as string).slice(LINE3_PREFIX.length));
    if (isExpired(expires, ctx.nowNs)) return { ok: false, error: 'the signed message has expired' };
    if (expires * 1_000_000_000n > ctx.nowNs + BigInt(MAX_EXPIRY_AHEAD_SECONDS) * 1_000_000_000n) {
      return { ok: false, error: 'the signed message expires more than 10 minutes ahead' };
    }

    if (isImplicitAccountId(input.accountId)) {
      if (implicitAccountId(publicKey) !== input.accountId) {
        return { ok: false, error: 'the key does not belong to this implicit account' };
      }
      return { ok: true, checkedBy: 'contract', expires: msg.expires };
    }
    if (!ctx.allowRegistrarChecked) return { ok: false, error: 'this registry only links implicit accounts' };
    return { ok: true, checkedBy: 'registrar', expires: msg.expires };
  } catch (e) {
    if (e instanceof LinkError) return { ok: false, error: e.message };
    throw e;
  }
};

/** Builds and signs a link the way the bonuz app does. Tests and the demo use it. */
export const signLink = (
  secretSeed: Uint8Array,
  args: { bonuzId: string; network: Network; expires: number; recipient: string; nonce: Uint8Array },
): LinkInput => {
  const publicKey = nacl.sign.keyPair.fromSeed(secretSeed).publicKey;
  const accountId = implicitAccountId(publicKey);
  const message = buildLinkMessage({ accountId, bonuzId: args.bonuzId, network: args.network, expires: args.expires });
  const signature = signNep413(secretSeed, { message, nonce: args.nonce, recipient: args.recipient });
  return {
    accountId,
    bonuzId: args.bonuzId,
    publicKey: publicKeyToString(publicKey),
    nonce: base64.encode(args.nonce),
    signature: base64.encode(signature),
    message,
  };
};

// ---- tiny read-only registry client (JSON-RPC, no SDK) ----

export interface RegistryLink {
  bonuz_id: string;
  public_key: string;
  checked_by: 'contract' | 'registrar';
  linked_at_block: string;
  linked_at: string;
}

export interface RegistryOpts {
  contractId: string;
  rpcUrl: string;
  /** `final` (default, a second or two behind) or `optimistic`. */
  finality?: 'final' | 'optimistic';
  fetchImpl?: typeof fetch;
}

export const viewRegistry = async <T>(
  method: 'bonuz_of' | 'account_of' | 'link_of' | 'status',
  args: Record<string, string>,
  opts: RegistryOpts,
): Promise<T> => {
  const res = await (opts.fetchImpl ?? fetch)(opts.rpcUrl, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 'near-link',
      method: 'query',
      params: {
        request_type: 'call_function',
        finality: opts.finality ?? 'final',
        account_id: opts.contractId,
        method_name: method,
        args_base64: base64.encode(utf8.encode(JSON.stringify(args))),
      },
    }),
  });
  if (!res.ok) throw new Error(`NEAR RPC answered ${res.status}`);
  const body = (await res.json()) as { result?: { result?: number[] }; error?: unknown };
  if (!body.result?.result) throw new Error(`NEAR RPC error: ${JSON.stringify(body.error ?? body)}`);
  return JSON.parse(new TextDecoder().decode(Uint8Array.from(body.result.result))) as T;
};

export const bonuzOf = (accountId: string, opts: RegistryOpts) =>
  viewRegistry<string | null>('bonuz_of', { account_id: accountId }, opts);
export const accountOf = (bonuzId: string, opts: RegistryOpts) =>
  viewRegistry<string | null>('account_of', { bonuz_id: bonuzId }, opts);
export const linkOf = (accountId: string, opts: RegistryOpts) =>
  viewRegistry<RegistryLink | null>('link_of', { account_id: accountId }, opts);
