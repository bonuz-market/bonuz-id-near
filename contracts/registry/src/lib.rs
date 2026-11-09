//! bonuz ID registry on NEAR.
//!
//! Links a NEAR account to a bonuz ID handle (the profile at `bonuz.id/<handle>`).
//! One NEAR account maps to at most one bonuz ID and one bonuz ID maps to at
//! most one NEAR account.
//!
//! Two ways to link:
//!
//! * `link(bonuz_id)`: the NEAR account calls the contract itself.
//!   `predecessor_account_id` is the proof, the account pays its own storage.
//!   Works for any account that exists on chain (named or implicit).
//!
//! * `link_signed(account_id, bonuz_id, nonce, signature)`: one-click path for
//!   the wallets the bonuz app creates. Those are NEAR implicit accounts: the
//!   account id is the hex of an ed25519 public key, so the contract can check a
//!   NEP-413 signature made with that key, even before the account exists on
//!   chain. The bonuz sponsor account submits the call and pays the storage; the
//!   user needs no NEAR and pays no gas. Because only the sponsor can submit,
//!   a sponsored link is also bonuz's statement that the handle belongs to the
//!   person who signed.
//!
//! Every link has one payer (the account itself or the sponsor). Removing a link
//! refunds exactly the storage it freed to that payer.
//!
//! Trust model: a self-made link proves control of the NEAR account, not of the
//! bonuz ID. A link counts as verified only when the bonuz ID on Base points back
//! at the same NEAR account (its `w_near` slot). The owner can remove a squatted
//! claim with `admin_unlink`, and a sponsored link replaces a squatter directly.

use near_sdk::json_types::Base64VecU8;
use near_sdk::store::LookupMap;
use near_sdk::{
    assert_one_yocto, borsh, env, near, require, AccountId, BorshStorageKey, NearToken,
    PanicOnDefault, Promise, StorageUsage,
};

/// Shortest allowed bonuz ID handle.
pub const MIN_LEN: usize = 2;
/// Longest allowed bonuz ID handle.
pub const MAX_LEN: usize = 32;

/// Upper bound of storage bytes one link can use: two map records with the
/// longest account id (64) and the longest handle (32), a sponsor record and a
/// signed-call counter, each with NEAR's 40 byte per-record overhead, rounded up.
const MAX_LINK_BYTES: StorageUsage = 600;

/// NEP-413 tag: 2^31 + 413, prepended (borsh, little-endian) before hashing.
const NEP413_TAG: u32 = (1 << 31) + 413;

#[near]
#[derive(BorshStorageKey)]
enum StorageKey {
    BonuzOf,
    AccountOf,
    SponsorOf,
    SignedCounter,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Registry {
    /// May remove squatted links and set the sponsor. Cannot create links.
    owner_id: AccountId,
    /// The only account allowed to submit `link_signed` / `unlink_signed`.
    sponsor_id: Option<AccountId>,
    /// NEAR account -> bonuz ID handle.
    bonuz_of: LookupMap<AccountId, String>,
    /// bonuz ID handle -> NEAR account.
    account_of: LookupMap<String, AccountId>,
    /// NEAR account -> who paid its storage, only when that is not the account itself.
    sponsor_of: LookupMap<AccountId, AccountId>,
    /// Implicit account -> number of signed calls used. Part of every signed
    /// message, so a signature works once. Kept after unlink on purpose.
    signed_counter: LookupMap<AccountId, u32>,
}

/// State layout of version 1.0.0 (deployed 9 Nov 2026), read by `migrate`.
#[near(serializers = [borsh])]
struct RegistryV1 {
    owner_id: AccountId,
    bonuz_of: LookupMap<AccountId, String>,
    account_of: LookupMap<String, AccountId>,
}

/// NEP-413 message payload, borsh-serialized and hashed together with the tag.
#[near(serializers = [borsh])]
struct Nep413Payload {
    message: String,
    nonce: [u8; 32],
    recipient: String,
    callback_url: Option<String>,
}

/// NEP-297 events, standard `bonuz_id_near`.
#[near(event_json(standard = "bonuz_id_near"))]
pub enum RegistryEvent {
    #[event_version("1.0.0")]
    Link {
        account_id: AccountId,
        bonuz_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        previous_bonuz_id: Option<String>,
        /// Set when the bonuz sponsor submitted a signed link.
        #[serde(skip_serializing_if = "Option::is_none")]
        sponsored_by: Option<AccountId>,
    },
    #[event_version("1.0.0")]
    Unlink {
        account_id: AccountId,
        bonuz_id: String,
        /// Set when someone other than the account removed the link
        /// (the owner, or the sponsor replacing a squatter).
        #[serde(skip_serializing_if = "Option::is_none")]
        removed_by: Option<AccountId>,
    },
}

/// Panics unless `bonuz_id` is 2 to 32 chars of `a-z`, `0-9` or `_`.
pub fn assert_valid_bonuz_id(bonuz_id: &str) {
    let len = bonuz_id.len();
    if !(MIN_LEN..=MAX_LEN).contains(&len) {
        env::panic_str(&format!(
            "bonuz ID must be {MIN_LEN} to {MAX_LEN} characters, got {len}"
        ));
    }
    if !bonuz_id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        env::panic_str("bonuz ID may only contain a-z, 0-9 and _ (lowercase)");
    }
}

/// The ed25519 public key of a NEAR implicit account (64 lowercase hex chars).
pub fn implicit_public_key(account_id: &str) -> Option<[u8; 32]> {
    let bytes = account_id.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut key = [0u8; 32];
    for (i, pair) in bytes.chunks(2).enumerate() {
        key[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(key)
}

/// The text the wallet signs (NEP-413 `message`) to link `bonuz_id`.
pub fn link_message(bonuz_id: &str, counter: u32) -> String {
    format!("Link this NEAR account to bonuz ID {bonuz_id} (#{counter})")
}

/// The text the wallet signs (NEP-413 `message`) to unlink.
pub fn unlink_message(counter: u32) -> String {
    format!("Unlink this NEAR account from its bonuz ID (#{counter})")
}

#[near]
impl Registry {
    /// Only the contract account itself can initialise (no front-running).
    #[init]
    #[private]
    pub fn new(owner_id: AccountId, sponsor_id: Option<AccountId>) -> Self {
        Self {
            owner_id,
            sponsor_id,
            bonuz_of: LookupMap::new(StorageKey::BonuzOf),
            account_of: LookupMap::new(StorageKey::AccountOf),
            sponsor_of: LookupMap::new(StorageKey::SponsorOf),
            signed_counter: LookupMap::new(StorageKey::SignedCounter),
        }
    }

    /// Upgrades state from version 1.0.0. Run once, in the deploy transaction.
    #[init(ignore_state)]
    #[private]
    pub fn migrate(sponsor_id: Option<AccountId>) -> Self {
        let old: RegistryV1 =
            env::state_read().unwrap_or_else(|| env::panic_str("no 1.0.0 state to migrate"));
        Self {
            owner_id: old.owner_id,
            sponsor_id,
            bonuz_of: old.bonuz_of,
            account_of: old.account_of,
            sponsor_of: LookupMap::new(StorageKey::SponsorOf),
            signed_counter: LookupMap::new(StorageKey::SignedCounter),
        }
    }

    /// Links the calling NEAR account to `bonuz_id`, replacing any earlier
    /// link of this account. Refuses a handle already linked to another
    /// account. Attach enough NEAR for storage (see `link_deposit`); any
    /// excess is refunded in the same call. A deposit of at least 1 yoctoNEAR
    /// is always required, so only a full-access key can change a link.
    #[payable]
    pub fn link(&mut self, bonuz_id: String) {
        assert_valid_bonuz_id(&bonuz_id);
        let account_id = env::predecessor_account_id();
        let attached = env::attached_deposit();
        require!(
            !attached.is_zero(),
            "attach at least 1 yoctoNEAR (see link_deposit) so the call needs a full-access key"
        );
        self.write_link(account_id.clone(), bonuz_id, account_id, attached, false);
    }

    /// Sponsor only. Links the implicit account `account_id` to `bonuz_id`
    /// with a NEP-413 signature from that account's key over
    /// `signed_link_message(account_id, bonuz_id)`, recipient = this contract,
    /// no callback URL. The sponsor attaches the storage deposit (see
    /// `link_deposit`) and gets the excess back. A handle held by another
    /// account is taken over, since the sponsor vouches for the handle.
    #[payable]
    pub fn link_signed(
        &mut self,
        account_id: AccountId,
        bonuz_id: String,
        nonce: Base64VecU8,
        signature: Base64VecU8,
    ) {
        let sponsor = self.assert_sponsor();
        assert_valid_bonuz_id(&bonuz_id);
        let counter = self.signed_counter_of(&account_id);
        self.verify_nep413(&account_id, &link_message(&bonuz_id, counter), &nonce.0, &signature.0);
        let remaining = self.bump_counter_paid(&account_id, counter, env::attached_deposit());
        self.write_link(account_id, bonuz_id, sponsor, remaining, true);
    }

    /// Removes the caller's link and refunds the storage to whoever paid it.
    /// Attach exactly 1 yoctoNEAR (full-access key). Panics if there is no link.
    #[payable]
    pub fn unlink(&mut self) {
        assert_one_yocto();
        self.remove_link(&env::predecessor_account_id(), None);
    }

    /// Sponsor only. Removes the link of the implicit account `account_id`
    /// with a NEP-413 signature over `signed_unlink_message(account_id)`.
    /// The sponsor attaches a deposit for the signed-call counter if this
    /// account has none yet (`link_deposit` always covers it); excess is refunded.
    #[payable]
    pub fn unlink_signed(&mut self, account_id: AccountId, nonce: Base64VecU8, signature: Base64VecU8) {
        let sponsor = self.assert_sponsor();
        require!(
            self.bonuz_of.contains_key(&account_id),
            "this NEAR account has no bonuz ID linked"
        );
        let counter = self.signed_counter_of(&account_id);
        self.verify_nep413(&account_id, &unlink_message(counter), &nonce.0, &signature.0);
        let remaining = self.bump_counter_paid(&account_id, counter, env::attached_deposit());
        refund(&sponsor, remaining);
        self.remove_link(&account_id, None);
    }

    /// Owner only: removes the link of `bonuz_id` (for example a squatted
    /// handle) and refunds its storage to whoever paid it. Attach exactly 1 yoctoNEAR.
    #[payable]
    pub fn admin_unlink(&mut self, bonuz_id: String) {
        assert_one_yocto();
        require!(
            env::predecessor_account_id() == self.owner_id,
            "only the registry owner can call admin_unlink"
        );
        let account_id = self
            .account_of
            .get(&bonuz_id)
            .cloned()
            .unwrap_or_else(|| env::panic_str("this bonuz ID has no NEAR account linked"));
        self.remove_link(&account_id, Some(self.owner_id.clone()));
    }

    /// Owner only: sets (or clears, with `null`) the sponsor account. Attach exactly 1 yoctoNEAR.
    #[payable]
    pub fn set_sponsor(&mut self, sponsor_id: Option<AccountId>) {
        assert_one_yocto();
        require!(
            env::predecessor_account_id() == self.owner_id,
            "only the registry owner can call set_sponsor"
        );
        self.sponsor_id = sponsor_id;
    }

    /// The bonuz ID linked to `account_id`, if any.
    pub fn bonuz_of(&self, account_id: AccountId) -> Option<String> {
        self.bonuz_of.get(&account_id).cloned()
    }

    /// The NEAR account linked to `bonuz_id`, if any.
    pub fn account_of(&self, bonuz_id: String) -> Option<AccountId> {
        self.account_of.get(&bonuz_id).cloned()
    }

    /// A deposit that always covers one `link` or `link_signed`. The unused part is refunded.
    pub fn link_deposit(&self) -> NearToken {
        storage_cost(MAX_LINK_BYTES)
    }

    /// The exact NEP-413 `message` text to sign for `link_signed` right now.
    pub fn signed_link_message(&self, account_id: AccountId, bonuz_id: String) -> String {
        link_message(&bonuz_id, self.signed_counter_of(&account_id))
    }

    /// The exact NEP-413 `message` text to sign for `unlink_signed` right now.
    pub fn signed_unlink_message(&self, account_id: AccountId) -> String {
        unlink_message(self.signed_counter_of(&account_id))
    }

    /// Who paid the storage of `account_id`'s link: the account itself or the sponsor.
    pub fn payer_of(&self, account_id: AccountId) -> Option<AccountId> {
        self.bonuz_of
            .contains_key(&account_id)
            .then(|| self.payer(&account_id))
    }

    /// The account allowed to call `admin_unlink` and `set_sponsor`.
    pub fn owner(&self) -> AccountId {
        self.owner_id.clone()
    }

    /// The account allowed to submit signed calls, if any.
    pub fn sponsor(&self) -> Option<AccountId> {
        self.sponsor_id.clone()
    }
}

impl Registry {
    fn flush(&mut self) {
        self.bonuz_of.flush();
        self.account_of.flush();
        self.sponsor_of.flush();
        self.signed_counter.flush();
    }

    fn payer(&self, account_id: &AccountId) -> AccountId {
        self.sponsor_of.get(account_id).cloned().unwrap_or_else(|| account_id.clone())
    }

    fn signed_counter_of(&self, account_id: &AccountId) -> u32 {
        self.signed_counter.get(account_id).copied().unwrap_or(0)
    }

    fn assert_sponsor(&self) -> AccountId {
        let caller = env::predecessor_account_id();
        require!(
            self.sponsor_id.as_ref() == Some(&caller),
            "only the bonuz sponsor account can submit signed calls"
        );
        caller
    }

    /// Panics unless `signature` is a valid NEP-413 signature by the key of
    /// the implicit account `account_id` over `message`, for this contract.
    fn verify_nep413(&self, account_id: &AccountId, message: &str, nonce: &[u8], signature: &[u8]) {
        let public_key = implicit_public_key(account_id.as_str()).unwrap_or_else(|| {
            env::panic_str("signed calls only work for implicit NEAR accounts (64 hex characters)")
        });
        let nonce: [u8; 32] = nonce
            .try_into()
            .unwrap_or_else(|_| env::panic_str("nonce must be 32 bytes"));
        let signature: [u8; 64] = signature
            .try_into()
            .unwrap_or_else(|_| env::panic_str("signature must be 64 bytes"));
        let payload = Nep413Payload {
            message: message.to_owned(),
            nonce,
            recipient: env::current_account_id().to_string(),
            callback_url: None,
        };
        let mut data = NEP413_TAG.to_le_bytes().to_vec();
        data.extend(borsh::to_vec(&payload).unwrap_or_else(|_| env::panic_str("borsh")));
        require!(
            env::ed25519_verify(&signature, env::sha256_array(&data), &public_key),
            "invalid signature: sign signed_link_message / signed_unlink_message with NEP-413, recipient = this contract"
        );
    }

    /// Increments the signed-call counter and charges its storage to `attached`.
    /// Returns what is left of `attached`.
    fn bump_counter_paid(&mut self, account_id: &AccountId, counter: u32, attached: NearToken) -> NearToken {
        let before = env::storage_usage();
        self.signed_counter.insert(account_id.clone(), counter + 1);
        self.flush();
        let cost = storage_cost(env::storage_usage().saturating_sub(before));
        if attached < cost {
            env::panic_str(&format!(
                "attach at least {} yoctoNEAR for storage, got {}",
                cost.as_yoctonear(),
                attached.as_yoctonear()
            ));
        }
        attached.saturating_sub(cost)
    }

    /// Deletes the account's records and returns its handle and payer.
    /// Does not refund or emit.
    fn take_link(&mut self, account_id: &AccountId) -> Option<(String, AccountId)> {
        let bonuz_id = self.bonuz_of.remove(account_id)?;
        let payer = self.sponsor_of.remove(account_id).unwrap_or_else(|| account_id.clone());
        self.account_of.remove(&bonuz_id);
        Some((bonuz_id, payer))
    }

    /// Deletes a link, refunds the freed storage to its payer, emits `unlink`.
    fn remove_link(&mut self, account_id: &AccountId, removed_by: Option<AccountId>) {
        let before = env::storage_usage();
        let (bonuz_id, payer) = self
            .take_link(account_id)
            .unwrap_or_else(|| env::panic_str("this NEAR account has no bonuz ID linked"));
        self.flush();
        refund(&payer, storage_cost(before.saturating_sub(env::storage_usage())));
        RegistryEvent::Unlink { account_id: account_id.clone(), bonuz_id, removed_by }.emit();
    }

    /// Writes `account_id -> bonuz_id` paid by `payer` out of `attached`.
    /// `sponsored` lets the write take over a handle held by another account.
    fn write_link(
        &mut self,
        account_id: AccountId,
        bonuz_id: String,
        payer: AccountId,
        attached: NearToken,
        sponsored: bool,
    ) {
        if let Some(holder) = self.account_of.get(&bonuz_id).cloned() {
            if holder == account_id {
                // Same link again: nothing to change, give the deposit back.
                refund(&payer, attached);
                return;
            }
            if !sponsored {
                env::panic_str("this bonuz ID is already linked to another NEAR account");
            }
            self.remove_link(&holder, Some(payer.clone()));
        }

        // Relink: remove the old link first, refunding whoever paid for it.
        let previous_bonuz_id = if self.bonuz_of.contains_key(&account_id) {
            let before = env::storage_usage();
            let (old, old_payer) = self.take_link(&account_id).unwrap_or_else(|| env::abort());
            self.flush();
            refund(&old_payer, storage_cost(before.saturating_sub(env::storage_usage())));
            Some(old)
        } else {
            None
        };

        let before = env::storage_usage();
        self.bonuz_of.insert(account_id.clone(), bonuz_id.clone());
        self.account_of.insert(bonuz_id.clone(), account_id.clone());
        if payer != account_id {
            self.sponsor_of.insert(account_id.clone(), payer.clone());
        }
        // `store` collections write on flush; flush now so storage_usage is real.
        self.flush();
        let cost = storage_cost(env::storage_usage().saturating_sub(before));
        if attached < cost {
            env::panic_str(&format!(
                "attach at least {} yoctoNEAR for storage, got {}",
                cost.as_yoctonear(),
                attached.as_yoctonear()
            ));
        }
        refund(&payer, attached.saturating_sub(cost));

        RegistryEvent::Link {
            account_id,
            bonuz_id,
            previous_bonuz_id,
            sponsored_by: sponsored.then_some(payer),
        }
        .emit();
    }
}

fn storage_cost(bytes: StorageUsage) -> NearToken {
    env::storage_byte_cost().saturating_mul(u128::from(bytes))
}

fn refund(to: &AccountId, amount: NearToken) {
    if !amount.is_zero() {
        Promise::new(to.clone()).transfer(amount).detach();
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn accepts_valid_ids() {
        for id in ["ab", "mende", "a_b_1", &"x".repeat(32)] {
            assert_valid_bonuz_id(id);
        }
    }

    #[test]
    #[should_panic(expected = "2 to 32")]
    fn refuses_one_char() {
        assert_valid_bonuz_id("a");
    }

    #[test]
    #[should_panic(expected = "2 to 32")]
    fn refuses_33_chars() {
        assert_valid_bonuz_id(&"x".repeat(33));
    }

    #[test]
    #[should_panic(expected = "lowercase")]
    fn refuses_uppercase() {
        assert_valid_bonuz_id("Mende");
    }

    #[test]
    #[should_panic(expected = "lowercase")]
    fn refuses_punctuation() {
        assert_valid_bonuz_id("mende.near");
    }

    #[test]
    fn implicit_key_decodes_hex_only() {
        let hex = "0f".repeat(32);
        assert_eq!(implicit_public_key(&hex), Some([0x0f; 32]));
        assert_eq!(implicit_public_key("alice.near"), None);
        assert_eq!(implicit_public_key(&"0F".repeat(32)), None, "uppercase hex is not a NEAR account");
        assert_eq!(implicit_public_key(&"0f".repeat(31)), None);
    }

    #[test]
    fn messages_carry_the_counter() {
        assert_eq!(link_message("mende", 0), "Link this NEAR account to bonuz ID mende (#0)");
        assert_eq!(unlink_message(3), "Unlink this NEAR account from its bonuz ID (#3)");
    }
}
