# Registry 2.2.0: links keyed to the creation wallet, named accounts confirmed on chain

Status: built and rehearsed on testnet (15 Nov 2026). Mainnet `id.bonuz-id.near` stays on
2.1.0 until Mende says yes, not before 26 Nov.

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
   alone. The implicit-account path (NEP-413 signature checked by the contract) stays.

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
    registrar. Unchanged NEP-413 check. With bonuz_eoa: labels bonuz_id -> bonuz_eoa and
    links account -> bonuz_eoa. Without it: the handle must already be a label; refused
    otherwise ("pass bonuz_eoa ...").
propose_link(account_id, bonuz_eoa, bonuz_id) -> U64 (expires, unix seconds)
    registrar. Labels bonuz_id -> bonuz_eoa and writes a pending claim valid 7 days.
    Same account + same wallet again only refreshes the expiry (safe retry). Refused when
    the account or the wallet already has another pending claim, or the wallet is linked
    to another account.
cancel_link_proposal(account_id) -> bool          registrar. False when there is none.
accept_link(bonuz_eoa)                            the claimed account itself, 1 yoctoNEAR.
reject_link()                                     the claimed account itself.
set_handle(bonuz_eoa, bonuz_id?: String)          registrar. Sets, moves or clears a label.
adopt_legacy_link(account_id, bonuz_eoa)          registrar. Turns a 2.1.0 link into a
    2.2.0 one, keeping its handle as the label. Migration backfill.
prune_link_proposals(limit) -> u32                anyone. Deletes up to 50 expired claims.
migrate()                                         private (the contract account), once.
```

New views: `identity_of(account_id)`, `account_of_eoa(bonuz_eoa)`, `eoa_of(bonuz_id)`,
`handle_of(bonuz_eoa)`, `link_proposal_of(account_id)`, `link_proposal_for(bonuz_eoa)`,
`link_proposals(from_index, limit)`. Expired claims read as none.

Unchanged: `unlink_by_registrar`, `unlink_by_owner`, `prune_nonces`, every owner method.
`bonuz_of(account)` answers the current handle label. `account_of(handle)` resolves
`handle -> EOA -> account`, falling back to the 2.1.0 map for a not yet adopted link.
`link_of` keeps its fields (`bonuz_id` is the current label, `""` when the wallet has
none) and adds `bonuz_eoa` (null for a 2.1.0 link). `status()` adds `pending_links`,
`max_pending_links`, `link_proposal_ttl_seconds` and `legacy_links`.

Accepted links record `checked_by: "account"` and the public key that signed the
`accept_link` transaction.

Events (standard `bonuz_id_near`): `link` and `unlink` move to version 2.2.0 and add
`bonuz_eoa`; new `link_proposed`, `link_proposal_closed` (reason `accepted`, `rejected`,
`cancelled`, `expired`, `replaced`) and `handle` (label set, moved or cleared).

## Griefing and front-running

- **Who can write a claim**: only the registrar. Nobody else can fill storage with claims.
- **Who pays**: the contract's own balance, like links. `propose_link` refuses under the
  reserve, as `link` does.
- **Caps**: at most 1,000 pending claims at once (about 0.25 NEAR of storage at ~250 bytes
  each), one per NEAR account, one per user. Every `propose_link` also deletes up to 3
  expired claims, and anyone can call `prune_link_proposals`.
- **Expiry**: 7 days. An expired claim cannot be accepted and reads as none.
- **A leaked registrar key** can fill the 1,000 slots for up to 7 days (owner pauses,
  rotates the registrar, and the registrar cancels), and can label handles. It still
  cannot make a live link to a named account it does not hold.
- **Swapping a claim under the user**: `accept_link` names the wallet it accepts. If the
  registrar cancelled and re-proposed to another wallet between the user's tap and the
  block, the accept fails instead of linking to the wrong user.
- **Someone else accepting**: `accept_link` only acts on the claim of the predecessor
  account. A third party has no claim to accept.
- **1 yoctoNEAR on accept**: only a full-access key (the user's wallet) can attach a
  deposit, so a function-call key some dapp added cannot accept on the user's behalf.
- **The wallet got linked meanwhile**: `accept_link` re-checks. If the wallet is linked to
  another NEAR account by then, the accept fails ("unlink it first"), the same rule `link`
  has for a taken handle. If the accepting account was linked to another wallet, that old
  link is replaced: the account's own accept is the consent, as with `link`.
- **Paused**: `propose_link` and `accept_link` refuse while paused. `reject_link`,
  `cancel_link_proposal` and `unlink_by_owner` still work.

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
`status().legacy_links` counts down to 0. Mainnet has very few links (it went live
14 Nov), testnet a handful.

## What this does not do

- It does not prove who holds a handle. Only bonuz knows that.
- It does not move existing `allow_registrar_checked` behaviour: mainnet stays `false`.
  Named accounts on mainnet go through `propose_link` + `accept_link` only. There is
  deliberately no `set_allow_registrar_checked`.
- The NEP-413 message still names the handle, not the wallet, so the app's signing text
  and `packages/near-link` do not change.
