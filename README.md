# bonuz ID on NEAR

A NEAR smart contract that links a NEAR account to a bonuz ID. A bonuz ID is the public profile a
bonuz user has at `bonuz.id/<handle>`. Anyone, including AI agents, can then ask the registry
"which bonuz ID is this NEAR account?" or "which NEAR account is `alice`?". The same pair is stored
the other way round on the bonuz ID contract on Base (its `w_near` slot), so the two registries
point at each other without a bridge.

There are two ways to link:

- **One click from the bonuz app.** The NEAR wallet the bonuz app creates is a NEAR implicit
  account: its account id is the hex of its public key. The app signs one message with that key
  (NEP-413), bonuz submits it and pays the storage. The user needs no NEAR, pays no gas, and the
  account does not even have to exist on chain yet.
- **Any NEAR account, by itself.** An account like `alice.near` calls `link("alice")` and pays its
  own small storage deposit. The protocol tells the contract which account signed, so no extra
  signature is needed.

Built for the NEAR HACKATHON (November 2026). All code in this repo was written with Claude
(Anthropic's AI) and reviewed by the bonuz team.

## Live on testnet

| | |
|---|---|
| Contract | `id.bonuz.testnet` (version 1.1.0) |
| Owner (may remove squatted links and set the sponsor, cannot create links) | `bonuz.testnet` |
| Sponsor (submits one-click links and pays their storage) | `sponsor.bonuz.testnet` |
| Explorer | https://testnet.nearblocks.io/address/id.bonuz.testnet |
| Deploy record | [`deploy/testnet.json`](deploy/testnet.json) |

Real calls against the live contract (9 Nov 2026). `bonuz.testnet` linked itself to `bonuz`. A
fresh app-style wallet that was never funded was linked to `bonuz_demo` in one click:

```
$ near view id.bonuz.testnet bonuz_of '{"account_id":"bonuz.testnet"}' --networkId testnet
Function execution return value (printed to stdout):
"bonuz"

$ near view id.bonuz.testnet account_of '{"bonuz_id":"bonuz"}' --networkId testnet
Function execution return value (printed to stdout):
"bonuz.testnet"

$ near view id.bonuz.testnet account_of '{"bonuz_id":"bonuz_demo"}' --networkId testnet
Function execution return value (printed to stdout):
"1fc703743735f7d47f60bea7d0f916a11fc886bb1404fad8689eb09cc2d29928"
```

That last account has no balance and does not exist on chain. It never needed NEAR to be linked.

## Try it on testnet (about 10 minutes)

### 0. Check it with nothing installed

Ask the public testnet RPC which bonuz ID `bonuz.testnet` has:

```bash
curl -s https://rpc.testnet.near.org -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"query","params":{"request_type":"call_function","finality":"final","account_id":"id.bonuz.testnet","method_name":"bonuz_of","args_base64":"eyJhY2NvdW50X2lkIjoiYm9udXoudGVzdG5ldCJ9"}}'
```

You see `"result":[34,98,111,110,117,122,34]`, which is the bytes of `"bonuz"`.
(`args_base64` is `{"account_id":"bonuz.testnet"}` in base64.)

### 1. Install the NEAR CLI

```bash
npm install -g near-cli-rs@0.31.1
```

Or, with Rust: `cargo install near-cli-rs --locked`. Check with `near --version`.

### 2. Get a free testnet account

Pick a name nobody has, for example `yourname-bonuz-demo.testnet`. The faucet funds it with 10 test NEAR.
The key is saved in `~/.near-credentials`.

```bash
near account create-account sponsor-by-faucet-service yourname-bonuz-demo.testnet autogenerate-new-keypair save-to-legacy-keychain network-config testnet create
```

### 3. Link it to a handle

Handles are 2 to 32 characters of `a-z`, `0-9` and `_`. Attach 0.006 NEAR for storage; what is not
needed comes back in the same transaction.

```bash
near contract call-function as-transaction id.bonuz.testnet link json-args '{"bonuz_id":"yourname_demo"}' prepaid-gas '30.0 Tgas' attached-deposit '0.006 NEAR' sign-as yourname-bonuz-demo.testnet network-config testnet sign-with-legacy-keychain send
```

You see the event in the logs:

```
EVENT_JSON:{"standard":"bonuz_id_near","version":"1.0.0","event":"link","data":{"account_id":"yourname-bonuz-demo.testnet","bonuz_id":"yourname_demo"}}
```

### 4. Look it up both ways

```bash
near view id.bonuz.testnet bonuz_of '{"account_id":"yourname-bonuz-demo.testnet"}' --networkId testnet
```

```bash
near view id.bonuz.testnet account_of '{"bonuz_id":"yourname_demo"}' --networkId testnet
```

You see `"yourname_demo"` and `"yourname-bonuz-demo.testnet"`.

### 5. See what the contract refuses

Try to take the demo handle that `bonuz.testnet` already holds:

```bash
near contract call-function as-transaction id.bonuz.testnet link json-args '{"bonuz_id":"bonuz"}' prepaid-gas '30.0 Tgas' attached-deposit '0.006 NEAR' sign-as yourname-bonuz-demo.testnet network-config testnet sign-with-legacy-keychain send
```

You see `this bonuz ID is already linked to another NEAR account`. A handle like `Yourname` or `a`
fails with a message about the allowed characters or length.

### 6. Unlink and get the storage back

```bash
near contract call-function as-transaction id.bonuz.testnet unlink json-args '{}' prepaid-gas '30.0 Tgas' attached-deposit '1 yoctoNEAR' sign-as yourname-bonuz-demo.testnet network-config testnet sign-with-legacy-keychain send
```

`bonuz_of` now returns `null` and the storage deposit is back in your account.

### 7. The one-click path, the way the app signs it

Only the bonuz sponsor can submit a one-click link, so on testnet you can make and check the
signature yourself. The repo has a small tool that acts like the app wallet (needs Rust, see
"Build and test"):

```bash
cd contracts/registry && cargo run -q --example app_wallet -- new /tmp/demo-wallet.key
```

It prints a new implicit account id, a key that exists only on your machine. Ask the contract for
the exact text to sign, then sign it:

```bash
near view id.bonuz.testnet signed_link_message '{"account_id":"<printed id>","bonuz_id":"yourname_app"}' --networkId testnet
```

```bash
cargo run -q --example app_wallet -- sign /tmp/demo-wallet.key "Link this NEAR account to bonuz ID yourname_app (#0)" id.bonuz.testnet
```

The output is the `account_id`, `nonce` and `signature` the sponsor passes to `link_signed`. Calling
`link_signed` from your own account shows the guard: `only the bonuz sponsor account can submit
signed calls`. The full flow is covered by the sandbox tests.

## Methods

| Method | Kind | Who | What |
|---|---|---|---|
| `link(bonuz_id: String)` | change, payable | the NEAR account itself | Links the caller to `bonuz_id`. Replaces the caller's earlier link and frees the old handle. Refuses a handle linked to another account. Needs a deposit of at least the storage cost (`link_deposit()` always covers it, excess is refunded). Linking the same handle again changes nothing and refunds everything. |
| `link_signed(account_id, bonuz_id, nonce, signature)` | change, payable | sponsor only | One-click link for an implicit account. `signature` is a NEP-413 signature by that account's key over `signed_link_message(account_id, bonuz_id)`, recipient `id.bonuz.testnet`, no callback URL. `nonce` and `signature` are base64. The sponsor pays the storage. A handle held by another account is taken over, since bonuz vouches for the handle. |
| `unlink()` | change, 1 yoctoNEAR | the NEAR account itself | Removes the caller's link and refunds the storage to whoever paid it. |
| `unlink_signed(account_id, nonce, signature)` | change, payable | sponsor only | Same as `unlink` for an implicit account, signed over `signed_unlink_message(account_id)`. |
| `admin_unlink(bonuz_id: String)` | change, 1 yoctoNEAR | owner only | Removes a squatted link. The storage refund goes to whoever paid it. |
| `set_sponsor(sponsor_id: Option<AccountId>)` | change, 1 yoctoNEAR | owner only | Sets or clears the sponsor. |
| `bonuz_of(account_id) -> Option<String>` | view | anyone | The handle linked to a NEAR account. |
| `account_of(bonuz_id) -> Option<AccountId>` | view | anyone | The NEAR account linked to a handle. |
| `signed_link_message(account_id, bonuz_id) -> String` | view | anyone | The exact text to sign for `link_signed` right now. |
| `signed_unlink_message(account_id) -> String` | view | anyone | The exact text to sign for `unlink_signed` right now. |
| `payer_of(account_id) -> Option<AccountId>` | view | anyone | Who paid the storage of a link: the account or the sponsor. |
| `link_deposit() -> NearToken` | view | anyone | A deposit that always covers one link (0.006 NEAR). |
| `owner()`, `sponsor()` | view | anyone | The owner and sponsor accounts. |
| `new(owner_id, sponsor_id)` | init, private | the contract account | Run once, in the deploy transaction. |
| `migrate(sponsor_id)` | init, private | the contract account | Upgrades version 1.0.0 state, in the deploy transaction. |

Every change needs an attached deposit, so only a full-access key can change a link. A
function-call access key (for example one a dapp got at login) cannot.

Each signed message ends in a counter (`#0`, `#1`, ...) that goes up with every signed call, so a
signature works only once.

### Events (NEP-297, standard `bonuz_id_near`, version `1.0.0`)

```
EVENT_JSON:{"standard":"bonuz_id_near","version":"1.0.0","event":"link","data":{"account_id":"alice.testnet","bonuz_id":"alice","previous_bonuz_id":"old_handle"}}
EVENT_JSON:{"standard":"bonuz_id_near","version":"1.0.0","event":"link","data":{"account_id":"1fc7...9928","bonuz_id":"bonuz_demo","sponsored_by":"sponsor.bonuz.testnet"}}
EVENT_JSON:{"standard":"bonuz_id_near","version":"1.0.0","event":"unlink","data":{"account_id":"alice.testnet","bonuz_id":"alice"}}
```

`previous_bonuz_id` appears only on a relink, `sponsored_by` only on a one-click link. `removed_by`
appears on `unlink` only when someone other than the account removed it.

### Trust model

- A **self-made link** proves who controls the NEAR account. It does not prove who controls the
  bonuz ID: anyone can claim a free handle.
- A **one-click link** is submitted by the bonuz sponsor only after the user is signed in to that
  bonuz ID in the app, so it also carries bonuz's word that the handle is theirs. It replaces a
  squatter.
- bonuz shows a NEAR account as verified on a profile only when both sides agree: this registry says
  account to handle, and the bonuz ID on Base says handle to account.
- The owner can remove a squatted link with `admin_unlink`. The contract account keeps its own key,
  so bonuz can upgrade it (testnet).
- This is "a bonuz ID with real-world activity", not proof of personhood: bonuz does no biometrics.

## Build and test

Needs Rust (stable, tested on 1.95), the `wasm32-unknown-unknown` target and `cargo-near`
0.23. The sandbox tests download a local NEAR node on the first run.

```bash
rustup target add wasm32-unknown-unknown
```

```bash
cargo install cargo-near --locked
```

```bash
cd contracts/registry && cargo test
```

This runs 7 unit tests and 16 sandbox tests with `near-workspaces`. They cover link, relink,
same-handle no-op, conflict refusal, unlink with storage refund, excess deposit refund and too-small
deposit refused, invalid handles refused, the 1 yoctoNEAR rule, owner-only `admin_unlink` and
`set_sponsor`, private init, and the one-click path: an unfunded app wallet linked by the sponsor,
replayed signatures refused, wrong key, changed handle, named accounts and non-sponsor callers
refused, a squatter replaced, and a signed unlink that refunds the sponsor. Expected end:

```
test result: ok. 7 passed; 0 failed; ...
test result: ok. 16 passed; 0 failed; ...
```

Build the wasm that is deployed (sha256 in `deploy/testnet.json`):

```bash
cd contracts/registry && cargo near build non-reproducible-wasm
```

## What existed before the hackathon

Code from before 19 Oct 2026 is baseline, it lives in other (private) repos and is not part of this entry:

- **The bonuz app** (iOS and Android), with bonuz ID profiles and real-world check-ins.
- **bonuz ID on Base**: the profile contract already has a `w_near` slot for a NEAR account, unused until now.
- **A NEAR wallet inside the bonuz app**: accounts, balances, sending NEAR and tokens. Merged on 14 Oct 2026, not released yet (behind a switch that is off in the store build).

**Everything in this repo is new.** The repo starts with an empty commit tagged
[`pre-hackathon`](https://github.com/bonuz-market/bonuz-id-near/releases/tag/pre-hackathon), so the
judged diff is everything after it. All code here was written with Claude.

## Roadmap

1. **bonuz ID on NEAR** (this hackathon): a NEAR account becomes one more identifier on a bonuz ID, next to socials, ENS and EVM wallets, verified, shown on the profile, readable by agents.
2. **NEAR in the bonuz wallet**: already built, switched on for the demo; later sign in with a NEAR account and simple named-account and key tools.
3. **Agents**: AI agents already get bonuz IDs; give them a NEAR account too, then list agentic NEAR apps in bonuz discovery. Research first.

## License

MIT, see [LICENSE](LICENSE).
