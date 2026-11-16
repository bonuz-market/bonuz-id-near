# Registry 2.2.0: links keyed to the creation wallet, named accounts confirmed on chain

Status: LIVE on mainnet since 16 Nov 2026 (Mende moved the date up). `id.bonuz-id.near`
was upgraded from 2.1.0 with 0 links (nothing to backfill) in one transaction,
`43VJCPMaoRHeTDwTiP7NmUg1C1LzVWYFPYYArDLA9xa8` (deploy + migrate); new registrar
function-call key `AkAvp2aqrDs12NjMYDJqyqPvPSyw4xi2kwbs1T3uDJZh`. Before it: the
mainnet-fork rehearsal passed 26 of 26 the same afternoon, and BonuzAdmin v2.0.217 (which
picks the message form by `status().version`) was live in production. The new key is in
Key Vault; the old 2.1.0 function-call key is deleted once a production link shows the
new key in use (step 6).

## Two changes

1. **A link belongs to the bonuz user, not to the handle.** 2.1.0 keys every link on the
   handle string (`bonuz_id`). Handles can change, and an old handle can later be taken by
   someone else. In 2.1.0 the new holder of `@mende` would resolve to whatever NEAR account
   the old holder linked, unless bonuz unlinked it in time (BonuzAdmin drops the NEAR
   verification on every rename for exactly this reason). 2.2.0 keys each link to the
   user's **creation wallet** (an EVM address that never changes) and keeps the handle only
   as a display label that points at that wallet.
2. **Named accounts (`mende.near`) without trusting bonuz.** The registrar writes a
   *pending* claim "creation wallet W wants NEAR account A". The link only goes live when A
   itself sends `accept_link(W)`. bonuz never holds A's key, so it cannot make the link
   alone. The implicit-account path (NEP-413 signature checked by the contract) stays, and
   its signed message now names W too (a fourth line, see below), so on both paths the
   NEAR account consents to the wallet itself, never to a handle.

## Which stable id

Options looked at in BonuzAdmin (`src/collections/Users/Users.ts`):

| id | stable? | already public? | verdict |
|---|---|---|---|
| `users.walletAddress` (the EVM EOA made for the user at sign-up, the login wallet) | Yes. `unique`, `required`, never reassigned. The only write after creation is the EIP-55 case normalisation in `auth-v2/auth.controller.ts`, which does not change the address. | Yes. The Base `BonuzSocialId` contract keys the whole public profile on it (`setUserHandle`, `setSocialLinks(..., address _user)`, `getUserProfileAndSocialLinks(address)`), so EOA -> handle is already public on Base. | **chosen** |
| `users.id` (database row id) | Yes | No | Means nothing outside bonuz's database; anyone reading the registry could not check it against anything. Leaks user counts. |
| `smartAccountAddress` | Legacy, admin-only, "never proven" | Partly | No. |
| the handle | No, it is what changes | Yes | The problem being fixed. |

So the stable id is `bonuz_eoa`: the user's creation EOA, written as `0x` plus 40
**lowercase** hex characters. The contract refuses any other form (no checksum casing), so
one wallet has exactly one spelling on chain.

## Privacy

Putting the EOA on NEAR adds nothing new: the Base Social ID contract already publishes
EOA -> handle -> social links (including `w_near`). What 2.2.0 adds is the same pair on
a second chain. Two things to know:

- The NEAR link is now readable by wallet as well as by handle (`account_of_eoa`). Someone
  holding only the EOA (for example from a Base transaction) can find the NEAR account. On
  Base they can already read the same `w_near` value from the profile.
- Account deletion: BonuzAdmin clears `setSocialLinks` on Base but not the handle. On NEAR
  the backend should call `unlink_by_registrar` (already does) and `set_handle(W, null)`
  (new), so the registry stops naming the handle. The EOA stays in old transactions and
  events forever, as on Base.

## Data model

| map | key -> value | notes |
|---|---|---|
| `links` (2.1.0 prefix, kept) | NEAR account -> `VersionedLink` | `V1` = 2.1.0 link naming a handle. `V2` = 2.2.0 link naming `bonuz_eoa`. |
| `account_of` (2.1.0 prefix, kept) | handle -> NEAR account | Only for `V1` links. Empties as they are adopted or removed. |
| `account_of_eoa` (new) | EOA -> NEAR account | One live NEAR account per user. |
| `handle_of_eoa` / `eoa_of_handle` (new) | EOA <-> handle | The display label. One handle per EOA, one EOA per handle. |
| `proposals` (new) | NEAR account -> pending claim | `{bonuz_eoa, proposed_at, expires}`. |
| `proposal_of_eoa` (new) | EOA -> NEAR account | One pending claim per user. |

Rules the contract keeps true:

- One NEAR account has at most one live link, and at most one pending claim.
- One EOA has at most one live NEAR account and at most one pending claim.
- A handle is either still named by a 2.1.0 link **or** a label of an EOA, never both.
  `set_handle` refuses a handle a 2.1.0 link still holds.
- While any 2.1.0 link waits for adoption (`legacy_links > 0`), `link` and `propose_link`
  refuse. A 2.1.0 link is keyed by handle, so the contract cannot see which wallet it
  belongs to; without this gate that wallet could get a second live account.
- `link` and `propose_link` never move a handle away from another wallet that has a live
  link (2.1.0 refused that case too, "unlink it first"). Moving it takes an explicit
  `set_handle` on the old wallet first. `propose_link` may move a label of a wallet without
  a live link (the claim still needs the account's `accept_link`). `link` never moves a label
  off another wallet at all: its signed message can be up to 10 minutes old, so a rename or
  retake bonuz wrote since then wins ("labels another bonuz wallet; set_handle first").

## The link message names the wallet

From 2.2.0 the NEP-413 message for `link` (the implicit-account path) has a fourth line:

```text
Link NEAR account <account_id> to bonuz ID @<handle>
Network: <testnet|mainnet>
Expires: <unix seconds>
Wallet: <bonuz_eoa>
```

- `<bonuz_eoa>` is the user's creation wallet in the one spelling the registry stores: `0x`
  plus 40 **lowercase** hex characters. Builders lowercase `users.walletAddress` (BonuzAdmin
  stores it EIP-55 cased). The parser refuses any other spelling (`0X`, checksum casing,
  missing space, wrong length, a trailing newline), so one wallet has exactly one signable
  text.
- The contract links the account to the wallet on line 4 and to nothing else. The optional
  `bonuz_eoa` argument must equal it ("the signed message names a different bonuz wallet").
  The wallet is no longer looked up from the handle.
- Why: with the three-line message the account consented to a handle, and the contract took
  the wallet from the registrar's `bonuz_eoa` argument, or, without it, from whatever wallet
  the handle labelled when the call landed. A registrar that passed another wallet B, or a
  rename plus retake of the handle inside the 10-minute window, put the link on a wallet that
  never consented, and could move the handle's label off an unlinked wallet on the way. That
  broke the point of 2.2.0 (links keyed to the creation wallet, never the username).
- **Three-line (2.1.0) messages are refused after the upgrade** ("the signed message must
  name the bonuz wallet"). The alternative, accepting them when the handle labels the
  `bonuz_eoa` argument, keeps the consent on the handle: the registrar writes labels
  (`set_handle`) and so could point any handle at any wallet right before the call. Refusing
  costs little: a message lives at most 10 minutes, `link` refuses anyway until the backfill
  is done, and a refused call stores nothing (the user signs a fresh challenge).
- Registry 2.1.0 refuses the four-line message ("exactly three lines"; pinned by the sandbox
  upgrade test), so the backend picks the form by `status().version`: three lines while it
  says `2.1.x`, four from `2.2.0`.
- `packages/near-link` builds and parses both forms (`buildLinkMessage({ ..., bonuzEoa })`,
  `checkLink(...).bonuzEoa`), and the shared vectors cover the fourth line.
- `propose_link` / `accept_link` do not use this message: `accept_link(bonuz_eoa)` already
  names the wallet in the account's own transaction.

## Renames and handle reuse

- **Rename** `@old` -> `@new`: the registrar calls `set_handle(W, "new")`. The link stays,
  because it is keyed on W. `bonuz_of(A)` now answers `new`; `account_of("old")` answers
  nothing.
- **Reuse**: a later user V takes `@old`. The registrar calls `set_handle(V, "old")`. If
  `@old` still labelled W (bonuz missed the rename), the label moves to V and W is left
  without a label. `account_of("old")` resolves `old -> V -> V's NEAR account` (none until V
  links one). V inherits nothing from W.
- The handle side is bonuz's word, as in 2.1.0: only bonuz knows who holds a handle. The
  NEAR side is never bonuz's word: the account signs (implicit path) or sends `accept_link`
  (named path).

## Methods

New or changed (registrar = the bonuz registrar account; FC = its function-call key):

```text
link(account_id, bonuz_id, public_key, nonce, signature, message, bonuz_eoa?: String) -> U64
    registrar. NEP-413 check as before, and the message must have the fourth line
    "Wallet: <bonuz_eoa>" (three-line messages are refused). Links account -> that wallet
    and labels bonuz_id -> that wallet. bonuz_eoa, if passed, must equal the signed wallet.
    Refused while 2.1.0 links wait for adoption, when the wallet is linked to another
    account, and when the handle labels another wallet (linked or not).
propose_link(account_id, bonuz_eoa, bonuz_id) -> U64 (expires, unix seconds)
    registrar. Labels bonuz_id -> bonuz_eoa and writes a pending claim valid 7 days.
    Same account + same wallet again only refreshes the expiry (safe retry). Refused when
    the account or the wallet already has another pending claim, the wallet is linked
    to another account, the handle labels another linked wallet, or 2.1.0 links wait
    for adoption.
cancel_link_proposal(account_id) -> bool          registrar. False when there is none.
accept_link(bonuz_eoa)                            the claimed account itself, 1 yoctoNEAR.
reject_link()                                     the claimed account itself, 1 yoctoNEAR.
set_handle(bonuz_eoa, bonuz_id?: String)          registrar. Sets, moves or clears a label.
    Refused under the reserve, like propose_link.
adopt_legacy_link(account_id, bonuz_eoa)          registrar. Turns a 2.1.0 link into a
    2.2.0 one, keeping its handle as the label. Migration backfill. Works while paused;
    refused under the reserve.
drop_legacy_link(account_id)                      owner, 1 yoctoNEAR. Removes a 2.1.0 link
    that cannot be adopted (e.g. a second 2.1.0 link of a wallet already adopted). Only
    2.1.0 links. Works while paused. Emits `unlink` with reason `legacy`.
prune_link_proposals(limit) -> u32                anyone. Deletes up to 50 expired claims.
migrate()                                         private (the contract account), once.
```

New views: `account_of_eoa(bonuz_eoa)`, `eoa_of(bonuz_id)`,
`handle_of(bonuz_eoa)`, `link_proposal_of(account_id)`, `link_proposal_for(bonuz_eoa)`,
`link_proposals(from_index, limit)`. Expired claims read as none.

Unchanged: `unlink_by_registrar`, `unlink_by_owner`, `prune_nonces`, every owner method.
`bonuz_of(account)` answers the current handle label. `account_of(handle)` resolves
`handle -> EOA -> account`, falling back to the 2.1.0 map for a not yet adopted link.
`link_of` keeps its fields (`bonuz_id` is the current label, `""` when the wallet has
none) and adds `bonuz_eoa` (null for a 2.1.0 link). `status()` adds
`stored_link_proposals`, `max_link_proposals`, `link_proposal_ttl_seconds` and
`legacy_links`.

Accepted links record `checked_by: "account"` and the public key that signed the
`accept_link` transaction, but only when the account signed it itself (signer =
predecessor). When the account accepts through a contract call, the signing key belongs to
someone else, so `public_key` is null in `link_of` and in the `link` event.

Events (standard `bonuz_id_near`): `link` and `unlink` move to version 2.2.0 and add
`bonuz_eoa` (`unlink` gains the reason `legacy`, from `drop_legacy_link`); new `link_proposed`, `link_proposal_closed` (reason `accepted`, `rejected`,
`cancelled`, `expired`, `replaced`) and `handle` (label set, moved or cleared).

## Griefing and front-running

- **Who can write a claim**: only the registrar. Nobody else can fill storage with claims.
- **Who pays**: the contract's own balance, like links. `propose_link` refuses under the
  reserve, as `link` does.
- **Caps**: at most 200 pending claims at once, one per NEAR account, one per user. A
  claim (with its wallet index and label) measured +501 bytes on the sandbox, about 0.005
  NEAR, so a full queue locks about 1 NEAR of the registry's balance. Every `propose_link` also deletes up to 3
  expired claims, and anyone can call `prune_link_proposals`.
- **Expiry**: 7 days. An expired claim cannot be accepted and reads as none.
- **A leaked registrar key** can fill the 200 slots for up to 7 days (owner pauses,
  rotates the registrar, and the registrar cancels), and can label handles. It still
  cannot make a live link to a named account it does not hold.
- **Swapping a claim under the user**: `accept_link` names the wallet it accepts. If the
  registrar cancelled and re-proposed to another wallet between the user's tap and the
  block, the accept fails instead of linking to the wrong user.
- **Someone else accepting**: `accept_link` only acts on the claim of the predecessor
  account. A third party has no claim to accept.
- **1 yoctoNEAR on accept and reject**: only a full-access key (the user's wallet) can
  attach a deposit, so a function-call key some dapp added can neither accept nor turn
  down a claim on the user's behalf.
- **The wallet got linked meanwhile**: `accept_link` re-checks. If the wallet is linked to
  another NEAR account by then, the accept fails ("unlink it first"), the same rule `link`
  has for a taken handle. If the accepting account was linked to another wallet, that old
  link is replaced: the account's own accept is the consent, as with `link`.
- **Paused**: `propose_link`, `accept_link`, `link`, `set_handle` and
  `unlink_by_registrar` refuse while paused. `reject_link`, `cancel_link_proposal`,
  `unlink_by_owner`, `adopt_legacy_link` and the owner's `drop_legacy_link` still work, so
  the backfill can run, and finish, behind a pause.
- **A deleted and re-created named account inherits its link and its open claim.** NEAR
  lets `alice.near` be deleted and later created again by whoever controls its parent
  (`near` for a top-level name, through the registrar there). The registry keys links and
  claims on the account id and never learns about the deletion, so the new holder of
  `alice.near` is shown as linked to the old user, and can `accept_link` a claim that is
  still open (within its 7 days). 2.1.0 has the same property for its links. Mitigation is
  off chain: BonuzAdmin's hourly `check.ts` should re-check every `checked_by: "account"`
  link (and every open claim) over RPC: the account still exists (`view_account`), and, if
  it reports one, its creation block is not later than `linked_at_block` / `proposed_at`.
  When either fails, `unlink_by_registrar` / `cancel_link_proposal`. Implicit accounts are
  not affected in practice (the id is the key; a re-created implicit account has the same
  key holder). There is no `check.ts` in this repo, so this is listed under the BonuzAdmin
  changes below.
- **Label spam**: `set_handle` has no cap, like `link`, but it is registrar-only and refuses
  under the reserve, so a leaked key cannot drain the balance below it.

## Migration of 2.1.0 state

The root struct gains fields, so the stored layout changes. `migrate()` reads the 2.1.0
struct and writes the 2.2.0 one with the new maps empty; every 2.1.0 map keeps its
storage prefix, so no link is rewritten. The upgrade is **one transaction**:
`DeployContract(2.2.0) + FunctionCall(migrate)`. If `migrate` fails, the deploy is undone
too, so the registry is never left with code that cannot read its state. A second
`migrate` call fails (the state no longer parses as 2.1.0).

After the upgrade, 2.1.0 links still read correctly (`bonuz_of`, `account_of`, `link_of`).
BonuzAdmin then calls `adopt_legacy_link(account, W)` for each, with W = the creation
wallet of the user whose outbox row made the link (not whoever holds the handle now).
`status().legacy_links` counts down to 0. Until it is 0, `link` and `propose_link`
refuse (see the rules above), so the backfill comes right after the upgrade, if needed
behind a pause (adoption works while paused). Mainnet has very few links (it went live
14 Nov; 0 at the time of the review), testnet a handful.

A 2.1.0 link that cannot be adopted would otherwise stall the backfill: `adopt_legacy_link`
refuses a second link for a wallet that is linked already (two 2.1.0 links of one user, e.g.
under an old and a new handle), and `unlink_by_registrar` refuses while paused. The owner
removes such a link with `drop_legacy_link(account)` (1 yoctoNEAR, works while paused, 2.1.0
links only); `legacy_links` and `links` count down and an `unlink` event with reason
`legacy` is emitted. Which of the two to keep is bonuz's call (normally the newer one).

## What this does not do

- It does not prove who holds a handle. Only bonuz knows that.
- It does not move existing `allow_registrar_checked` behaviour: mainnet stays `false`.
  Named accounts on mainnet go through `propose_link` + `accept_link` only. There is
  deliberately no `set_allow_registrar_checked`.
- It does not keep the 2.1.0 signing text: the `link` message gains the `Wallet:` line (see
  "The link message names the wallet"), so the app's signing gate, BonuzAdmin's challenge
  builder and `packages/near-link` change with it.
- It does not notice a deleted and re-created named account (see "Griefing and
  front-running").

## Testnet rehearsal (15 Nov 2026)

`node scripts/rehearse-2.2.0-testnet.ts --funder bonuz-readme-check.testnet` on fresh accounts
`r22.bonuz-readme-check.testnet` (registry), `r22-registrar...` and `r22-alice...` (a named
account): 2.1.0 deployed from the frozen wasm, three 2.1.0 links written (two implicit, one
registrar-checked named), upgraded with deploy + migrate in one transaction, old links read
back, a new registrar key, adopt, rename, a 2.2.0 implicit link, then propose -> three refused
accepts -> `accept_link` by the account -> views -> `unlink_by_owner`. Every hash is in
[`deploy/testnet-2.2.0-rehearsal.json`](../deploy/testnet-2.2.0-rehearsal.json).
`id.bonuz.testnet` (the hackathon testnet registry) was not touched.

That run used the build before the review fixes (wasm sha256 `c32f60ac...`). The fixes (the
backfill gate, no silent handle moves, reserve checks, the accept key, 1 yoctoNEAR on reject,
the deploy guards, and from the second review the wallet line in the link message, the
stricter label rule in `link` and `drop_legacy_link`) are covered by unit tests, the sandbox upgrade test from the frozen 2.1.0
wasm, and `node scripts/rehearse-deploy.ts`. The testnet run was not repeated because the
funder is low. Note that the `r22` testnet registry stores `LinkV2.public_key` as a plain
string, while the fixed build stores it as optional, so `r22` must not be upgraded in place to
the fixed build. It is a throwaway account.

## Mainnet upgrade: run 16 Nov 2026 (steps 1 to 5 and the Key Vault half of step 6; see Status above)

Originally planned for not before 26 Nov 2026; Mende moved it up.

1. BonuzAdmin and the app first, both before the contract upgrade:
   - BonuzAdmin: a release that sends `bonuz_eoa` on every `link` call (2.1.0 ignores the
     extra argument, pinned by the sandbox upgrade test) and builds the challenge message by
     `status().version`: three lines for `2.1.x`, four (with `Wallet:`) from `2.2.0`.
   - The app: a release whose signing gate accepts the four-line message and checks its
     wallet line against the signed-in user's own creation wallet. App versions without it
     refuse to sign 2.2.0 challenges, so implicit linking breaks on them after the upgrade;
     check the share of users on the new version before step 4.
2. On the deploy machine, in this repo at the merged commit:
   `cd contracts/registry && cargo test` and `node scripts/rehearse-deploy.ts` (sandbox, both
   pass), and check `contracts/registry/res/bonuz_id_registry.wasm` is the reviewed one
   (`shasum -a 256`; the PR records its hash).
   Then `node scripts/rehearse-mainnet-fork.ts`: it reads `id.bonuz-id.near`'s live code, state
   and keys (read only), loads them into a local sandbox and runs the exact step 4 command
   against it, with throwaway keys and a preload that blocks every request not to 127.0.0.1.
   Then the step 5 checks, the backfill count, a 2.2.0 link, refusals and the pause path. Run it
   the same day as step 4, so it sees the state the upgrade will meet. First run, 16 Nov 2026:
   26 of 26 pass, 0 links on mainnet (nothing to backfill), 3.52 NEAR free after the upgrade.
3. Read only: `status()` on `id.bonuz-id.near` says `2.1.0`, and note `links`.
4. `node scripts/deploy.ts --network mainnet --yes-mainnet --upgrade-code --new-registrar-key`
   (`--upgrade-code` without `--new-registrar-key` is refused before any network call)
   - before sending, the script checks the registry's free balance after the upgrade (the
     bigger wasm locks about 0.55 NEAR more) stays at or above the 1 NEAR reserve, and
     stops otherwise: top up `id.bonuz-id.near` first;
   - one transaction `DeployContract + migrate()` signed by `id.bonuz-id.near`'s deploy key
     (step `upgrade_code_and_migrate_2_1_to_2_2`); a failed migrate undoes the deploy;
   - a new function-call key on `registrar.bonuz-id.near` with the 2.2.0 registrar methods,
     saved to `~/.near-credentials/mainnet/registrar-fc/registrar.bonuz-id.near.json`; the old
     file becomes `registrar.bonuz-id.near.1.json` and the old key stays on chain;
   - all 13 post-deploy checks, and `deploy/mainnet.json` is updated. Commit it.
5. Check: `status()` says `2.2.0`, `links` unchanged, `legacy_links` = `links`;
   `bonuz_of` / `link_of` of every known link unchanged.
6. Put the new function-call key into the production Key Vault secret
   `NEAR-REGISTRAR-PRIVATE-KEY` (`az keyvault secret set ... --file <file holding only the
   key>`), restart the app, then delete the old function-call key from
   `registrar.bonuz-id.near` with its full-access key.
7. Backfill: for each 2.1.0 link, `adopt_legacy_link(account, W)` with W = the creation wallet
   of the user whose outbox row made the link. Done when `legacy_links` is 0. New links
   and claims wait until then. If there are any 2.1.0 links, pause first
   (`set_paused(true)`), adopt them all, then unpause. A link that cannot be adopted (a
   second one for a wallet already adopted) is removed by the owner with
   `drop_legacy_link(account)` (works while paused). Read-only pre-count before step 4: list
   the 2.1.0 links (`link_of` of every account in the outbox) and group them by creation
   wallet; any wallet with two is a `drop_legacy_link` to plan.
8. Named accounts go live when BonuzAdmin and the app use `propose_link` / `accept_link`.
   `allow_registrar_checked` stays `false` on mainnet.

Rollback: 2.1.0 code cannot read 2.2.0 state, so there is no going back by redeploying
2.1.0. If something is wrong after step 4, the owner pauses (`set_paused(true)`, links and
claims stop, owner unlinks and rejects still work) and a 2.2.x fix is deployed with
`--upgrade-code` (same layout, plain code deploy).

## What BonuzAdmin and the app need to change

BonuzAdmin (`src/services/nearRegistry/`):

- Challenge builder (the verify route that issues the NEP-413 message): from registry 2.2.0
  add the fourth line `Wallet: <walletAddress lowercased>` (via the re-copied
  `buildLinkMessage({ ..., bonuzEoa })`), three lines while `status().version` is `2.1.x`.
  The pre-check (`checkLink`) then returns `bonuzEoa`; refuse when it is not the user's own
  wallet. Map the new refusals `must name the bonuz wallet`, `names a different bonuz
  wallet` and `labels another bonuz wallet; set_handle first` (the last one: call
  `set_handle` for the right wallet, then resend the same signed message if it has not
  expired).
- `registrar.ts`: send `bonuz_eoa` (the user's `walletAddress`, lowercased) in `link`; new
  `proposeLink`, `cancelLinkProposal`, `setHandle`, `adoptLegacyLink` and the new views;
  map the new refusals (`pending link proposal`, `too many pending link proposals`,
  `2.1.0 link still names`, `not all adopted yet`, `pass bonuz_eoa`); treat
  `link_of(...).public_key` as nullable.
- `nearEdit.ts onHandleChanged`: stop dropping the NEAR verification on a rename; call
  `set_handle(W, newHandle)` instead. Account deletion: `unlink_by_registrar` (as today) plus
  `set_handle(W, null)`.
- `namedAccounts.ts` / `config.ts`: on mainnet, named accounts are allowed when the
  registry's `status().version` is 2.2 or later (propose + accept), not only when
  `allow_registrar_checked` is true. `IMPLICIT_ONLY_REFUSAL` goes away for 2.2.0.
- Verify flow for a named account: after the NEP-413 check and the RPC key check, queue a
  `propose` outbox op instead of `link`; the profile shows the account as verified only
  once the chain shows `link_of(account).checked_by == "account"` with this user's wallet
  (the app or the hourly check reads it). New outbox op kinds `propose` and
  `cancel_proposal`, and a state for "waiting for the account to accept".
- `check.ts`: compare `link_of(...).bonuz_eoa` with the user's wallet, not
  `link.bonuz_id` with the handle (a rename is no longer a mismatch); skip the key re-check
  (`namedKeyCheck.ts`) for `checked_by: "account"` links, whose consent is on chain, but
  re-check that the named account still exists and was not re-created after the link or
  the open claim (see "A deleted and re-created named account"), read-only RPC.
- One-off backfill job for step 7 above, with the pre-count and `drop_legacy_link` for
  links that cannot be adopted.
- Registrar key rotation (step 6) and the docs (`docs/near-registry.md`).
- Re-copy `packages/near-link` into `src/vendor/near-link/` with
  `scripts/copy-near-link.ts` (new optional `bonuz_eoa`, `checked_by: "account"`, view
  helpers).

The app (bonuz-app-v2):

- `features/wallet/lib/near/linkMessage.ts` (`parseNearLinkSignRequest`, the gate in front
  of `SecureKeyManager.signNearMessage`) and its vendored `near-link/near-link.ts`: re-copy
  near-link (its parser accepts the fourth line), then require the `Wallet:` line from
  registry 2.2.0 and check it equals the signed-in user's creation wallet (lowercased),
  the same way the gate already checks the handle. Today's vendored parser refuses the
  four-line message ("message must have exactly three lines"), so an app without this
  change cannot link on 2.2.0. The gate's canonical-form check (`buildLinkMessage(parsed)
  === message`) keeps working with the new copy, which rebuilds the wallet line.

- A named NEAR wallet (`mende.near`) shows "Confirm on NEAR" for a pending claim
  (`link_proposal_of(account)`), and signs one transaction: `accept_link({ bonuz_eoa })` on
  the registry with 1 yoctoNEAR, receiver the registry, about 3 Tgas. The review screen
  names the registry contract, the method and the wallet.
- A "Not me" action: `reject_link()`.
- The verified badge for a named account follows `link_of(...).checked_by == "account"`.
