//! bonuz ID registry on NEAR.
//!
//! Links a NEAR account to a bonuz ID handle (the profile at `bonuz.id/<handle>`).
//! The NEAR account proves ownership simply by calling `link` itself:
//! `predecessor_account_id` is the account that signed (or, for a meta
//! transaction, the account that signed the delegate action).
//!
//! One NEAR account maps to at most one bonuz ID and one bonuz ID maps to at
//! most one NEAR account. Callers pay for the storage their entry uses and get
//! it back when they unlink or relink to a shorter handle.

use near_sdk::store::LookupMap;
use near_sdk::{env, near, AccountId, BorshStorageKey, NearToken, PanicOnDefault, Promise, StorageUsage};

/// Shortest allowed bonuz ID handle.
pub const MIN_LEN: usize = 2;
/// Longest allowed bonuz ID handle.
pub const MAX_LEN: usize = 32;

/// Upper bound of storage bytes one link can use: two map records with the
/// longest account id (64) and the longest handle (32), including NEAR's
/// 40 byte per-record overhead, rounded up.
const MAX_LINK_BYTES: StorageUsage = 320;

#[near]
#[derive(BorshStorageKey)]
enum StorageKey {
    BonuzOf,
    AccountOf,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Registry {
    /// NEAR account -> bonuz ID handle.
    bonuz_of: LookupMap<AccountId, String>,
    /// bonuz ID handle -> NEAR account.
    account_of: LookupMap<String, AccountId>,
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
    },
    #[event_version("1.0.0")]
    Unlink { account_id: AccountId, bonuz_id: String },
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

#[near]
impl Registry {
    #[init]
    pub fn new() -> Self {
        Self {
            bonuz_of: LookupMap::new(StorageKey::BonuzOf),
            account_of: LookupMap::new(StorageKey::AccountOf),
        }
    }

    /// Links the calling NEAR account to `bonuz_id`, replacing any earlier
    /// link of this account. Refuses a handle already linked to another
    /// account. Attach enough NEAR for storage (see `link_deposit`); any
    /// excess is refunded in the same call.
    #[payable]
    pub fn link(&mut self, bonuz_id: String) {
        assert_valid_bonuz_id(&bonuz_id);
        let account_id = env::predecessor_account_id();
        let attached = env::attached_deposit();

        if let Some(owner) = self.account_of.get(&bonuz_id) {
            if owner != &account_id {
                env::panic_str("this bonuz ID is already linked to another NEAR account");
            }
            // Same link again: nothing to change, give the deposit back.
            refund(&account_id, attached);
            return;
        }

        let before = env::storage_usage();
        let previous_bonuz_id = self.bonuz_of.insert(account_id.clone(), bonuz_id.clone());
        if let Some(old) = &previous_bonuz_id {
            self.account_of.remove(old);
        }
        self.account_of.insert(bonuz_id.clone(), account_id.clone());
        // `store` collections write on flush; flush now so storage_usage is real.
        self.bonuz_of.flush();
        self.account_of.flush();
        let after = env::storage_usage();

        let refund_amount = if after >= before {
            let cost = storage_cost(after - before);
            if attached < cost {
                env::panic_str(&format!(
                    "attach at least {} yoctoNEAR for storage, got {}",
                    cost.as_yoctonear(),
                    attached.as_yoctonear()
                ));
            }
            attached.saturating_sub(cost)
        } else {
            // Relinked to a shorter handle: return the freed storage too.
            attached.saturating_add(storage_cost(before - after))
        };
        refund(&account_id, refund_amount);

        RegistryEvent::Link { account_id, bonuz_id, previous_bonuz_id }.emit();
    }

    /// Removes the caller's link and refunds the storage it used.
    /// Panics if the caller has no link.
    pub fn unlink(&mut self) {
        let account_id = env::predecessor_account_id();
        let before = env::storage_usage();
        let bonuz_id = self
            .bonuz_of
            .remove(&account_id)
            .unwrap_or_else(|| env::panic_str("this NEAR account has no bonuz ID linked"));
        self.account_of.remove(&bonuz_id);
        self.bonuz_of.flush();
        self.account_of.flush();
        let freed = before.saturating_sub(env::storage_usage());
        refund(&account_id, storage_cost(freed));

        RegistryEvent::Unlink { account_id, bonuz_id }.emit();
    }

    /// The bonuz ID linked to `account_id`, if any.
    pub fn bonuz_of(&self, account_id: AccountId) -> Option<String> {
        self.bonuz_of.get(&account_id).cloned()
    }

    /// The NEAR account linked to `bonuz_id`, if any.
    pub fn account_of(&self, bonuz_id: String) -> Option<AccountId> {
        self.account_of.get(&bonuz_id).cloned()
    }

    /// A deposit that always covers one `link` call. The unused part is refunded.
    pub fn link_deposit(&self) -> NearToken {
        storage_cost(MAX_LINK_BYTES)
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
}
