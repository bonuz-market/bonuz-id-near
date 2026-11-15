//! bonuz ID registry on NEAR.
//!
//! Links a NEAR account to a bonuz user, one to one. From 2.2.0 the user is
//! named by their creation wallet (`bonuz_eoa`, the EVM address made for them at
//! sign-up, which never changes). The handle (`bonuz_id`, the public profile at
//! `bonuz.id/<handle>`) is only a label that points at that wallet, so a rename
//! keeps the link and a later holder of an old handle inherits nothing.
//!
//! Two ways a NEAR account gets linked, and neither works on bonuz's word alone:
//!
//! * `link` (implicit accounts): the registrar sends a NEP-413 signature by the
//!   NEAR account's own key over a message that names the account, the handle,
//!   the network and an expiry. The contract re-checks the signature itself.
//! * `propose_link` + `accept_link` (named accounts such as `alice.near`): the
//!   registrar writes a pending claim, and the link only goes live when the
//!   named account itself calls `accept_link`.
//!
//! Storage is paid from the contract's own balance, so the registrar can use a
//! function-call access key (which cannot attach deposits) with a capped gas
//! allowance. What the contract proves and what it does not: see the README and
//! `docs/registry-2.2.0.md`.

pub mod proof;
#[cfg(test)]
mod tests;

use near_sdk::json_types::{Base64VecU8, U64};
use near_sdk::store::{IterableMap, LookupMap};
use near_sdk::{
    assert_one_yocto, env, near, require, AccountId, BorshStorageKey, NearToken, PanicOnDefault,
};

pub use proof::{
    build_link_message, check_bonuz_eoa, check_bonuz_id, check_link, is_expired, CheckedBy, Context,
    LinkInput,
};

/// Daily cap on `unlink_by_registrar` when no raised cap is active.
pub const DEFAULT_UNLINK_DAILY_CAP: u32 = 100;
/// How long a pending link proposal can be accepted.
pub const LINK_PROPOSAL_TTL_SECONDS: u64 = 7 * 86_400;
/// Most pending link proposals stored at once.
pub const MAX_LINK_PROPOSALS: u32 = 200;
/// Expired nonces (and proposals) removed by each `link` (`propose_link`) call.
const PRUNE_PER_LINK: u32 = 3;
/// Most entries one `prune_nonces` / `prune_link_proposals` call looks at.
const PRUNE_LIMIT_MAX: u32 = 50;
/// Most proposals one `link_proposals` view returns.
const LIST_LIMIT_MAX: u32 = 100;
const NS_PER_DAY: u64 = 86_400 * 1_000_000_000;

const ERR_TAKEN: &str = "this bonuz ID is linked to another NEAR account; unlink it first";
const ERR_LEGACY: &str = "a 2.1.0 link still names this bonuz ID; adopt_legacy_link or unlink it first";
const ERR_BACKFILL: &str =
    "2.1.0 links are not all adopted yet; adopt_legacy_link (or unlink) them before new links or proposals";

/// Variants are only ever appended: each one's index is its storage prefix.
#[near]
#[derive(BorshStorageKey)]
enum StorageKey {
    Links,
    AccountOf,
    Nonces,
    AccountOfEoa,
    HandleOfEoa,
    EoaOfHandle,
    Proposals,
    ProposalOfEoa,
}

/// A link as 2.0.0 and 2.1.0 stored it: keyed by handle. Never written by 2.2.0
/// except through `adopt_legacy_link`, which turns it into a `LinkV2`.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub bonuz_id: String,
    /// `ed25519:<base58>`, the key that signed the link message.
    pub public_key: String,
    /// `contract` (implicit account, key checked here) or `registrar` (named account, key checked by bonuz).
    pub checked_by: String,
    pub linked_at_block: U64,
    /// Block time in nanoseconds.
    pub linked_at: U64,
}

/// A 2.2.0 link: keyed by the user's creation wallet. The handle is looked up.
#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct LinkV2 {
    pub bonuz_eoa: String,
    /// The key that signed: the link message, or the `accept_link` transaction.
    /// None when the account accepted through a contract (the key is not known).
    pub public_key: Option<String>,
    /// `contract`, `registrar` or `account` (the account confirmed with `accept_link`).
    pub checked_by: String,
    pub linked_at_block: u64,
    pub linked_at: u64,
}

/// What `links` stores. A later version adds a variant instead of changing one.
#[near(serializers = [borsh])]
#[derive(Clone)]
pub enum VersionedLink {
    V1(Link),
    V2(LinkV2),
}

/// What `link_of` answers.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq)]
pub struct LinkView {
    /// The current handle label; `""` when the wallet has none.
    pub bonuz_id: String,
    /// The user's creation wallet; null for a 2.1.0 link not yet adopted.
    pub bonuz_eoa: Option<String>,
    /// Null only for a link accepted through a contract call.
    pub public_key: Option<String>,
    pub checked_by: String,
    pub linked_at_block: U64,
    pub linked_at: U64,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub bonuz_eoa: String,
    /// Block time in nanoseconds.
    pub proposed_at: u64,
    /// Unix seconds; can be accepted while the block time is before it.
    pub expires: u64,
}

#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq)]
pub struct LinkProposalView {
    pub account_id: AccountId,
    pub bonuz_eoa: String,
    /// The wallet's current handle label.
    pub bonuz_id: Option<String>,
    pub proposed_at: U64,
    pub expires: U64,
}

#[near(serializers = [json])]
pub struct Status {
    pub version: String,
    pub owner: AccountId,
    pub pending_owner: Option<AccountId>,
    pub registrar: AccountId,
    pub network: String,
    pub links: U64,
    /// 2.1.0 links (keyed by handle) not yet adopted.
    pub legacy_links: U64,
    pub allow_registrar_checked: bool,
    pub paused: bool,
    /// Balance not locked by storage, in yoctoNEAR. `link` refuses below `reserve`.
    pub available_balance: NearToken,
    pub reserve: NearToken,
    pub unlink_daily_cap: u32,
    /// Raised cap and its end (unix seconds), if one was set.
    pub raised_unlink_cap: Option<(u32, U64)>,
    pub unlinks_today: u32,
    pub stored_nonces: u32,
    /// Link proposals in storage, expired ones not yet pruned included.
    pub stored_link_proposals: u32,
    pub max_link_proposals: u32,
    pub link_proposal_ttl_seconds: U64,
}

/// The 2.1.0 state, field for field. Only `migrate` reads it.
#[near(serializers = [borsh])]
struct RegistryV2_1 {
    owner: AccountId,
    pending_owner: Option<AccountId>,
    registrar: AccountId,
    network: String,
    allow_registrar_checked: bool,
    paused: bool,
    reserve: NearToken,
    links: LookupMap<AccountId, VersionedLink>,
    link_count: u64,
    account_of: LookupMap<String, AccountId>,
    nonces: IterableMap<[u8; 32], u64>,
    raised_cap: Option<(u32, u64)>,
    unlink_day: u64,
    unlinks_today: u32,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Registry {
    owner: AccountId,
    /// Proposed by the owner, takes over on `accept_owner`.
    pending_owner: Option<AccountId>,
    registrar: AccountId,
    network: String,
    allow_registrar_checked: bool,
    paused: bool,
    reserve: NearToken,
    links: LookupMap<AccountId, VersionedLink>,
    /// How many NEAR accounts are linked right now.
    link_count: u64,
    /// Handle -> account, for 2.1.0 (`V1`) links only.
    account_of: LookupMap<String, AccountId>,
    /// Used nonce -> expiry of its message (unix seconds). Kept only until expiry.
    nonces: IterableMap<[u8; 32], u64>,
    /// Raised daily cap and its end (unix seconds).
    raised_cap: Option<(u32, u64)>,
    unlink_day: u64,
    unlinks_today: u32,
    // ---- 2.2.0 ----
    /// Creation wallet -> its live NEAR account (`V2` links).
    account_of_eoa: LookupMap<String, AccountId>,
    /// Creation wallet <-> handle: the display label.
    handle_of_eoa: LookupMap<String, String>,
    eoa_of_handle: LookupMap<String, String>,
    /// NEAR account -> its pending claim.
    proposals: IterableMap<AccountId, Proposal>,
    /// Creation wallet -> the NEAR account its pending claim names.
    proposal_of_eoa: LookupMap<String, AccountId>,
    /// How many `V1` links are left.
    legacy_links: u64,
}

/// NEP-297 events, standard `bonuz_id_near`.
#[near(event_json(standard = "bonuz_id_near"))]
pub enum RegistryEvent {
    /// `bonuz_eoa` is null only for a 2.1.0 link.
    #[event_version("2.2.0")]
    Link {
        account_id: AccountId,
        bonuz_id: String,
        bonuz_eoa: Option<String>,
        public_key: Option<String>,
        checked_by: String,
    },
    /// `reason`: `registrar` (unlink_by_registrar), `owner` (unlink_by_owner) or
    /// `replaced` (the same account was linked to another user).
    #[event_version("2.2.0")]
    Unlink { account_id: AccountId, bonuz_id: String, bonuz_eoa: Option<String>, reason: String },
    #[event_version("2.2.0")]
    LinkProposed { account_id: AccountId, bonuz_eoa: String, bonuz_id: String, expires: U64 },
    /// `reason`: `accepted`, `rejected`, `cancelled`, `expired` or `replaced`.
    #[event_version("2.2.0")]
    LinkProposalClosed { account_id: AccountId, bonuz_eoa: String, reason: String },
    /// A handle label was set (`bonuz_id`), moved or cleared (`bonuz_id` null).
    #[event_version("2.2.0")]
    Handle { bonuz_eoa: String, bonuz_id: Option<String>, previous: Option<String> },
}

#[near]
impl Registry {
    /// Only the contract account itself can initialise (no front-running).
    /// `network` is `testnet` or `mainnet` and must match the signed messages.
    #[init]
    #[private]
    pub fn new(
        owner: AccountId,
        registrar: AccountId,
        network: String,
        allow_registrar_checked: bool,
        reserve: NearToken,
    ) -> Self {
        require!(network == "testnet" || network == "mainnet", "network must be testnet or mainnet");
        Self {
            owner,
            pending_owner: None,
            registrar,
            network,
            allow_registrar_checked,
            paused: false,
            reserve,
            links: LookupMap::new(StorageKey::Links),
            link_count: 0,
            account_of: LookupMap::new(StorageKey::AccountOf),
            nonces: IterableMap::new(StorageKey::Nonces),
            raised_cap: None,
            unlink_day: 0,
            unlinks_today: 0,
            account_of_eoa: LookupMap::new(StorageKey::AccountOfEoa),
            handle_of_eoa: LookupMap::new(StorageKey::HandleOfEoa),
            eoa_of_handle: LookupMap::new(StorageKey::EoaOfHandle),
            proposals: IterableMap::new(StorageKey::Proposals),
            proposal_of_eoa: LookupMap::new(StorageKey::ProposalOfEoa),
            legacy_links: 0,
        }
    }

    /// 2.1.0 -> 2.2.0. Only the contract account can call it; send it in the
    /// same transaction as the code deploy. Every 2.1.0 link is kept as it is
    /// (same storage prefixes) and counted as legacy. Fails on any state that is
    /// not 2.1.0, so a second call fails.
    #[private]
    #[init(ignore_state)]
    pub fn migrate() -> Self {
        let old: RegistryV2_1 = env::state_read().unwrap_or_else(|| env::panic_str("no state to migrate"));
        Self {
            owner: old.owner,
            pending_owner: old.pending_owner,
            registrar: old.registrar,
            network: old.network,
            allow_registrar_checked: old.allow_registrar_checked,
            paused: old.paused,
            reserve: old.reserve,
            links: old.links,
            link_count: old.link_count,
            account_of: old.account_of,
            nonces: old.nonces,
            raised_cap: old.raised_cap,
            unlink_day: old.unlink_day,
            unlinks_today: old.unlinks_today,
            account_of_eoa: LookupMap::new(StorageKey::AccountOfEoa),
            handle_of_eoa: LookupMap::new(StorageKey::HandleOfEoa),
            eoa_of_handle: LookupMap::new(StorageKey::EoaOfHandle),
            proposals: IterableMap::new(StorageKey::Proposals),
            proposal_of_eoa: LookupMap::new(StorageKey::ProposalOfEoa),
            legacy_links: old.link_count,
        }
    }

    /// Registrar only. Links `account_id` to the bonuz user `bonuz_eoa` with a
    /// NEP-413 signature (recipient = this contract, no callback URL) over `message`:
    ///
    /// ```text
    /// Link NEAR account <account_id> to bonuz ID @<bonuz_id>
    /// Network: <testnet|mainnet>
    /// Expires: <unix seconds>
    /// ```
    ///
    /// `bonuz_id` becomes the wallet's handle label. Without `bonuz_eoa` the
    /// handle must already be a label (`set_handle`), else the call fails.
    /// If the account was linked to another user, that link is replaced (the
    /// account's new signature is the consent). If the user is linked to a
    /// different account, the call fails: the registrar must
    /// `unlink_by_registrar` that account first. Returns the block height.
    #[allow(clippy::too_many_arguments)]
    pub fn link(
        &mut self,
        account_id: AccountId,
        bonuz_id: String,
        public_key: String,
        nonce: Base64VecU8,
        signature: Base64VecU8,
        message: String,
        bonuz_eoa: Option<String>,
    ) -> U64 {
        self.assert_registrar();
        require!(!self.paused, "the registry is paused");
        self.assert_reserve();

        let now = env::block_timestamp();
        let input = LinkInput {
            account_id: account_id.as_str(),
            bonuz_id: &bonuz_id,
            public_key: &public_key,
            nonce: &nonce.0,
            signature: &signature.0,
            message: &message,
        };
        let current = env::current_account_id();
        let ctx = Context {
            recipient: current.as_str(),
            network: &self.network,
            now_ns: now,
            allow_registrar_checked: self.allow_registrar_checked,
        };
        // Expiry is checked inside check_link, before the nonce lookup below, so
        // a pruned nonce can only belong to a message that is refused as expired.
        let (checked_by, expires) = check_link(
            &input,
            &ctx,
            |data| env::sha256_array(data),
            |sig, hash, key| env::ed25519_verify(sig, hash, key),
        )
        .unwrap_or_else(|e| env::panic_str(e));

        // Every check runs before the first write.
        let nonce: [u8; 32] = nonce.0.try_into().unwrap_or_else(|_| env::abort());
        require!(!self.nonces.contains_key(&nonce), "this nonce was already used");
        let eoa = match bonuz_eoa {
            Some(eoa) => {
                check_bonuz_eoa(&eoa).unwrap_or_else(|e| env::panic_str(e));
                eoa
            }
            None => self.eoa_of_handle.get(&bonuz_id).cloned().unwrap_or_else(|| {
                env::panic_str("pass bonuz_eoa: from 2.2.0 a link is keyed to the user's creation wallet")
            }),
        };
        // While 2.1.0 links wait for adoption, a wallet's old link is invisible here
        // (it is keyed by handle), so a second live account could slip in.
        require!(self.legacy_links == 0, ERR_BACKFILL);
        if let Some(holder) = self.account_of_eoa.get(&eoa) {
            require!(holder == &account_id, ERR_TAKEN);
        }
        self.assert_handle_free_for(&bonuz_id, &eoa);

        self.nonces.insert(nonce, expires);
        self.write_link(account_id, &eoa, Some(&bonuz_id), Some(public_key), checked_by.as_str());
        self.prune(PRUNE_PER_LINK);
        U64(env::block_height())
    }

    /// Registrar only. Writes a pending claim: the bonuz user `bonuz_eoa`
    /// (handle `bonuz_id`) wants `account_id`. Nothing is linked until that
    /// account calls `accept_link`. Valid for 7 days. The same claim again only
    /// refreshes its expiry. Returns the expiry (unix seconds).
    pub fn propose_link(&mut self, account_id: AccountId, bonuz_eoa: String, bonuz_id: String) -> U64 {
        self.assert_registrar();
        require!(!self.paused, "the registry is paused");
        self.assert_reserve();
        check_bonuz_eoa(&bonuz_eoa).unwrap_or_else(|e| env::panic_str(e));
        check_bonuz_id(&bonuz_id).unwrap_or_else(|e| env::panic_str(e));
        require!(self.legacy_links == 0, ERR_BACKFILL);
        if let Some(holder) = self.account_of_eoa.get(&bonuz_eoa) {
            require!(holder != &account_id, "this NEAR account is already linked to this bonuz ID");
            env::panic_str(ERR_TAKEN);
        }
        self.assert_handle_free_for(&bonuz_id, &bonuz_eoa);

        self.prune_proposals(PRUNE_PER_LINK);
        let now = env::block_timestamp();
        let mut refresh = false;
        if let Some(p) = self.proposals.get(&account_id) {
            if is_expired(p.expires, now) {
                self.close_proposal(&account_id, "expired");
            } else if p.bonuz_eoa == bonuz_eoa {
                refresh = true;
            } else {
                env::panic_str("this NEAR account already has a pending link proposal; cancel it first");
            }
        }
        if let Some(other) = self.proposal_of_eoa.get(&bonuz_eoa).cloned() {
            if other != account_id {
                let expired = self.proposals.get(&other).map_or(true, |p| is_expired(p.expires, now));
                require!(expired, "this bonuz ID already has a pending link proposal for another NEAR account; cancel it first");
                self.close_proposal(&other, "expired");
            }
        }
        require!(
            refresh || self.proposals.len() < MAX_LINK_PROPOSALS,
            "too many pending link proposals; try again later"
        );

        self.set_label(&bonuz_eoa, Some(&bonuz_id));
        let expires = now / 1_000_000_000 + LINK_PROPOSAL_TTL_SECONDS;
        self.proposals.insert(account_id.clone(), Proposal { bonuz_eoa: bonuz_eoa.clone(), proposed_at: now, expires });
        self.proposal_of_eoa.insert(bonuz_eoa.clone(), account_id.clone());
        RegistryEvent::LinkProposed { account_id, bonuz_eoa, bonuz_id, expires: U64(expires) }.emit();
        U64(expires)
    }

    /// Registrar only. Removes the pending claim on `account_id`. False when
    /// there was none. Works while paused.
    pub fn cancel_link_proposal(&mut self, account_id: AccountId) -> bool {
        self.assert_registrar();
        if !self.proposals.contains_key(&account_id) {
            return false;
        }
        self.close_proposal(&account_id, "cancelled");
        true
    }

    /// The claimed NEAR account confirms its pending claim and the link goes
    /// live. `bonuz_eoa` must be the wallet the claim names, so the claim cannot
    /// be swapped between the tap and the block. Attach exactly 1 yoctoNEAR so
    /// only a full-access key can do it.
    #[payable]
    pub fn accept_link(&mut self, bonuz_eoa: String) {
        assert_one_yocto();
        require!(!self.paused, "the registry is paused");
        let account_id = env::predecessor_account_id();
        let p = self
            .proposals
            .get(&account_id)
            .cloned()
            .unwrap_or_else(|| env::panic_str("this NEAR account has no pending link proposal"));
        require!(!is_expired(p.expires, env::block_timestamp()), "the link proposal has expired; ask bonuz for a new one");
        require!(p.bonuz_eoa == bonuz_eoa, "the pending link proposal is for another bonuz ID");
        if let Some(holder) = self.account_of_eoa.get(&bonuz_eoa) {
            require!(holder == &account_id, ERR_TAKEN);
        }
        self.close_proposal(&account_id, "accepted");
        // The signer's key is the account's own key only when the account signed
        // this transaction itself; through a contract call it is someone else's.
        let public_key = (env::signer_account_id() == account_id).then(|| String::from(&env::signer_account_pk()));
        self.write_link(account_id, &bonuz_eoa, None, public_key, "account");
    }

    /// The claimed NEAR account turns its pending claim down. Attach exactly 1
    /// yoctoNEAR (full-access key only, like `accept_link`). Works while paused.
    #[payable]
    pub fn reject_link(&mut self) {
        assert_one_yocto();
        let account_id = env::predecessor_account_id();
        require!(self.proposals.contains_key(&account_id), "this NEAR account has no pending link proposal");
        self.close_proposal(&account_id, "rejected");
    }

    /// Registrar only. Sets the handle label of `bonuz_eoa` (a rename), or
    /// clears it with null. A handle that labelled another wallet moves here:
    /// that wallet keeps its link and loses only the label.
    pub fn set_handle(&mut self, bonuz_eoa: String, bonuz_id: Option<String>) {
        self.assert_registrar();
        require!(!self.paused, "the registry is paused");
        self.assert_reserve();
        check_bonuz_eoa(&bonuz_eoa).unwrap_or_else(|e| env::panic_str(e));
        if let Some(handle) = &bonuz_id {
            check_bonuz_id(handle).unwrap_or_else(|e| env::panic_str(e));
            require!(!self.account_of.contains_key(handle), ERR_LEGACY);
        }
        self.set_label(&bonuz_eoa, bonuz_id.as_deref());
    }

    /// Registrar only. Turns the 2.1.0 link of `account_id` into a 2.2.0 link
    /// of the user `bonuz_eoa`, keeping its key, time and handle (as the label).
    /// Works while paused, so the backfill can run behind a pause.
    pub fn adopt_legacy_link(&mut self, account_id: AccountId, bonuz_eoa: String) {
        self.assert_registrar();
        self.assert_reserve();
        check_bonuz_eoa(&bonuz_eoa).unwrap_or_else(|e| env::panic_str(e));
        let old = match self.links.get(&account_id) {
            Some(VersionedLink::V1(link)) => link.clone(),
            _ => env::panic_str("this NEAR account has no 2.1.0 link"),
        };
        require!(!self.account_of_eoa.contains_key(&bonuz_eoa), "this bonuz wallet is already linked to another NEAR account");

        self.account_of.remove(&old.bonuz_id);
        self.legacy_links = self.legacy_links.saturating_sub(1);
        self.close_proposals_of(&account_id, &bonuz_eoa);
        self.set_label(&bonuz_eoa, Some(&old.bonuz_id));
        self.links.insert(
            account_id.clone(),
            VersionedLink::V2(LinkV2 {
                bonuz_eoa: bonuz_eoa.clone(),
                public_key: Some(old.public_key.clone()),
                checked_by: old.checked_by.clone(),
                linked_at_block: old.linked_at_block.0,
                linked_at: old.linked_at.0,
            }),
        );
        self.account_of_eoa.insert(bonuz_eoa.clone(), account_id.clone());
        RegistryEvent::Link {
            account_id,
            bonuz_id: old.bonuz_id,
            bonuz_eoa: Some(bonuz_eoa),
            public_key: Some(old.public_key),
            checked_by: old.checked_by,
        }
        .emit();
    }

    /// Registrar only, removal only. Returns false (and uses no cap) when the
    /// account has no link, so a retry after success is harmless.
    pub fn unlink_by_registrar(&mut self, account_id: AccountId) -> bool {
        self.assert_registrar();
        require!(!self.paused, "the registry is paused");
        if !self.links.contains_key(&account_id) {
            return false;
        }
        let day = env::block_timestamp() / NS_PER_DAY;
        if day != self.unlink_day {
            self.unlink_day = day;
            self.unlinks_today = 0;
        }
        require!(self.unlinks_today < self.effective_cap(), "unlink cap reached for today");
        self.unlinks_today += 1;
        self.remove(&account_id, "registrar");
        true
    }

    /// The linked account removes its own link. Attach exactly 1 yoctoNEAR so
    /// only a full-access key can do it. Works while paused.
    #[payable]
    pub fn unlink_by_owner(&mut self) {
        assert_one_yocto();
        let account_id = env::predecessor_account_id();
        require!(self.links.contains_key(&account_id), "this NEAR account has no bonuz ID linked");
        self.remove(&account_id, "owner");
    }

    /// Anyone may call. Deletes up to `limit` (max 50) expired nonces. Never
    /// transfers anything; the caller pays its own gas. Returns how many were deleted.
    pub fn prune_nonces(&mut self, limit: u32) -> u32 {
        self.prune(limit.min(PRUNE_LIMIT_MAX))
    }

    /// Anyone may call. Deletes up to `limit` (max 50) expired link proposals.
    pub fn prune_link_proposals(&mut self, limit: u32) -> u32 {
        self.prune_proposals(limit.min(PRUNE_LIMIT_MAX))
    }

    // ---- owner ----

    /// Step one of an owner change. The new owner must call `accept_owner`;
    /// until then nothing changes and the owner can propose someone else.
    #[payable]
    pub fn propose_owner(&mut self, new_owner: AccountId) {
        self.assert_owner();
        self.pending_owner = Some(new_owner);
    }

    /// Step two: the proposed account takes over. Attach 1 yoctoNEAR.
    #[payable]
    pub fn accept_owner(&mut self) {
        assert_one_yocto();
        let caller = env::predecessor_account_id();
        require!(self.pending_owner.as_ref() == Some(&caller), "only the proposed owner can accept");
        self.owner = caller;
        self.pending_owner = None;
    }

    #[payable]
    pub fn set_registrar(&mut self, registrar: AccountId) {
        self.assert_owner();
        self.registrar = registrar;
    }

    #[payable]
    pub fn set_paused(&mut self, paused: bool) {
        self.assert_owner();
        self.paused = paused;
    }

    #[payable]
    pub fn set_reserve(&mut self, reserve: NearToken) {
        self.assert_owner();
        self.reserve = reserve;
    }

    /// Raises (or lowers) the daily `unlink_by_registrar` cap until `until`
    /// (unix seconds). After that the cap falls back to 100 on its own.
    #[payable]
    pub fn set_unlink_daily_cap(&mut self, cap: u32, until: U64) {
        self.assert_owner();
        self.raised_cap = Some((cap, until.0));
    }

    // ---- views ----

    /// The current handle of the user `account_id` is linked to.
    pub fn bonuz_of(&self, account_id: AccountId) -> Option<String> {
        match self.links.get(&account_id)? {
            VersionedLink::V1(link) => Some(link.bonuz_id.clone()),
            VersionedLink::V2(link) => self.handle_of_eoa.get(&link.bonuz_eoa).cloned(),
        }
    }

    /// handle -> creation wallet -> NEAR account (or a 2.1.0 link of that handle).
    pub fn account_of(&self, bonuz_id: String) -> Option<AccountId> {
        match self.eoa_of_handle.get(&bonuz_id) {
            Some(eoa) => self.account_of_eoa.get(eoa).cloned(),
            None => self.account_of.get(&bonuz_id).cloned(),
        }
    }

    pub fn link_of(&self, account_id: AccountId) -> Option<LinkView> {
        Some(match self.links.get(&account_id)? {
            VersionedLink::V1(l) => LinkView {
                bonuz_id: l.bonuz_id.clone(),
                bonuz_eoa: None,
                public_key: Some(l.public_key.clone()),
                checked_by: l.checked_by.clone(),
                linked_at_block: l.linked_at_block,
                linked_at: l.linked_at,
            },
            VersionedLink::V2(l) => LinkView {
                bonuz_id: self.handle_of_eoa.get(&l.bonuz_eoa).cloned().unwrap_or_default(),
                bonuz_eoa: Some(l.bonuz_eoa.clone()),
                public_key: l.public_key.clone(),
                checked_by: l.checked_by.clone(),
                linked_at_block: U64(l.linked_at_block),
                linked_at: U64(l.linked_at),
            },
        })
    }

    pub fn account_of_eoa(&self, bonuz_eoa: String) -> Option<AccountId> {
        self.account_of_eoa.get(&bonuz_eoa).cloned()
    }

    /// The creation wallet a handle currently labels.
    pub fn eoa_of(&self, bonuz_id: String) -> Option<String> {
        self.eoa_of_handle.get(&bonuz_id).cloned()
    }

    /// The current handle label of a creation wallet.
    pub fn handle_of(&self, bonuz_eoa: String) -> Option<String> {
        self.handle_of_eoa.get(&bonuz_eoa).cloned()
    }

    /// The pending claim on a NEAR account; none once expired.
    pub fn link_proposal_of(&self, account_id: AccountId) -> Option<LinkProposalView> {
        let p = self.proposals.get(&account_id)?;
        self.proposal_view(&account_id, p)
    }

    /// The pending claim a creation wallet has; none once expired.
    pub fn link_proposal_for(&self, bonuz_eoa: String) -> Option<LinkProposalView> {
        let account_id = self.proposal_of_eoa.get(&bonuz_eoa)?;
        self.proposal_view(account_id, self.proposals.get(account_id)?)
    }

    /// Stored claims from `from_index`, at most `limit` (max 100), expired ones skipped.
    pub fn link_proposals(&self, from_index: Option<u32>, limit: Option<u32>) -> Vec<LinkProposalView> {
        self.proposals
            .iter()
            .skip(from_index.unwrap_or(0) as usize)
            .take(limit.unwrap_or(LIST_LIMIT_MAX).min(LIST_LIMIT_MAX) as usize)
            .filter_map(|(a, p)| self.proposal_view(a, p))
            .collect()
    }

    pub fn status(&self) -> Status {
        let today = env::block_timestamp() / NS_PER_DAY;
        Status {
            version: env!("CARGO_PKG_VERSION").into(),
            owner: self.owner.clone(),
            pending_owner: self.pending_owner.clone(),
            registrar: self.registrar.clone(),
            network: self.network.clone(),
            links: U64(self.link_count),
            legacy_links: U64(self.legacy_links),
            allow_registrar_checked: self.allow_registrar_checked,
            paused: self.paused,
            available_balance: self.available_balance(),
            reserve: self.reserve,
            unlink_daily_cap: self.effective_cap(),
            raised_unlink_cap: self.raised_cap.map(|(c, u)| (c, U64(u))),
            unlinks_today: if today == self.unlink_day { self.unlinks_today } else { 0 },
            stored_nonces: self.nonces.len(),
            stored_link_proposals: self.proposals.len(),
            max_link_proposals: MAX_LINK_PROPOSALS,
            link_proposal_ttl_seconds: U64(LINK_PROPOSAL_TTL_SECONDS),
        }
    }
}

impl Registry {
    fn assert_registrar(&self) {
        require!(env::predecessor_account_id() == self.registrar, "only the bonuz registrar can call this");
    }

    fn assert_owner(&self) {
        assert_one_yocto();
        require!(env::predecessor_account_id() == self.owner, "only the registry owner can call this");
    }

    fn assert_reserve(&self) {
        require!(
            self.available_balance() >= self.reserve,
            "the registry balance is under its reserve; top it up before linking"
        );
    }

    fn available_balance(&self) -> NearToken {
        let locked = env::storage_byte_cost().saturating_mul(u128::from(env::storage_usage()));
        env::account_balance().saturating_sub(locked)
    }

    fn effective_cap(&self) -> u32 {
        match self.raised_cap {
            Some((cap, until)) if !is_expired(until, env::block_timestamp()) => cap,
            _ => DEFAULT_UNLINK_DAILY_CAP,
        }
    }

    /// `link` and `propose_link` never move a handle away from another wallet
    /// that has a live link (2.1.0 refused that too). Moving it takes an explicit
    /// `set_handle` first. A 2.1.0 link of the handle also blocks it.
    fn assert_handle_free_for(&self, handle: &str, eoa: &str) {
        require!(!self.account_of.contains_key(handle), ERR_LEGACY);
        if let Some(other) = self.eoa_of_handle.get(handle) {
            require!(other == eoa || !self.account_of_eoa.contains_key(other), ERR_TAKEN);
        }
    }

    fn proposal_view(&self, account_id: &AccountId, p: &Proposal) -> Option<LinkProposalView> {
        if is_expired(p.expires, env::block_timestamp()) {
            return None;
        }
        Some(LinkProposalView {
            account_id: account_id.clone(),
            bonuz_eoa: p.bonuz_eoa.clone(),
            bonuz_id: self.handle_of_eoa.get(&p.bonuz_eoa).cloned(),
            proposed_at: U64(p.proposed_at),
            expires: U64(p.expires),
        })
    }

    /// Points `handle` at `eoa` (or clears the wallet's label). Callers have
    /// checked the handle is not held by a 2.1.0 link.
    fn set_label(&mut self, eoa: &str, handle: Option<&str>) {
        let previous = self.handle_of_eoa.get(eoa).cloned();
        if previous.as_deref() == handle {
            return;
        }
        if let Some(p) = &previous {
            self.eoa_of_handle.remove(p);
        }
        match handle {
            Some(h) => {
                if let Some(other) = self.eoa_of_handle.get(h).cloned() {
                    self.handle_of_eoa.remove(&other);
                    RegistryEvent::Handle { bonuz_eoa: other, bonuz_id: None, previous: Some(h.into()) }.emit();
                }
                self.eoa_of_handle.insert(h.into(), eoa.into());
                self.handle_of_eoa.insert(eoa.into(), h.into());
            }
            None => {
                self.handle_of_eoa.remove(eoa);
            }
        }
        RegistryEvent::Handle { bonuz_eoa: eoa.into(), bonuz_id: handle.map(Into::into), previous }.emit();
    }

    /// Writes the live link `account_id` -> `eoa`, replacing the account's
    /// other link, and closes any claim on either. Callers have checked that
    /// `eoa` is not linked to another account.
    fn write_link(
        &mut self,
        account_id: AccountId,
        eoa: &str,
        handle: Option<&str>,
        public_key: Option<String>,
        checked_by: &str,
    ) {
        let same_user = matches!(self.links.get(&account_id), Some(VersionedLink::V2(l)) if l.bonuz_eoa == eoa);
        if !same_user && self.links.contains_key(&account_id) {
            self.remove(&account_id, "replaced");
        }
        self.close_proposals_of(&account_id, eoa);
        if let Some(h) = handle {
            self.set_label(eoa, Some(h));
        }
        let link = VersionedLink::V2(LinkV2 {
            bonuz_eoa: eoa.into(),
            public_key: public_key.clone(),
            checked_by: checked_by.into(),
            linked_at_block: env::block_height(),
            linked_at: env::block_timestamp(),
        });
        if self.links.insert(account_id.clone(), link).is_none() {
            self.link_count += 1;
        }
        self.account_of_eoa.insert(eoa.into(), account_id.clone());
        RegistryEvent::Link {
            account_id,
            bonuz_id: self.handle_of_eoa.get(eoa).cloned().unwrap_or_default(),
            bonuz_eoa: Some(eoa.into()),
            public_key,
            checked_by: checked_by.into(),
        }
        .emit();
    }

    fn remove(&mut self, account_id: &AccountId, reason: &str) {
        let (bonuz_id, bonuz_eoa) = match self.links.remove(account_id).unwrap_or_else(|| env::abort()) {
            VersionedLink::V1(link) => {
                self.account_of.remove(&link.bonuz_id);
                self.legacy_links = self.legacy_links.saturating_sub(1);
                (link.bonuz_id, None)
            }
            VersionedLink::V2(link) => {
                self.account_of_eoa.remove(&link.bonuz_eoa);
                (self.handle_of_eoa.get(&link.bonuz_eoa).cloned().unwrap_or_default(), Some(link.bonuz_eoa))
            }
        };
        self.link_count = self.link_count.saturating_sub(1);
        RegistryEvent::Unlink { account_id: account_id.clone(), bonuz_id, bonuz_eoa, reason: reason.into() }.emit();
    }

    fn close_proposal(&mut self, account_id: &AccountId, reason: &str) {
        if let Some(p) = self.proposals.remove(account_id) {
            if self.proposal_of_eoa.get(&p.bonuz_eoa) == Some(account_id) {
                self.proposal_of_eoa.remove(&p.bonuz_eoa);
            }
            RegistryEvent::LinkProposalClosed { account_id: account_id.clone(), bonuz_eoa: p.bonuz_eoa, reason: reason.into() }
                .emit();
        }
    }

    /// Closes the claim on `account_id` and the claim of `eoa`, as `replaced`.
    fn close_proposals_of(&mut self, account_id: &AccountId, eoa: &str) {
        self.close_proposal(account_id, "replaced");
        if let Some(other) = self.proposal_of_eoa.get(eoa).cloned() {
            self.close_proposal(&other, "replaced");
        }
    }

    /// Looks at up to `limit` nonces starting at a random position and wrapping
    /// around. A fixed start would stall: removing an entry moves the newest one
    /// into its slot, so the first slots fill with nonces that cannot expire yet.
    fn prune(&mut self, limit: u32) -> u32 {
        let len = self.nonces.len() as usize;
        if len == 0 || limit == 0 {
            return 0;
        }
        let seed = env::random_seed();
        let start = u32::from_le_bytes([seed[0], seed[1], seed[2], seed[3]]) as usize % len;
        let now = env::block_timestamp();
        let expired: Vec<[u8; 32]> = self
            .nonces
            .iter()
            .skip(start)
            .chain(self.nonces.iter().take(start))
            .take(limit as usize)
            .filter(|(_, &expires)| is_expired(expires, now))
            .map(|(k, _)| *k)
            .collect();
        for k in &expired {
            self.nonces.remove(k);
        }
        expired.len() as u32
    }

    /// The same random-start walk over the link proposals.
    fn prune_proposals(&mut self, limit: u32) -> u32 {
        let len = self.proposals.len() as usize;
        if len == 0 || limit == 0 {
            return 0;
        }
        let seed = env::random_seed();
        let start = u32::from_le_bytes([seed[4], seed[5], seed[6], seed[7]]) as usize % len;
        let now = env::block_timestamp();
        let expired: Vec<AccountId> = self
            .proposals
            .iter()
            .skip(start)
            .chain(self.proposals.iter().take(start))
            .take(limit as usize)
            .filter(|(_, p)| is_expired(p.expires, now))
            .map(|(a, _)| a.clone())
            .collect();
        for a in &expired {
            self.close_proposal(a, "expired");
        }
        expired.len() as u32
    }
}
