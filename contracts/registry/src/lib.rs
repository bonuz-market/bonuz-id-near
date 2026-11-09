//! bonuz ID registry on NEAR.
//!
//! Links a NEAR account to a bonuz ID handle (the public profile at
//! `bonuz.id/<handle>`), one to one. Only the bonuz registrar account can write,
//! and every link carries a NEP-413 signature by the NEAR account's own key over
//! a message that names the account, the handle, the network and an expiry. The
//! contract re-checks that signature itself, so bonuz cannot attach a NEAR
//! account to a handle without that account's key.
//!
//! What the contract proves and what it does not: see the README.
//!
//! * `link`: registrar only. Verifies the signature and the message, refuses a
//!   used nonce, then writes the pair. Storage is paid from the contract's own
//!   balance, so the registrar can use a function-call access key (which cannot
//!   attach deposits) with a capped gas allowance.
//! * `unlink_by_registrar`: registrar only, removal only, capped per day.
//! * `unlink_by_owner`: the linked account removes itself.
//! * Views: `bonuz_of`, `account_of`, `link_of`, `status`.

pub mod proof;
#[cfg(test)]
mod tests;

use near_sdk::json_types::{Base64VecU8, U64};
use near_sdk::store::{IterableMap, LookupMap};
use near_sdk::{
    assert_one_yocto, env, near, require, AccountId, BorshStorageKey, NearToken, PanicOnDefault,
};

pub use proof::{build_link_message, check_link, is_expired, CheckedBy, Context, LinkInput};

/// Daily cap on `unlink_by_registrar` when no raised cap is active.
pub const DEFAULT_UNLINK_DAILY_CAP: u32 = 100;
/// Expired nonces removed by each `link` call, on top of `prune_nonces`.
const PRUNE_PER_LINK: u32 = 3;
/// Most nonces one `prune_nonces` call looks at.
const PRUNE_LIMIT_MAX: u32 = 50;
const NS_PER_DAY: u64 = 86_400 * 1_000_000_000;

#[near]
#[derive(BorshStorageKey)]
enum StorageKey {
    Links,
    AccountOf,
    Nonces,
}

/// One linked NEAR account.
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

/// What `links` stores. A later version adds a variant instead of changing `Link`.
#[near(serializers = [borsh])]
#[derive(Clone)]
pub enum VersionedLink {
    V1(Link),
}

impl VersionedLink {
    fn get(&self) -> &Link {
        match self {
            VersionedLink::V1(link) => link,
        }
    }
}

#[near(serializers = [json])]
pub struct Status {
    pub version: String,
    pub owner: AccountId,
    pub pending_owner: Option<AccountId>,
    pub registrar: AccountId,
    pub network: String,
    pub links: U64,
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
    account_of: LookupMap<String, AccountId>,
    /// Used nonce -> expiry of its message (unix seconds). Kept only until expiry.
    nonces: IterableMap<[u8; 32], u64>,
    /// Raised daily cap and its end (unix seconds).
    raised_cap: Option<(u32, u64)>,
    unlink_day: u64,
    unlinks_today: u32,
}

/// NEP-297 events, standard `bonuz_id_near`.
#[near(event_json(standard = "bonuz_id_near"))]
pub enum RegistryEvent {
    #[event_version("2.0.0")]
    Link { account_id: AccountId, bonuz_id: String, public_key: String, checked_by: String },
    /// `reason`: `registrar` (unlink_by_registrar), `owner` (unlink_by_owner) or
    /// `replaced` (the same account signed a link to another handle).
    #[event_version("2.0.0")]
    Unlink { account_id: AccountId, bonuz_id: String, reason: String },
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
        }
    }

    /// Starts this version on an account that ran 1.x (testnet only): removes
    /// the old storage keys, listed by the deploy script with `view_state`,
    /// then initialises like `new`. Only the contract account can call it.
    #[init(ignore_state)]
    #[private]
    pub fn migrate_from_v1(
        old_keys: Vec<Base64VecU8>,
        owner: AccountId,
        registrar: AccountId,
        network: String,
        allow_registrar_checked: bool,
        reserve: NearToken,
    ) -> Self {
        // Never on a live 2.x registry: its state would deserialize as this version.
        if let Some(raw) = env::storage_read(b"STATE") {
            require!(
                near_sdk::borsh::from_slice::<Registry>(&raw).is_err(),
                "migrate_from_v1 refused: this account already runs version 2"
            );
        }
        for key in old_keys {
            env::storage_remove(&key.0);
        }
        Self::new(owner, registrar, network, allow_registrar_checked, reserve)
    }

    /// Registrar only. Links `account_id` to `bonuz_id` with a NEP-413
    /// signature (recipient = this contract, no callback URL) over `message`:
    ///
    /// ```text
    /// Link NEAR account <account_id> to bonuz ID @<bonuz_id>
    /// Network: <testnet|mainnet>
    /// Expires: <unix seconds>
    /// ```
    ///
    /// If the account was linked to another handle, that pair is replaced
    /// (the account's new signature is the consent). If the handle is linked
    /// to a different account, the call fails: the registrar must
    /// `unlink_by_registrar` that account first. Returns the block height.
    pub fn link(
        &mut self,
        account_id: AccountId,
        bonuz_id: String,
        public_key: String,
        nonce: Base64VecU8,
        signature: Base64VecU8,
        message: String,
    ) -> U64 {
        self.assert_registrar();
        require!(!self.paused, "the registry is paused");
        require!(
            self.available_balance() >= self.reserve,
            "the registry balance is under its reserve; top it up before linking"
        );

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
        if let Some(holder) = self.account_of.get(&bonuz_id) {
            require!(
                holder == &account_id,
                "this bonuz ID is linked to another NEAR account; unlink it first"
            );
        }

        self.nonces.insert(nonce, expires);
        if let Some(old) = self.links.get(&account_id).map(|l| l.get().clone()) {
            if old.bonuz_id != bonuz_id {
                self.account_of.remove(&old.bonuz_id);
                RegistryEvent::Unlink {
                    account_id: account_id.clone(),
                    bonuz_id: old.bonuz_id,
                    reason: "replaced".into(),
                }
                .emit();
            }
        }

        let block = env::block_height();
        let link = VersionedLink::V1(Link {
                bonuz_id: bonuz_id.clone(),
                public_key: public_key.clone(),
                checked_by: checked_by.as_str().into(),
                linked_at_block: U64(block),
                linked_at: U64(now),
        });
        if self.links.insert(account_id.clone(), link).is_none() {
            self.link_count += 1;
        }
        self.account_of.insert(bonuz_id.clone(), account_id.clone());
        self.prune(PRUNE_PER_LINK);
        RegistryEvent::Link { account_id, bonuz_id, public_key, checked_by: checked_by.as_str().into() }
            .emit();
        U64(block)
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

    pub fn bonuz_of(&self, account_id: AccountId) -> Option<String> {
        self.links.get(&account_id).map(|l| l.get().bonuz_id.clone())
    }

    pub fn account_of(&self, bonuz_id: String) -> Option<AccountId> {
        self.account_of.get(&bonuz_id).cloned()
    }

    pub fn link_of(&self, account_id: AccountId) -> Option<Link> {
        self.links.get(&account_id).map(|l| l.get().clone())
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
            allow_registrar_checked: self.allow_registrar_checked,
            paused: self.paused,
            available_balance: self.available_balance(),
            reserve: self.reserve,
            unlink_daily_cap: self.effective_cap(),
            raised_unlink_cap: self.raised_cap.map(|(c, u)| (c, U64(u))),
            unlinks_today: if today == self.unlink_day { self.unlinks_today } else { 0 },
            stored_nonces: self.nonces.len(),
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

    fn remove(&mut self, account_id: &AccountId, reason: &str) {
        let link = self.links.remove(account_id).unwrap_or_else(|| env::abort()).get().clone();
        self.link_count -= 1;
        self.account_of.remove(&link.bonuz_id);
        RegistryEvent::Unlink { account_id: account_id.clone(), bonuz_id: link.bonuz_id, reason: reason.into() }
            .emit();
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
}
