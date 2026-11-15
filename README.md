# bonuz ID on NEAR

bonuz is a live consumer app (iOS and Android) where people collect rewards for showing up at real
places and events. Every bonuz user has a **bonuz ID**, a public profile at `bonuz.id/<handle>`
that lists their verified socials and wallets.

This repo adds **NEAR to the bonuz ID**. A bonuz user taps "Show on my bonuz ID" in the app's NEAR
wallet. The wallet signs one free message (NEP-413: no transaction, no gas, the account does not
even have to exist on chain). bonuz checks it, marks the NEAR account verified on the bonuz ID, and
records the pair in a public **registry contract on NEAR**, which re-checks the signature itself.
After that, anyone, including AI agents, can ask NEAR "which bonuz ID is this account?" and get a
handle backed by a real person's profile.

Built for the NEAR HACKATHON (November 2026). All code in this repo was written with Claude
(Anthropic's AI), directed by the bonuz team.

## Try it

You need Node.js 22.18 or newer. Nothing else: the contract wasm is committed.

### 0. The live mainnet registry (no install)

The production registry runs on NEAR mainnet at
[`id.bonuz-id.near`](https://nearblocks.io/address/id.bonuz-id.near), and the bonuz backend in
production writes to it (the app side is merged and ships with the next app release). Ask it for its status:

```bash
curl -s https://rpc.mainnet.fastnear.com -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"query","params":{"request_type":"call_function","finality":"final","account_id":"id.bonuz-id.near","method_name":"status","args_base64":"e30="}}' | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>console.log(Buffer.from(JSON.parse(s).result.result).toString()))'
```

You see `"version":"2.1.0"`, `"network":"mainnet"`, owner `bonuz-id.near`, registrar
`registrar.bonuz-id.near` and `"links"`, the number of NEAR accounts linked right now. Mainnet links
implicit accounts only, so the contract itself checks every signature. The steps below use testnet,
where you can also run the whole flow yourself.

### 1. Read the live testnet registry (no install)

```bash
curl -s https://rpc.testnet.near.org -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"query","params":{"request_type":"call_function","finality":"final","account_id":"id.bonuz.testnet","method_name":"account_of","args_base64":"eyJib251el9pZCI6ImJvbnV6X2RlbW8ifQ=="}}' | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>console.log(Buffer.from(JSON.parse(s).result.result).toString()))'
```

You see `"e8f85fc15d2b62fe970b472872829490763922313f4762f8bceb2e083c7f08e2"`: an app-style NEAR
wallet linked to the handle `bonuz_demo`. That account has never been funded and does not exist on
chain. It never needed NEAR to be linked. (`args_base64` is `{"bonuz_id":"bonuz_demo"}`.)

With [near-cli-rs](https://github.com/near/near-cli-rs) installed, the same in one line:

```bash
near contract call-function as-read-only id.bonuz.testnet link_of json-args '{"account_id":"bonuz.testnet"}' network-config testnet now
```

You see `"bonuz_id": "bonuz"` and `"checked_by": "registrar"` (a named account; see "What the
contract proves" below). `status` shows the version (`2.0.1`), the registrar, the balance left and
the daily unlink cap.

### 2. Run the whole flow on a local NEAR node (one command)

```bash
npm ci
npm run demo
```

It starts a local NEAR sandbox (the first run downloads the node), deploys the registry, gives the
registrar a **function-call key** (registry only, two methods, 2 NEAR gas allowance), makes a
brand-new NEAR key the way the bonuz app does, signs the link message with it, links it with zero
deposit, reads it back, and shows two refusals. You see, among others:

```
[3] Make a brand-new app wallet (NEAR implicit account, never funded)
    ok  exists on chain: false
[4] The wallet signs the link message (NEP-413: no transaction, no gas)
        | Link NEAR account <64 hex> to bonuz ID @demo_user
        | Network: testnet
        | Expires: <unix seconds>
[5] The registrar submits link() with 0 deposit; the contract re-checks the signature
    ok  gas burnt 2.64 Tgas; the contract paid 490 bytes of storage
[6] Read it back, like any app or agent can
    ok  bonuz_of(...) = "demo_user"
[7] Refusals
    ok  same signature again -> refused: this nonce was already used
    ok  another wallet takes "demo_user" -> refused: this bonuz ID is linked to another NEAR account; unlink it first
```

### 3. Run the tests

```bash
npm test                 # @bonuz/near-link: 35 tests, incl. 27 shared vectors and a cross-check with near-api-js
npm run test:contract    # the contract: 36 unit tests, 3 sandbox tests, the same 27 vectors (needs Rust + cargo-near)
```

Rust toolchain for the contract: `rustup` with Rust 1.95, `cargo install cargo-near --version 0.23.0`.
`npm run build:contract` rebuilds `contracts/registry/res/bonuz_id_registry.wasm`; the sha256 of the
deployed code is in [`deploy/testnet.json`](deploy/testnet.json).

## How it works

```
bonuz app (NEAR wallet)          bonuz backend                      NEAR registry (this repo)
-----------------------          -------------                      -------------------------
1. ask for a challenge   ---->   32-byte nonce, bound to the
                                 logged-in bonuz user, 5 min
2. Face ID, sign NEP-413 ---->   checks the signature with
   (no tx, no gas)               @bonuz/near-link, checks the
                                 handle is the user's own,
                                 writes the verified link to
                                 the bonuz ID on Base
                                 registrar function-call key  ---->  link(): re-checks the signature,
                                                                     the message, the network, the
                                                                     expiry, the nonce; writes the pair;
                                                                     pays storage from its own balance
                                                                     bonuz_of / account_of / link_of
                                                                     readable by anyone
```

The message the wallet signs, exactly (LF line endings, nothing else allowed):

```
Link NEAR account <account_id> to bonuz ID @<handle>
Network: <testnet|mainnet>
Expires: <unix seconds>
```

NEP-413 `recipient` is the registry contract id and the `nonce` is the backend's 32-byte challenge.

### The contract (`contracts/registry`, Rust, near-sdk 5.30.1)

| Method | Who | What |
|---|---|---|
| `link(account_id, bonuz_id, public_key, nonce, signature, message)` | registrar only | Verifies the NEP-413 signature (recipient = this contract), parses the message exactly, checks it names this account, this handle and this network, that `now < Expires <= now + 10 min`, and that the nonce was never used. For an implicit account it checks `hex(public_key) == account_id`. One account to one handle: a new signed link from the same account replaces its old handle; a handle held by another account is refused until it is unlinked. Returns the block height. |
| `unlink_by_registrar(account_id)` | registrar only | Removal only, at most 100 per day (an owner-raised cap falls back to 100 at a set time on its own). |
| `unlink_by_owner()` | the linked account | Removes its own link (1 yoctoNEAR, full-access key). Works while paused. The account needs a little NEAR for gas; an unfunded app wallet removes its link from the bonuz app instead, which uses `unlink_by_registrar`. |
| `prune_nonces(limit)` | anyone | Deletes up to 50 used nonces whose messages have expired, starting at a random position so cleanup keeps up under steady traffic (each `link` also prunes up to 3). Never transfers anything. |
| `bonuz_of`, `account_of`, `link_of`, `status` | views | `status` includes the number of linked accounts (`links`). |
| `set_registrar`, `set_paused`, `set_reserve`, `set_unlink_daily_cap` | owner, 1 yoctoNEAR | |
| `propose_owner(new_owner)`, then `accept_owner()` | owner, then the proposed account; 1 yoctoNEAR each | Two-step owner change, so a typo cannot lock the owner out. |

Events (NEP-297, standard `bonuz_id_near`, version `2.0.0`): `link` and `unlink`, where `unlink`
carries a `reason`: `registrar`, `owner` or `replaced`.

**Why the registrar can use a function-call key.** A function-call access key cannot attach NEAR,
so it can never pay storage. The contract pays storage from its own pre-funded balance instead and
refuses to link when that balance falls under a reserve. The key is limited to this contract and to
`link` and `unlink_by_registrar`, with a capped gas allowance: a leaked key cannot move bonuz's NEAR
and cannot attach a NEAR account it does not hold (each link needs a fresh signature by that
account's own key). It can remove at most the day's unlink cap, every removal visible on chain, and
it could re-link a removed handle to an implicit account the attacker holds: which bonuz ID a
signer may claim is bonuz's attestation, not something the contract can check. The owner stops
that by pausing or replacing the registrar.
On a registry that accepts named accounts (testnet does), the contract cannot tie a named account
to its key, so a leaked registrar key could link a named account it does not hold; that is why a
mainnet registry starts with implicit accounts only.

Measured on testnet: `link` burns about 2.7 Tgas (about 0.0003 NEAR) and stores about 490 bytes for
an implicit account (0.0049 NEAR, including the nonce record that `prune_nonces` frees after it
expires); `unlink_by_registrar` burns about 2.1 Tgas.

### Next: registry 2.2.0 (built and rehearsed on testnet, not on mainnet)

2.2.0 keys every link to the bonuz user's creation wallet (`bonuz_eoa`, an EVM address that
never changes) instead of the handle, which becomes a label (`set_handle`): a rename keeps the
link, and a later holder of an old handle inherits nothing. Named accounts (`alice.near`) are
linked without trusting bonuz: the registrar writes a pending claim (`propose_link`) and the
link goes live only when the named account itself calls `accept_link` (1 yoctoNEAR). 2.1.0
links survive the upgrade (deploy + `migrate()` in one transaction) and are moved to their
wallet with `adopt_legacy_link`. Design, trade-offs, griefing limits and the prepared mainnet
steps: [`docs/registry-2.2.0.md`](docs/registry-2.2.0.md). Testnet rehearsal hashes:
[`deploy/testnet-2.2.0-rehearsal.json`](deploy/testnet-2.2.0-rehearsal.json). The mainnet
registry stays on 2.1.0 until at least 26 Nov.

### `packages/near-link` (TypeScript)

One implementation of the link message, used by the bonuz app (to build and sign), the bonuz backend
(to check before it calls the registry) and the tests: `buildLinkMessage`, `parseLinkMessage`,
`nep413Hash`, `signNep413`, `verifyNep413`, `checkLink` (the same checks as the contract, in the same
order, with the same refusal texts) and a small read-only registry client. It depends only on
`@noble/hashes`, `@scure/base` and `tweetnacl`.

[`vectors.json`](packages/near-link/vectors.json) holds 27 cases with hand-written expectations:
valid links, and a valid signature over the wrong recipient, the wrong network, an expired message,
a different handle, a handle that is a prefix of the real one (`@men` vs `@mende`), the wrong case,
uppercase implicit hex, an `0x` account, an extra line, `\r\n` line endings and more. The TypeScript
tests and the Rust contract tests both run this file, so the two implementations cannot drift apart.
A separate test checks that our NEP-413 signature equals near-api-js's `signNep413Message` byte for
byte.

## What the contract proves, and what it does not

- For an implicit account, bonuz **cannot attach** it to a handle without that account's key signing
  a message that names both the account and that exact handle. The contract checks both, whole
  strings only. (Named accounts are different; see below.)
- For an **implicit account** (the 64-hex accounts the bonuz app creates) the key-to-account check is
  done by the contract itself. It proves the key is the account's **original** key, not that it still
  controls the account: once funded, an implicit account's owner can add or delete keys. In short,
  the key proves origin, not current control.
- Whoever holds the NEAR key can link it to their own bonuz ID. If the same seed is imported into two
  bonuz accounts, the newest verified link wins. Owning the key is the proof; that is by design.
- The contract **cannot know** that the handle belongs to the person who signed. That part is bonuz's
  attestation: the registrar only writes for the logged-in owner of the handle.
- For a **named account** (`alice.near`) the key check is also bonuz's (`checked_by: "registrar"`),
  and it is a point in time: the key could be removed from the account later. The testnet registry
  accepts named accounts; a mainnet registry starts with implicit accounts only
  (`allow_registrar_checked: false`), and the sandbox demo runs in that mode.
- bonuz **can remove** any link without a user signature (`unlink_by_registrar`, used when a user
  edits or clears the field on their profile). It **cannot attach** a NEAR account to a handle
  without a fresh signature by that account. Every link and removal emits an event.
- Every signed message expires within 10 minutes of block time and every nonce works once, so an old
  signature can never be replayed later, even after the account's keys change.

## Deployments

**Mainnet** (live since 14 Nov 2026)

| | |
|---|---|
| Contract | `id.bonuz-id.near`, version 2.1.0 |
| Accepts | implicit accounts only (checked by the contract itself) |
| Owner (pause, registrar, caps; cannot link) | `bonuz-id.near` |
| Registrar (function-call key only) | `registrar.bonuz-id.near` |
| Explorer | https://nearblocks.io/address/id.bonuz-id.near |
| Deploy record, tx hashes, code hash | [`deploy/mainnet.json`](deploy/mainnet.json) |

**Testnet**

| | |
|---|---|
| Contract | `id.bonuz.testnet`, version 2.0.1 |
| Accepts | implicit accounts (checked by the contract) and named accounts (checked by bonuz, trusted to the registrar) |
| Owner (pause, registrar, caps; cannot link) | `bonuz.testnet` |
| Registrar (function-call key only) | `registrar.bonuz.testnet` |
| Explorer | https://testnet.nearblocks.io/address/id.bonuz.testnet |
| Deploy record, tx hashes, code hash | [`deploy/testnet.json`](deploy/testnet.json) |

Testnet stays on 2.0.1, the version the hackathon demo links were made with. Version 2.1.0 (rotating
nonce cleanup, two-step owner change, link count, versioned link records) changes the stored layout,
so it is for fresh deploys, starting with mainnet.

**Mainnet** (`id.bonuz-id.near`, owner `bonuz-id.near`, registrar `registrar.bonuz-id.near`, implicit
accounts only) is deployed with `node scripts/deploy.ts --network mainnet --yes-mainnet`, rehearsed first
on a local sandbox by `node scripts/rehearse-deploy.ts`. The owner wallet is made on the deploy machine
by `node scripts/owner-wallet.ts new` (12 words saved owner-only, never printed), funded from any
exchange or swap, and turned into `bonuz-id.near` by `node scripts/owner-wallet.ts claim --yes-mainnet`.

**Who can change the code.** Code upgrades (state kept) need a full-access key of `id.bonuz-id.near`.
There are two kinds: the deploy key (saved in `~/.near-credentials` on the deploy machine; back it up
offline) and the `bonuz-id.near` owner wallet's key, which the deploy script adds as a backup. The
`owner` in the contract can pause, change the registrar and hand ownership over; it cannot change the
code, so handing ownership over with `accept_owner` does not hand over upgrade power: rotate the
full-access keys of `id.bonuz-id.near` too.

## What was built during the hackathon, and what existed before

Only work committed after 19 Oct 2026 is new. The tag `pre-hackathon` marks the empty baseline of
this repo; everything here was written in the hackathon window
([diff](https://github.com/bonuz-market/bonuz-id-near/compare/pre-hackathon...main)).

**Built in the window (this repo, public):** the registry contract and its tests, `@bonuz/near-link`
and its shared vectors, the sandbox demo, the testnet deploy, and the mainnet deploy (`scripts/deploy.ts`,
`scripts/owner-wallet.ts`, `deploy/mainnet.json`).

**Built in the window (bonuz's private repos, using `@bonuz/near-link`):** the "Show on my bonuz ID"
flow and the NEP-413 message signer in the bonuz app; the `w_near` verify case, challenge route and
registry outbox in the bonuz backend, live in production on mainnet since 14 Nov 2026; the NEAR field
on bonuz.id profiles; `near_account` lookups in the bonuz MCP server's `get_identity` tool, so AI agents
can ask who a NEAR account belongs to.

**Existed before (baseline, not part of this entry):**
- the bonuz app, live in the App Store and Google Play;
- the bonuz ID and its contract on Base, which already had a `w_near` slot for a NEAR account;
- the app's NEAR wallet (accounts, balances, sending NEAR and NEP-141 tokens), merged on 14 Oct 2026
  and not yet released to everyone;
- the backend's verify flow for socials (the one verifying an X account), which the NEAR case joins.

**AI-written code:** all code in this repo was written with Claude (Anthropic), directed by the
bonuz team.

## Roadmap

- **Stage 2:** NEAR for every bonuz user in the wallet, and sign in with a NEAR account.
- **Stage 3:** AI agents, which already get bonuz IDs today, get a NEAR account too, so an agent has
  an identity and a wallet in one place.

## License

MIT, see [LICENSE](LICENSE).
