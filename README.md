# bonuz ID on NEAR

A NEAR smart contract that links a NEAR account to a bonuz ID. A bonuz ID is the public profile a
bonuz user has at `bonuz.id/<handle>`. The NEAR account links itself by calling `link("<handle>")`:
the protocol tells the contract which account signed, so no signature checks are needed. Anyone,
including AI agents, can then ask the registry "which bonuz ID is `alice.near`?" or "which NEAR
account is `alice`?". The same pair is stored the other way round on the bonuz ID contract on Base
(its `w_near` slot), so the two registries point at each other without a bridge.

Built for the NEAR HACKATHON (November 2026). All code in this repo was written with Claude
(Anthropic's AI) and reviewed by the bonuz team.

## Live on testnet

| | |
|---|---|
| Contract | `id.bonuz.testnet` |
| Owner (may remove squatted links, cannot create links) | `bonuz.testnet` |
| Explorer | https://testnet.nearblocks.io/address/id.bonuz.testnet |
| Deploy record | [`deploy/testnet.json`](deploy/testnet.json) |

Two real calls against the live contract (9 Nov 2026), after `bonuz.testnet` linked the handle `bonuz`:

```
$ near view id.bonuz.testnet bonuz_of '{"account_id":"bonuz.testnet"}' --networkId testnet
Function execution return value (printed to stdout):
"bonuz"

$ near view id.bonuz.testnet account_of '{"bonuz_id":"bonuz"}' --networkId testnet
Function execution return value (printed to stdout):
"bonuz.testnet"
```

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

Handles are 2 to 32 characters of `a-z`, `0-9` and `_`. Attach 0.0032 NEAR for storage; what is
not needed comes back in the same transaction.

```bash
near contract call-function as-transaction id.bonuz.testnet link json-args '{"bonuz_id":"yourname_demo"}' prepaid-gas '30.0 Tgas' attached-deposit '0.0032 NEAR' sign-as yourname-bonuz-demo.testnet network-config testnet sign-with-legacy-keychain send
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
near contract call-function as-transaction id.bonuz.testnet link json-args '{"bonuz_id":"bonuz"}' prepaid-gas '30.0 Tgas' attached-deposit '0.0032 NEAR' sign-as yourname-bonuz-demo.testnet network-config testnet sign-with-legacy-keychain send
```

You see `this bonuz ID is already linked to another NEAR account`. A handle like `Yourname` or `a`
fails with a message about the allowed characters or length.

### 6. Unlink and get the storage back

```bash
near contract call-function as-transaction id.bonuz.testnet unlink json-args '{}' prepaid-gas '30.0 Tgas' attached-deposit '1 yoctoNEAR' sign-as yourname-bonuz-demo.testnet network-config testnet sign-with-legacy-keychain send
```

`bonuz_of` now returns `null` and the storage deposit is back in your account.

## Methods

| Method | Kind | Who | What |
|---|---|---|---|
| `link(bonuz_id: String)` | change, payable | the NEAR account itself | Links the caller to `bonuz_id`. Replaces the caller's earlier link and frees the old handle. Refuses a handle linked to another account. Needs a deposit of at least the storage cost (`link_deposit()` always covers it, excess is refunded). Linking the same handle again changes nothing and refunds everything. |
| `unlink()` | change, 1 yoctoNEAR | the NEAR account itself | Removes the caller's link and refunds the storage. |
| `admin_unlink(bonuz_id: String)` | change, 1 yoctoNEAR | owner only | Removes a squatted link. The storage refund goes to the account that paid for it. |
| `bonuz_of(account_id: AccountId) -> Option<String>` | view | anyone | The handle linked to a NEAR account. |
| `account_of(bonuz_id: String) -> Option<AccountId>` | view | anyone | The NEAR account linked to a handle. |
| `link_deposit() -> NearToken` | view | anyone | A deposit that always covers one `link` (0.0032 NEAR). |
| `owner() -> AccountId` | view | anyone | The account allowed to call `admin_unlink`. |
| `new(owner_id: AccountId)` | init, private | the contract account | Run once, in the deploy transaction. |

Every change needs an attached deposit, so only a full-access key can change a link. A
function-call access key (for example one a dapp got at login) cannot.

### Events (NEP-297, standard `bonuz_id_near`, version `1.0.0`)

```
EVENT_JSON:{"standard":"bonuz_id_near","version":"1.0.0","event":"link","data":{"account_id":"alice.testnet","bonuz_id":"alice","previous_bonuz_id":"old_handle"}}
EVENT_JSON:{"standard":"bonuz_id_near","version":"1.0.0","event":"unlink","data":{"account_id":"alice.testnet","bonuz_id":"alice"}}
```

`previous_bonuz_id` appears only on a relink. `removed_by` appears on `unlink` only when the owner removed it.

### Trust model

A link in this registry proves who controls the NEAR account. It does not prove who controls the
bonuz ID: anyone can claim a free handle. bonuz treats a NEAR account as verified on a profile only
when both sides agree: this registry says account to handle, and the bonuz ID on Base says handle to
account. If someone squats a handle, the owner removes it with `admin_unlink`, and the real holder
links it. This is "a bonuz ID with real-world activity", not proof of personhood: bonuz does no
biometrics.

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

This runs 5 unit tests and 10 sandbox tests with `near-workspaces` (link, relink, same-handle no-op,
conflict refusal, unlink with storage refund, excess deposit refund and too-small deposit refused,
invalid handles refused, 1 yoctoNEAR rule, owner-only `admin_unlink`, private init). The sandbox
part takes about 2 minutes. Expected end:

```
test result: ok. 5 passed; 0 failed; ...
test result: ok. 10 passed; 0 failed; ...
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
