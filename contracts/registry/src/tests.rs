//! Unit tests with exact block times. The sandbox tests in `tests/` run the
//! real wasm; these pin every rule to the second.

use super::*;
use ed25519_dalek::{Signer, SigningKey};
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::testing_env;
use sha2::{Digest, Sha256};

const CONTRACT: &str = "id.bonuz.testnet";
const REGISTRAR: &str = "registrar.bonuz.testnet";
const OWNER: &str = "bonuz.testnet";
const T0: u64 = 1_763_000_000; // unix seconds
const SEC: u64 = 1_000_000_000;

struct Wallet {
    key: SigningKey,
}

impl Wallet {
    fn new(seed: u8) -> Self {
        Self { key: SigningKey::from_bytes(&[seed; 32]) }
    }
    fn account(&self) -> String {
        hex::encode(self.key.verifying_key().to_bytes())
    }
    fn public_key(&self) -> String {
        format!("ed25519:{}", bs58::encode(self.key.verifying_key().to_bytes()).into_string())
    }
    fn sign(&self, message: &str, nonce: [u8; 32], recipient: &str) -> Vec<u8> {
        let hash = Sha256::digest(proof::nep413_preimage(message, &nonce, recipient));
        self.key.sign(&hash).to_bytes().to_vec()
    }
}

struct Call {
    account_id: String,
    bonuz_id: String,
    public_key: String,
    nonce: [u8; 32],
    signature: Vec<u8>,
    message: String,
}

fn signed(w: &Wallet, handle: &str, nonce: u8, expires: u64) -> Call {
    signed_with(w, &w.account(), handle, "testnet", nonce, expires, CONTRACT)
}

/// Signed for an explicit creation wallet instead of `eoa(handle)`.
fn signed_for(w: &Wallet, handle: &str, wallet: &str, nonce: u8, expires: u64) -> Call {
    let mut c = signed(w, handle, nonce, expires);
    c.message = build_wallet_link_message(&w.account(), handle, "testnet", expires, wallet);
    c.signature = w.sign(&c.message, c.nonce, CONTRACT);
    c
}

/// `link` with an explicit `bonuz_eoa` argument (or none).
fn link_as(r: &mut Registry, c: &Call, bonuz_eoa: Option<String>) -> U64 {
    r.link(
        c.account_id.parse().unwrap(),
        c.bonuz_id.clone(),
        c.public_key.clone(),
        Base64VecU8(c.nonce.to_vec()),
        Base64VecU8(c.signature.clone()),
        c.message.clone(),
        bonuz_eoa,
    )
}

fn signed_with(
    w: &Wallet,
    account: &str,
    handle: &str,
    network: &str,
    nonce: u8,
    expires: u64,
    recipient: &str,
) -> Call {
    let message = build_wallet_link_message(account, handle, network, expires, &eoa(handle));
    let nonce = [nonce; 32];
    Call {
        account_id: account.into(),
        bonuz_id: handle.into(),
        public_key: w.public_key(),
        signature: w.sign(&message, nonce, recipient),
        nonce,
        message,
    }
}

fn ctx(caller: &str, now_s: u64) {
    ctx_ns(caller, now_s * SEC)
}

fn ctx_ns(caller: &str, now_ns: u64) {
    testing_env!(VMContextBuilder::new()
        .current_account_id(CONTRACT.parse().unwrap())
        .predecessor_account_id(caller.parse().unwrap())
        .signer_account_id(caller.parse().unwrap())
        .block_timestamp(now_ns)
        .account_balance(NearToken::from_near(5))
        .build());
}

fn ctx_yocto(caller: &str, now_s: u64) {
    testing_env!(VMContextBuilder::new()
        .current_account_id(CONTRACT.parse().unwrap())
        .predecessor_account_id(caller.parse().unwrap())
        .block_timestamp(now_s * SEC)
        .account_balance(NearToken::from_near(5))
        .attached_deposit(NearToken::from_yoctonear(1))
        .build());
}

fn registry(network: &str, allow_named: bool) -> Registry {
    ctx(CONTRACT, T0);
    Registry::new(
        OWNER.parse().unwrap(),
        REGISTRAR.parse().unwrap(),
        network.into(),
        allow_named,
        NearToken::from_near(1),
    )
}

fn link(r: &mut Registry, c: &Call) -> U64 {
    r.link(
        c.account_id.parse().unwrap(),
        c.bonuz_id.clone(),
        c.public_key.clone(),
        Base64VecU8(c.nonce.to_vec()),
        Base64VecU8(c.signature.clone()),
        c.message.clone(),
        Some(eoa(&c.bonuz_id)),
    )
}

/// A stable test creation wallet per handle.
fn eoa(handle: &str) -> String {
    format!("0x{}", &hex::encode(Sha256::digest(handle.as_bytes()))[..40])
}

fn link_err(r: &mut Registry, c: &Call) -> String {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        link(r, c);
    }));
    match res {
        Ok(()) => panic!("link was accepted"),
        Err(e) => e
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default(),
    }
}

fn events() -> Vec<serde_json::Value> {
    near_sdk::test_utils::get_logs()
        .iter()
        .filter_map(|l| l.strip_prefix("EVENT_JSON:"))
        .map(|j| serde_json::from_str(j).unwrap())
        .collect()
}

/// The link and unlink events of the last call (label events left out).
fn link_events() -> Vec<serde_json::Value> {
    events().into_iter().filter(|e| e["event"] == "link" || e["event"] == "unlink").collect()
}

// ---------- proofs ----------

#[test]
fn valid_link_writes_both_maps_and_one_event() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&w, "mende", 1, T0 + 480));
    let acc: AccountId = w.account().parse().unwrap();
    assert_eq!(r.bonuz_of(acc.clone()).as_deref(), Some("mende"));
    assert_eq!(r.account_of("mende".into()), Some(acc.clone()));
    let l = r.link_of(acc).unwrap();
    assert_eq!(l.checked_by, "contract");
    assert_eq!(l.public_key, Some(w.public_key()));
    assert_eq!(l.bonuz_eoa, Some(eoa("mende")));
    let ev = link_events();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["event"], "link");
    assert_eq!(ev[0]["version"], "2.2.0");
    assert_eq!(ev[0]["data"]["checked_by"], "contract");
    assert_eq!(ev[0]["data"]["bonuz_eoa"], eoa("mende"));
}

#[test]
fn wrong_signature_refused() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    let mut c = signed(&w, "mende", 1, T0 + 480);
    c.signature[0] ^= 1;
    ctx(REGISTRAR, T0);
    assert!(link_err(&mut r, &c).contains("invalid signature"));
}

#[test]
fn wrong_recipient_refused() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    let c = signed_with(&w, &w.account(), "mende", "testnet", 1, T0 + 480, "other.testnet");
    ctx(REGISTRAR, T0);
    assert!(link_err(&mut r, &c).contains("invalid signature"));
}

#[test]
fn wrong_network_refused() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    let c = signed_with(&w, &w.account(), "mende", "mainnet", 1, T0 + 480, CONTRACT);
    ctx(REGISTRAR, T0);
    assert!(link_err(&mut r, &c).contains("different network"));
}

#[test]
fn testnet_message_replayed_on_mainnet_contract_with_same_id_refused() {
    let w = Wallet::new(1);
    let c = signed(&w, "mende", 1, T0 + 480);
    let mut testnet = registry("testnet", true);
    ctx(REGISTRAR, T0);
    link(&mut testnet, &c);
    let mut mainnet = registry("mainnet", false);
    ctx(REGISTRAR, T0);
    assert!(link_err(&mut mainnet, &c).contains("different network"));
}

#[test]
fn message_naming_another_account_or_handle_refused() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    let other = Wallet::new(2);

    // Signed for "mende", submitted for the prefix "men".
    let mut c = signed(&w, "mende", 1, T0 + 480);
    c.bonuz_id = "men".into();
    ctx(REGISTRAR, T0);
    assert!(link_err(&mut r, &c).contains("different bonuz ID"));

    // Signed naming another account, submitted for the signer's own account.
    let mut c = signed_with(&w, &other.account(), "mende", "testnet", 2, T0 + 480, CONTRACT);
    c.account_id = w.account();
    ctx(REGISTRAR, T0);
    assert!(link_err(&mut r, &c).contains("different NEAR account"));
}

#[test]
fn only_the_registrar_can_link() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    ctx(OWNER, T0);
    assert!(link_err(&mut r, &signed(&w, "mende", 1, T0 + 480)).contains("only the bonuz registrar"));
    ctx(&w.account(), T0);
    assert!(link_err(&mut r, &signed(&w, "mende", 1, T0 + 480)).contains("only the bonuz registrar"));
}

#[test]
fn implicit_account_with_another_key_refused() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    let victim = Wallet::new(2);
    // w signs a message naming victim's implicit account with w's own key.
    let c = signed_with(&w, &victim.account(), "mende", "testnet", 1, T0 + 480, CONTRACT);
    ctx(REGISTRAR, T0);
    assert!(link_err(&mut r, &c).contains("does not belong to this implicit account"));
}

#[test]
fn eth_implicit_account_refused() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    let eth = "0x1234567890abcdef1234567890abcdef12345678";
    let c = signed_with(&w, eth, "mende", "testnet", 1, T0 + 480, CONTRACT);
    ctx(REGISTRAR, T0);
    assert!(link_err(&mut r, &c).contains("Ethereum-style"));
}

#[test]
fn named_account_recorded_as_registrar_checked_on_testnet_only() {
    let w = Wallet::new(1);
    let c = signed_with(&w, "alice.testnet", "mende", "testnet", 1, T0 + 480, CONTRACT);

    let mut open = registry("testnet", true);
    ctx(REGISTRAR, T0);
    link(&mut open, &c);
    assert_eq!(open.link_of("alice.testnet".parse().unwrap()).unwrap().checked_by, "registrar");

    let mut closed = registry("testnet", false);
    ctx(REGISTRAR, T0);
    assert!(link_err(&mut closed, &c).contains("only links implicit accounts"));
}

// ---------- time and nonces ----------

#[test]
fn expired_message_refused() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    let c = signed(&w, "mende", 1, T0 + 480);
    ctx(REGISTRAR, T0 + 480);
    assert!(link_err(&mut r, &c).contains("expired"));
    ctx(REGISTRAR, T0 + 481);
    assert!(link_err(&mut r, &c).contains("expired"));
}

#[test]
fn expiry_exactly_ten_minutes_ahead_accepted_one_second_more_refused() {
    let w = Wallet::new(1);
    let mut r = registry("testnet", true);
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&w, "mende", 1, T0 + 600));

    let mut r = registry("testnet", true);
    ctx(REGISTRAR, T0);
    assert!(link_err(&mut r, &signed(&w, "mende", 2, T0 + 601)).contains("more than 10 minutes"));
}

#[test]
fn backend_clock_90_seconds_ahead_still_accepted() {
    // The backend sets Expires = its now + 8 min. Its clock is 90 s ahead of the chain.
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&w, "mende", 1, T0 + 90 + 480));
}

#[test]
fn reused_nonce_refused() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&w, "mende", 7, T0 + 480));
    ctx(REGISTRAR, T0 + 1);
    // Same nonce, different handle, fresh signature.
    assert!(link_err(&mut r, &signed(&w, "other", 7, T0 + 480)).contains("nonce was already used"));
}

#[test]
fn pruning_deletes_only_expired_nonces() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    for (i, handle) in ["a1", "a2", "a3"].iter().enumerate() {
        ctx(REGISTRAR, T0);
        link(&mut r, &signed(&w, handle, i as u8 + 1, T0 + 100 + i as u64 * 100));
    }
    assert_eq!(r.status().stored_nonces, 3);
    ctx("anyone.testnet", T0 + 250);
    assert_eq!(r.prune_nonces(50), 2);
    assert_eq!(r.status().stored_nonces, 1);
    ctx("anyone.testnet", T0 + 10_000);
    assert_eq!(r.prune_nonces(500), 1, "limit is capped but still prunes");
}

#[test]
fn pruned_nonce_replayed_after_expiry_refused_as_expired() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    let c = signed(&w, "mende", 1, T0 + 480);
    ctx(REGISTRAR, T0);
    link(&mut r, &c);
    ctx("anyone.testnet", T0 + 480);
    assert_eq!(r.prune_nonces(50), 1);
    ctx(REGISTRAR, T0 + 480);
    assert!(link_err(&mut r, &c).contains("expired"));
}

#[test]
fn prune_and_retry_in_the_same_block_at_expiry() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    let c = signed(&w, "mende", 1, T0 + 480);
    ctx(REGISTRAR, T0 + 10);
    link(&mut r, &c);
    // One block at exactly expires_at: prune runs, then the retry.
    ctx("anyone.testnet", T0 + 480);
    assert_eq!(r.prune_nonces(50), 1);
    ctx(REGISTRAR, T0 + 480);
    assert!(link_err(&mut r, &c).contains("expired"), "same helper decides both");
    // One nanosecond earlier the nonce is kept and the retry is refused as reused.
    let mut r = registry("testnet", true);
    ctx(REGISTRAR, T0 + 10);
    link(&mut r, &c);
    ctx_ns("anyone.testnet", (T0 + 480) * SEC - 1);
    assert_eq!(r.prune_nonces(50), 0);
    ctx_ns(REGISTRAR, (T0 + 480) * SEC - 1);
    assert!(link_err(&mut r, &c).contains("nonce was already used"));
}

// ---------- relinks ----------

#[test]
fn account_side_relink_replaces_the_old_pair() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&w, "old", 1, T0 + 480));
    ctx(REGISTRAR, T0 + 5);
    link(&mut r, &signed(&w, "new", 2, T0 + 480));
    let acc: AccountId = w.account().parse().unwrap();
    assert_eq!(r.bonuz_of(acc.clone()).as_deref(), Some("new"));
    assert_eq!(r.account_of("old".into()), None);
    assert_eq!(r.account_of("new".into()), Some(acc));
    let ev = link_events();
    assert_eq!(ev[0]["event"], "unlink");
    assert_eq!(ev[0]["data"]["reason"], "replaced");
    assert_eq!(ev[0]["data"]["bonuz_id"], "old");
    assert_eq!(ev[1]["event"], "link");
    assert_eq!(r.status().unlinks_today, 0, "replaced does not count toward the cap");
}

#[test]
fn handle_side_relink_refused_until_registrar_unlinks() {
    let mut r = registry("testnet", true);
    let a = Wallet::new(1);
    let b = Wallet::new(2);
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&a, "mende", 1, T0 + 480));
    ctx(REGISTRAR, T0 + 1);
    assert!(link_err(&mut r, &signed(&b, "mende", 2, T0 + 480)).contains("unlink it first"));
    ctx(REGISTRAR, T0 + 2);
    assert!(r.unlink_by_registrar(a.account().parse().unwrap()));
    ctx(REGISTRAR, T0 + 3);
    link(&mut r, &signed(&b, "mende", 3, T0 + 480));
    assert_eq!(r.account_of("mende".into()), Some(b.account().parse().unwrap()));
    assert_eq!(r.bonuz_of(a.account().parse().unwrap()), None);
}

#[test]
fn same_key_two_handles_quickly_keeps_reverse_map_clean() {
    let mut r = registry("testnet", true);
    let a = Wallet::new(1);
    let b = Wallet::new(2);
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&b, "taken", 9, T0 + 480));
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&a, "first", 1, T0 + 480));
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&a, "second", 2, T0 + 480));
    assert_eq!(r.account_of("first".into()), None);
    // A failing third link (handle taken) writes nothing, not even its nonce.
    ctx(REGISTRAR, T0);
    assert!(link_err(&mut r, &signed(&a, "taken", 3, T0 + 480)).contains("unlink it first"));
    assert_eq!(r.account_of("second".into()), Some(a.account().parse().unwrap()));
    assert_eq!(r.account_of("taken".into()), Some(b.account().parse().unwrap()));
    assert_eq!(r.bonuz_of(a.account().parse().unwrap()).as_deref(), Some("second"));
    assert_eq!(r.status().stored_nonces, 3);
}

#[test]
fn registrar_unlink_is_idempotent_and_owner_unlink_needs_the_account() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&w, "mende", 1, T0 + 480));
    ctx_yocto(&w.account(), T0 + 1);
    r.unlink_by_owner();
    assert_eq!(events()[0]["data"]["reason"], "owner");
    assert_eq!(r.account_of("mende".into()), None);
    ctx(REGISTRAR, T0 + 2);
    assert!(!r.unlink_by_registrar(w.account().parse().unwrap()));
    assert_eq!(r.status().unlinks_today, 0, "a no-op uses no cap");
}

// ---------- limits ----------

fn fill(r: &mut Registry, n: u32, start: u8) -> Vec<Wallet> {
    (0..n)
        .map(|i| {
            let w = Wallet::new(start.wrapping_add(i as u8));
            ctx(REGISTRAR, T0);
            link(r, &signed(&w, &format!("h{start}_{i}"), start.wrapping_add(i as u8), T0 + 480));
            w
        })
        .collect()
}

#[test]
fn registrar_unlink_over_the_daily_cap_refused() {
    let mut r = registry("testnet", true);
    ctx_yocto(OWNER, T0);
    r.set_unlink_daily_cap(2, U64(T0 + 3600));
    let ws = fill(&mut r, 3, 10);
    ctx(REGISTRAR, T0 + 1);
    assert!(r.unlink_by_registrar(ws[0].account().parse().unwrap()));
    assert!(r.unlink_by_registrar(ws[1].account().parse().unwrap()));
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        r.unlink_by_registrar(ws[2].account().parse().unwrap());
    }));
    assert!(res.is_err(), "third unlink must hit the cap");
    // Next day the count resets.
    ctx(REGISTRAR, T0 + 86_400);
    assert!(r.unlink_by_registrar(ws[2].account().parse().unwrap()));
}

#[test]
fn raised_cap_falls_back_to_100_after_until() {
    let mut r = registry("testnet", true);
    assert_eq!(r.status().unlink_daily_cap, 100);
    ctx_yocto(OWNER, T0);
    r.set_unlink_daily_cap(1000, U64(T0 + 3600));
    ctx("anyone.testnet", T0 + 3599);
    assert_eq!(r.status().unlink_daily_cap, 1000);
    ctx("anyone.testnet", T0 + 3600);
    assert_eq!(r.status().unlink_daily_cap, 100);
}

#[test]
fn balance_under_reserve_refused() {
    let mut r = registry("testnet", true);
    ctx_yocto(OWNER, T0);
    r.set_reserve(NearToken::from_near(10));
    let w = Wallet::new(1);
    ctx(REGISTRAR, T0);
    assert!(link_err(&mut r, &signed(&w, "mende", 1, T0 + 480)).contains("under its reserve"));
}

#[test]
fn paused_refuses_link_and_registrar_unlink_but_not_owner_unlink() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&w, "mende", 1, T0 + 480));
    ctx_yocto(OWNER, T0);
    r.set_paused(true);
    ctx(REGISTRAR, T0);
    assert!(link_err(&mut r, &signed(&Wallet::new(2), "x2", 2, T0 + 480)).contains("paused"));
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        r.unlink_by_registrar(w.account().parse().unwrap());
    }));
    assert!(res.is_err());
    ctx_yocto(&w.account(), T0);
    r.unlink_by_owner();
}

#[test]
fn owner_methods_need_the_owner_and_one_yocto() {
    let mut r = registry("testnet", true);
    ctx_yocto(REGISTRAR, T0);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.set_paused(true))).is_err());
    ctx(OWNER, T0);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.set_paused(true))).is_err());
    ctx_yocto(OWNER, T0);
    r.set_registrar("new.testnet".parse().unwrap());
    assert_eq!(r.status().registrar.as_str(), "new.testnet");
}

// ---------- parser property ----------

#[test]
fn parser_accepts_only_the_canonical_text() {
    let three = build_link_message(&Wallet::new(1).account(), "mende", "testnet", T0 + 480);
    let four = build_wallet_link_message(&Wallet::new(1).account(), "mende", "testnet", T0 + 480, &eoa("mende"));
    parser_property(&three);
    parser_property(&four);
}

fn parser_property(base: &str) {
    let base = base.to_string();
    let line_count = base.split('\n').count();
    let alphabet: Vec<char> = " \n\r\t@.-_:aAzZ09\u{00A0}\u{200B}\u{FF20}\u{0430}\u{2028}".chars().collect();
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut rand = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    let mut accepted_changes = 0;
    for _ in 0..20_000 {
        let mut chars: Vec<char> = base.chars().collect();
        for _ in 0..1 + rand(3) {
            let at = rand(chars.len() + 1);
            match rand(4) {
                0 if at < chars.len() => {
                    chars.remove(at);
                }
                1 => chars.insert(at, alphabet[rand(alphabet.len())]),
                2 if at < chars.len() => chars[at] = alphabet[rand(alphabet.len())],
                _ => {
                    let lines: Vec<&str> = base.split('\n').collect();
                    let mut l = lines.clone();
                    l.swap(rand(line_count), rand(line_count));
                    chars = l.join("\n").chars().collect();
                }
            }
        }
        let m: String = chars.into_iter().collect();
        if m == base {
            continue;
        }
        if let Ok(p) = proof::parse_link_message(&m) {
            // Whatever is accepted must rebuild to itself: no hidden extra text.
            assert_eq!(p.to_text(), m);
            // And it never parses to the original's values.
            let o = proof::parse_link_message(&base).unwrap();
            assert_ne!(p, o, "mutation parsed to the original values: {m:?}");
            accepted_changes += 1;
        }
    }
    // Some mutations are just other valid messages (e.g. another digit in the
    // account); those name different values and fail check_link's argument match.
    assert!(accepted_changes < 20_000);
}

// ---------- 2.1: nonce cleanup under traffic, owner change, link count ----------

/// One link every 30 seconds for 6 hours, each message expiring 5 minutes out.
/// Returns how many nonces are stored at the end. At most ~10 can be unexpired.
fn steady_traffic(random_seeds: bool) -> u32 {
    let mut r = registry("mainnet", false);
    for i in 0u32..720 {
        let now = T0 + u64::from(i) * 30;
        let mut seed = [0u8; 32];
        if random_seeds {
            seed[..4].copy_from_slice(&(i.wrapping_mul(2_654_435_761)).to_le_bytes());
        }
        testing_env!(VMContextBuilder::new()
            .current_account_id(CONTRACT.parse().unwrap())
            .predecessor_account_id(REGISTRAR.parse().unwrap())
            .block_timestamp(now * SEC)
            .random_seed(seed)
            .account_balance(NearToken::from_near(50))
            .build());
        let w = Wallet { key: SigningKey::from_bytes(&{
            let mut k = [7u8; 32];
            k[..4].copy_from_slice(&i.to_le_bytes());
            k
        }) };
        let mut nonce = [9u8; 32];
        nonce[..4].copy_from_slice(&i.to_le_bytes());
        let handle = format!("user{i}");
        let message = build_wallet_link_message(&w.account(), &handle, "mainnet", now + 300, &eoa(&handle));
        link(&mut r, &Call {
            account_id: w.account(),
            bonuz_id: handle,
            public_key: w.public_key(),
            signature: w.sign(&message, nonce, CONTRACT),
            nonce,
            message,
        });
    }
    assert_eq!(r.status().links.0, 720);
    r.status().stored_nonces
}

// Mocked storage outlives testing_env! within one test, so each run gets its own test.
#[test]
fn nonce_cleanup_keeps_up_with_steady_traffic() {
    let stored = steady_traffic(true);
    eprintln!("random start: {stored} nonces stored after 720 links");
    assert!(stored <= 40, "nonces piled up: {stored} stored after 720 links");
}

/// The 2.0 behaviour (always scanning from the first slot) is what a zero seed
/// reproduces: the first slots refill with fresh nonces and cleanup stalls.
#[test]
fn fixed_start_cleanup_would_stall() {
    let stalled = steady_traffic(false);
    eprintln!("fixed start: {stalled} nonces stored after 720 links");
    assert!(stalled > 300, "expected the fixed-start scan to stall, got {stalled}");
}

#[test]
fn owner_change_takes_two_steps() {
    let mut r = registry("mainnet", false);
    ctx_yocto("someone.near", T0);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.propose_owner("x.near".parse().unwrap()))).is_err());
    ctx_yocto(OWNER, T0);
    r.propose_owner("new.near".parse().unwrap());
    assert_eq!(r.status().owner.as_str(), OWNER, "proposing changes nothing yet");
    ctx_yocto("someone.near", T0);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.accept_owner())).is_err());
    ctx("new.near", T0);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.accept_owner())).is_err(), "needs 1 yocto");
    ctx_yocto("new.near", T0);
    r.accept_owner();
    let s = r.status();
    assert_eq!(s.owner.as_str(), "new.near");
    assert!(s.pending_owner.is_none());
    ctx_yocto(OWNER, T0);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.set_paused(true))).is_err(), "old owner is out");
}

#[test]
fn link_count_follows_every_path() {
    let mut r = registry("testnet", true);
    let a = Wallet::new(1);
    let b = Wallet::new(2);
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&a, "one", 1, T0 + 480));
    link(&mut r, &signed(&b, "two", 2, T0 + 480));
    assert_eq!(r.status().links.0, 2);
    link(&mut r, &signed(&a, "three", 3, T0 + 480)); // replace: same account
    assert_eq!(r.status().links.0, 2);
    link(&mut r, &signed(&a, "three", 4, T0 + 480)); // same pair again
    assert_eq!(r.status().links.0, 2);
    assert!(r.unlink_by_registrar(b.account().parse().unwrap()));
    assert!(!r.unlink_by_registrar(b.account().parse().unwrap()));
    assert_eq!(r.status().links.0, 1);
    ctx_yocto(&a.account(), T0);
    r.unlink_by_owner();
    assert_eq!(r.status().links.0, 0);
    assert_eq!(r.account_of("three".into()), None);
}

// ---------- 2.2.0: creation-wallet keys, labels, pending claims ----------

const ALICE: &str = "alice.testnet";
const BOB: &str = "bob.testnet";
const TTL: u64 = LINK_PROPOSAL_TTL_SECONDS;

/// The account itself signs (signer = predecessor), with 1 yoctoNEAR attached.
fn ctx_self_yocto(account: &str, now_s: u64) {
    testing_env!(VMContextBuilder::new()
        .current_account_id(CONTRACT.parse().unwrap())
        .predecessor_account_id(account.parse().unwrap())
        .signer_account_id(account.parse().unwrap())
        .block_timestamp(now_s * SEC)
        .account_balance(NearToken::from_near(5))
        .attached_deposit(NearToken::from_yoctonear(1))
        .build());
}

fn panics<F: FnOnce()>(f: F) -> String {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(()) => panic!("the call was accepted"),
        Err(e) => e
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default(),
    }
}

fn acc(id: &str) -> AccountId {
    id.parse().unwrap()
}

fn propose(r: &mut Registry, account: &str, handle: &str, now_s: u64) -> u64 {
    ctx(REGISTRAR, now_s);
    r.propose_link(acc(account), eoa(handle), handle.into()).0
}

fn accept(r: &mut Registry, account: &str, handle: &str, now_s: u64) {
    ctx_self_yocto(account, now_s);
    r.accept_link(eoa(handle));
}

#[test]
fn propose_then_accept_makes_a_live_link_keyed_to_the_wallet() {
    let mut r = registry("mainnet", false);
    let expires = propose(&mut r, ALICE, "mende", T0);
    assert_eq!(expires, T0 + TTL);
    let ev = events();
    assert_eq!(ev.last().unwrap()["event"], "link_proposed");
    assert_eq!(ev.last().unwrap()["data"]["bonuz_eoa"], eoa("mende"));
    // Pending: visible, but nothing is linked yet.
    let p = r.link_proposal_of(acc(ALICE)).unwrap();
    assert_eq!((p.bonuz_eoa.as_str(), p.bonuz_id.as_deref(), p.expires.0), (eoa("mende").as_str(), Some("mende"), T0 + TTL));
    assert_eq!(r.link_proposal_for(eoa("mende")).unwrap().account_id, acc(ALICE));
    assert_eq!(r.link_proposals(None, None).len(), 1);
    assert_eq!(r.bonuz_of(acc(ALICE)), None);
    assert_eq!(r.account_of("mende".into()), None);
    assert_eq!(r.status().links.0, 0);
    assert_eq!(r.status().stored_link_proposals, 1);

    accept(&mut r, ALICE, "mende", T0 + 60);
    let signer_pk = String::from(&env::signer_account_pk());
    let l = r.link_of(acc(ALICE)).unwrap();
    assert_eq!(l.checked_by, "account");
    assert_eq!(l.public_key, Some(signer_pk));
    assert_eq!(l.bonuz_eoa, Some(eoa("mende")));
    assert_eq!(l.bonuz_id, "mende");
    assert_eq!(r.bonuz_of(acc(ALICE)).as_deref(), Some("mende"));
    assert_eq!(r.account_of("mende".into()), Some(acc(ALICE)));
    assert_eq!(r.account_of_eoa(eoa("mende")), Some(acc(ALICE)));
    assert_eq!(r.eoa_of("mende".into()), Some(eoa("mende")));
    assert_eq!(r.handle_of(eoa("mende")).as_deref(), Some("mende"));
    assert!(r.link_proposal_of(acc(ALICE)).is_none());
    assert_eq!(r.status().links.0, 1);
    assert_eq!(r.status().stored_link_proposals, 0);
    let names: Vec<_> = events().iter().map(|e| e["event"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["link_proposal_closed", "link"]);
    assert_eq!(events()[0]["data"]["reason"], "accepted");
}

#[test]
fn only_the_claimed_account_with_one_yocto_and_the_right_wallet_can_accept() {
    let mut r = registry("mainnet", false);
    propose(&mut r, ALICE, "mende", T0);
    // Someone else has no claim to accept.
    ctx_self_yocto(BOB, T0 + 1);
    assert!(panics(|| r.accept_link(eoa("mende"))).contains("no pending link proposal"));
    // The registrar cannot accept for the account.
    ctx_self_yocto(REGISTRAR, T0 + 1);
    assert!(panics(|| r.accept_link(eoa("mende"))).contains("no pending link proposal"));
    // No deposit: a function-call key cannot accept.
    ctx(ALICE, T0 + 1);
    assert!(panics(|| r.accept_link(eoa("mende"))).contains("1 yoctoNEAR"));
    // The claim names another wallet than the one accepted.
    ctx_self_yocto(ALICE, T0 + 1);
    assert!(panics(|| r.accept_link(eoa("other"))).contains("for another bonuz ID"));
    assert_eq!(r.bonuz_of(acc(ALICE)), None);
    // Only the registrar proposes.
    ctx(ALICE, T0 + 2);
    assert!(panics(|| {
        r.propose_link(acc(ALICE), eoa("x"), "x".into());
    })
    .contains("only the bonuz registrar"));
    accept(&mut r, ALICE, "mende", T0 + 3);
    assert_eq!(r.bonuz_of(acc(ALICE)).as_deref(), Some("mende"));
}

#[test]
fn a_claim_expires_after_seven_days_and_can_be_pruned() {
    let mut r = registry("mainnet", false);
    propose(&mut r, ALICE, "mende", T0);
    // Valid one second before, refused at the expiry second.
    ctx_self_yocto(ALICE, T0 + TTL);
    assert!(panics(|| r.accept_link(eoa("mende"))).contains("expired"));
    assert!(r.link_proposal_of(acc(ALICE)).is_none());
    assert!(r.link_proposal_for(eoa("mende")).is_none());
    assert!(r.link_proposals(None, None).is_empty());
    assert_eq!(r.status().stored_link_proposals, 1, "still stored until pruned");
    ctx(BOB, T0 + TTL);
    assert_eq!(r.prune_link_proposals(10), 1);
    assert_eq!(r.status().stored_link_proposals, 0);
    assert_eq!(events()[0]["data"]["reason"], "expired");
    // A fresh claim works again.
    propose(&mut r, ALICE, "mende", T0 + TTL + 1);
    accept(&mut r, ALICE, "mende", T0 + 2 * TTL);
    assert_eq!(r.account_of("mende".into()), Some(acc(ALICE)));
}

#[test]
fn accept_one_second_before_expiry_works() {
    let mut r = registry("mainnet", false);
    propose(&mut r, ALICE, "mende", T0);
    accept(&mut r, ALICE, "mende", T0 + TTL - 1);
    assert_eq!(r.bonuz_of(acc(ALICE)).as_deref(), Some("mende"));
}

#[test]
fn an_account_or_a_wallet_cannot_be_claimed_twice() {
    let mut r = registry("mainnet", false);
    propose(&mut r, ALICE, "mende", T0);
    // Same pair again: refreshes the expiry, no second claim.
    assert_eq!(propose(&mut r, ALICE, "mende", T0 + 100), T0 + 100 + TTL);
    assert_eq!(r.status().stored_link_proposals, 1);
    // The same account for another user.
    ctx(REGISTRAR, T0 + 101);
    assert!(panics(|| {
        r.propose_link(acc(ALICE), eoa("other"), "other".into());
    })
    .contains("already has a pending link proposal"));
    // The same user for another account.
    ctx(REGISTRAR, T0 + 101);
    assert!(panics(|| {
        r.propose_link(acc(BOB), eoa("mende"), "mende".into());
    })
    .contains("pending link proposal for another NEAR account"));
    accept(&mut r, ALICE, "mende", T0 + 102);
    // Linked now: claiming it again, or the wallet for another account, is refused.
    ctx(REGISTRAR, T0 + 103);
    assert!(panics(|| {
        r.propose_link(acc(ALICE), eoa("mende"), "mende".into());
    })
    .contains("already linked to this bonuz ID"));
    ctx(REGISTRAR, T0 + 103);
    assert!(panics(|| {
        r.propose_link(acc(BOB), eoa("mende"), "mende".into());
    })
    .contains("unlink it first"));
    // Cancel frees a claim; reject by the account too.
    propose(&mut r, BOB, "bobby", T0 + 104);
    ctx(REGISTRAR, T0 + 105);
    assert!(r.cancel_link_proposal(acc(BOB)));
    assert!(!r.cancel_link_proposal(acc(BOB)));
    propose(&mut r, BOB, "bobby", T0 + 106);
    ctx_self_yocto(BOB, T0 + 107);
    r.reject_link();
    assert_eq!(events()[0]["data"]["reason"], "rejected");
    assert!(r.link_proposal_of(acc(BOB)).is_none());
    ctx_self_yocto(BOB, T0 + 108);
    assert!(panics(|| r.reject_link()).contains("no pending link proposal"));
}

#[test]
fn accept_refused_when_the_wallet_got_linked_to_another_account_meanwhile() {
    let mut r = registry("testnet", true);
    propose(&mut r, ALICE, "mende", T0);
    // The registrar cancels and links an implicit account for the same user.
    ctx(REGISTRAR, T0 + 1);
    r.cancel_link_proposal(acc(ALICE));
    let w = Wallet::new(1);
    link(&mut r, &signed(&w, "mende", 1, T0 + 480));
    ctx_self_yocto(ALICE, T0 + 2);
    assert!(panics(|| r.accept_link(eoa("mende"))).contains("no pending link proposal"));
    assert_eq!(r.account_of("mende".into()), Some(acc(&w.account())));
}

#[test]
fn a_live_link_closes_open_claims_on_the_account_and_the_wallet() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    propose(&mut r, ALICE, "mende", T0);
    ctx(REGISTRAR, T0 + 1);
    link(&mut r, &signed(&w, "mende", 1, T0 + 480));
    assert!(r.link_proposal_of(acc(ALICE)).is_none(), "the user's other claim is closed");
    assert_eq!(r.status().stored_link_proposals, 0);
}

#[test]
fn accepting_replaces_the_accounts_link_to_another_user() {
    let mut r = registry("mainnet", false);
    propose(&mut r, ALICE, "first", T0);
    accept(&mut r, ALICE, "first", T0 + 1);
    propose(&mut r, ALICE, "second", T0 + 2);
    accept(&mut r, ALICE, "second", T0 + 3);
    let ev = link_events();
    assert_eq!((ev[0]["event"].as_str(), ev[0]["data"]["reason"].as_str()), (Some("unlink"), Some("replaced")));
    assert_eq!(r.account_of("first".into()), None);
    assert_eq!(r.account_of_eoa(eoa("first")), None);
    assert_eq!(r.account_of("second".into()), Some(acc(ALICE)));
    assert_eq!(r.status().links.0, 1);
}

#[test]
fn accepted_links_unlink_by_owner_and_by_registrar() {
    let mut r = registry("mainnet", false);
    propose(&mut r, ALICE, "mende", T0);
    accept(&mut r, ALICE, "mende", T0 + 1);
    ctx_self_yocto(ALICE, T0 + 2);
    r.unlink_by_owner();
    assert_eq!(events()[0]["data"]["bonuz_eoa"], eoa("mende"));
    assert_eq!(r.account_of("mende".into()), None);
    assert_eq!(r.account_of_eoa(eoa("mende")), None);
    assert_eq!(r.handle_of(eoa("mende")).as_deref(), Some("mende"), "the label stays");
    propose(&mut r, ALICE, "mende", T0 + 3);
    accept(&mut r, ALICE, "mende", T0 + 4);
    ctx(REGISTRAR, T0 + 5);
    assert!(r.unlink_by_registrar(acc(ALICE)));
    assert_eq!(r.bonuz_of(acc(ALICE)), None);
    assert_eq!(r.status().links.0, 0);
}

#[test]
fn a_rename_keeps_the_link_and_a_new_holder_of_the_old_handle_inherits_nothing() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&w, "mende", 1, T0 + 480));
    let user = eoa("mende");
    // Rename @mende -> @matthias.
    ctx(REGISTRAR, T0 + 1);
    r.set_handle(user.clone(), Some("matthias".into()));
    assert_eq!(r.bonuz_of(acc(&w.account())).as_deref(), Some("matthias"));
    assert_eq!(r.account_of("matthias".into()), Some(acc(&w.account())));
    assert_eq!(r.account_of("mende".into()), None);
    assert_eq!(r.link_of(acc(&w.account())).unwrap().bonuz_id, "matthias");
    // Someone else takes @mende: resolves to their wallet, which has no NEAR account.
    let newcomer = "0x00000000000000000000000000000000000000aa".to_string();
    ctx(REGISTRAR, T0 + 2);
    r.set_handle(newcomer.clone(), Some("mende".into()));
    assert_eq!(r.eoa_of("mende".into()), Some(newcomer.clone()));
    assert_eq!(r.account_of("mende".into()), None);
    // bonuz missed a rename: moving a label away leaves the old wallet's link, unlabelled.
    ctx(REGISTRAR, T0 + 3);
    r.set_handle(newcomer.clone(), Some("matthias".into()));
    assert_eq!(r.account_of("matthias".into()), None);
    assert_eq!(r.handle_of(user.clone()), None);
    assert_eq!(r.bonuz_of(acc(&w.account())), None);
    assert_eq!(r.account_of_eoa(user.clone()), Some(acc(&w.account())), "the link is still the wallet's");
    assert_eq!(r.link_of(acc(&w.account())).unwrap().bonuz_id, "");
    let ev = events();
    assert_eq!(ev[0]["event"], "handle");
    assert_eq!(ev[0]["data"]["bonuz_eoa"], user);
    assert!(ev[0]["data"]["bonuz_id"].is_null());
    // Clearing a label.
    ctx(REGISTRAR, T0 + 4);
    r.set_handle(newcomer.clone(), None);
    assert_eq!(r.eoa_of("matthias".into()), None);
    // Only the registrar labels.
    ctx(BOB, T0 + 5);
    assert!(panics(|| r.set_handle(user.clone(), Some("x".into()))).contains("only the bonuz registrar"));
}

#[test]
fn link_takes_the_wallet_from_the_signed_message() {
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    // No bonuz_eoa argument: the signed wallet line decides.
    ctx(REGISTRAR, T0);
    link_as(&mut r, &signed(&w, "mende", 1, T0 + 480), None);
    assert_eq!(r.account_of_eoa(eoa("mende")), Some(acc(&w.account())));
    // The three-line 2.1.0 message names no wallet: refused, whatever the argument says.
    let mut old = signed(&Wallet::new(2), "olls", 2, T0 + 480);
    old.message = build_link_message(&old.account_id, "olls", "testnet", T0 + 480);
    old.signature = Wallet::new(2).sign(&old.message, old.nonce, CONTRACT);
    ctx(REGISTRAR, T0 + 1);
    for arg in [None, Some(eoa("olls"))] {
        assert!(panics(|| {
            link_as(&mut r, &old, arg.clone());
        })
        .contains("must name the bonuz wallet"));
    }
    assert_eq!(r.status().stored_nonces, 1, "a refused link stores no nonce");
}

#[test]
fn an_explicit_bonuz_eoa_cannot_take_an_unlinked_wallets_label() {
    // The 2.2.0 review finding: with the handle-only message, the registrar
    // could pass any bonuz_eoa B and the link landed on B, moving the label off
    // an unlinked wallet U on the way. B never consented.
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    let u = "0x00000000000000000000000000000000000000aa".to_string();
    let b = "0x00000000000000000000000000000000000000bb".to_string();
    ctx(REGISTRAR, T0);
    r.set_handle(u.clone(), Some("mende".into())); // U holds @mende, no live link
    // The account signed for U; the registrar names B.
    let c = signed_for(&w, "mende", &u, 1, T0 + 480);
    ctx(REGISTRAR, T0 + 1);
    assert!(panics(|| {
        link_as(&mut r, &c, Some(b.clone()));
    })
    .contains("names a different bonuz wallet"));
    // The account signed for B, but @mende labels U: refused, no silent move.
    let c = signed_for(&w, "mende", &b, 2, T0 + 480);
    ctx(REGISTRAR, T0 + 2);
    for arg in [None, Some(b.clone())] {
        assert!(panics(|| {
            link_as(&mut r, &c, arg.clone());
        })
        .contains("labels another bonuz wallet"));
    }
    assert_eq!(r.eoa_of("mende".into()), Some(u.clone()), "the label did not move");
    assert_eq!(r.account_of_eoa(b.clone()), None);
    assert_eq!(r.account_of_eoa(u.clone()), None);
    assert_eq!(r.status().links.0, 0);
    // Signed for U, the wallet @mende labels: links U, the one that consented.
    let c = signed_for(&w, "mende", &u, 3, T0 + 480);
    ctx(REGISTRAR, T0 + 3);
    link_as(&mut r, &c, Some(u.clone()));
    assert_eq!(r.account_of_eoa(u), Some(acc(&w.account())));
}

#[test]
fn a_handle_retake_in_flight_never_lands_on_the_new_holder() {
    // The account signs for @mende while its wallet holds @mende. Before the
    // call lands (inside the 10-minute window) bonuz renames that user and a
    // new user V takes @mende. 2.2.0 before the fix, called without bonuz_eoa,
    // looked the wallet up by handle and linked the account to V.
    let mut r = registry("testnet", true);
    let w = Wallet::new(1);
    let user = eoa("mende");
    let v = "0x00000000000000000000000000000000000000cc".to_string();
    ctx(REGISTRAR, T0);
    r.set_handle(user.clone(), Some("mende".into()));
    let c = signed(&w, "mende", 1, T0 + 480); // names wallet eoa("mende")
    ctx(REGISTRAR, T0 + 60);
    r.set_handle(user.clone(), Some("matthias".into()));
    r.set_handle(v.clone(), Some("mende".into()));
    ctx(REGISTRAR, T0 + 120);
    for arg in [None, Some(user.clone()), Some(v.clone())] {
        assert!(panics(|| {
            link_as(&mut r, &c, arg.clone());
        })
        .contains(if arg.as_ref() == Some(&v) { "names a different bonuz wallet" } else { "labels another bonuz wallet" }));
    }
    assert_eq!(r.account_of_eoa(v.clone()), None, "V never consented");
    assert_eq!(r.account_of_eoa(user.clone()), None);
    assert_eq!(r.eoa_of("mende".into()), Some(v), "the retake stands");
    assert_eq!(r.handle_of(user.clone()), Some("matthias".into()));
    // A fresh message for the user's current handle links the right wallet.
    ctx(REGISTRAR, T0 + 130);
    link_as(&mut r, &signed_for(&w, "matthias", &user, 2, T0 + 600), None);
    assert_eq!(r.account_of_eoa(user), Some(acc(&w.account())));
}

#[test]
fn the_owner_can_drop_a_2_1_link_that_cannot_be_adopted_while_paused() {
    // Two 2.1.0 links that both belong to one wallet: the second cannot be
    // adopted, and unlink_by_registrar refuses while paused.
    let (a, b) = (Wallet::new(1), Wallet::new(2));
    write_v2_1_state(&a, &b);
    let mut r = Registry::migrate();
    ctx_yocto(OWNER, T0);
    r.set_paused(true);
    ctx(REGISTRAR, T0 + 1);
    r.adopt_legacy_link(acc(&a.account()), eoa("mende"));
    ctx(REGISTRAR, T0 + 1);
    assert!(panics(|| r.adopt_legacy_link(acc(&b.account()), eoa("mende"))).contains("already linked"));
    ctx(REGISTRAR, T0 + 1);
    assert!(panics(|| {
        r.unlink_by_registrar(acc(&b.account()));
    })
    .contains("paused"));
    // Only the owner, with 1 yoctoNEAR, and only for a 2.1.0 link.
    ctx_yocto(REGISTRAR, T0 + 2);
    assert!(panics(|| r.drop_legacy_link(acc(&b.account()))).contains("only the registry owner"));
    ctx(OWNER, T0 + 2);
    assert!(panics(|| r.drop_legacy_link(acc(&b.account()))).contains("1 yoctoNEAR"));
    ctx_yocto(OWNER, T0 + 2);
    assert!(panics(|| r.drop_legacy_link(acc(&a.account()))).contains("no 2.1.0 link"));
    ctx_yocto(OWNER, T0 + 2);
    assert!(panics(|| r.drop_legacy_link(acc(ALICE))).contains("no 2.1.0 link"));
    ctx_yocto(OWNER, T0 + 3);
    r.drop_legacy_link(acc(&b.account()));
    let ev = link_events();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["event"], "unlink");
    assert_eq!(ev[0]["data"]["reason"], "legacy");
    assert_eq!(ev[0]["data"]["bonuz_id"], "olls");
    assert!(ev[0]["data"]["bonuz_eoa"].is_null());
    let s = r.status();
    assert_eq!((s.links.0, s.legacy_links.0), (1, 0));
    assert_eq!(r.account_of("olls".into()), None);
    assert_eq!(r.link_of(acc(&b.account())), None);
    // The backfill is done: after unpausing, links work again.
    ctx_yocto(OWNER, T0 + 4);
    r.set_paused(false);
    ctx(REGISTRAR, T0 + 5);
    link(&mut r, &signed(&Wallet::new(3), "fresh", 5, T0 + 480));
    assert_eq!(r.account_of("fresh".into()), Some(acc(&Wallet::new(3).account())));
}

#[test]
fn bad_wallets_and_handles_refused() {
    for bad in ["", "0x", "0xABCDEF0000000000000000000000000000000000", "1x0000000000000000000000000000000000000000", "0x00000000000000000000000000000000000000000"] {
        assert!(check_bonuz_eoa(bad).is_err(), "{bad}");
    }
    assert!(check_bonuz_eoa("0x00000000000000000000000000000000000000af").is_ok());
    let mut r = registry("mainnet", false);
    ctx(REGISTRAR, T0);
    assert!(panics(|| {
        r.propose_link(acc(ALICE), "0xABC".into(), "mende".into());
    })
    .contains("bonuz_eoa must be"));
    ctx(REGISTRAR, T0);
    assert!(panics(|| {
        r.propose_link(acc(ALICE), eoa("x"), "Mende".into());
    })
    .contains("lowercase"));
}

#[test]
fn paused_refuses_propose_and_accept_but_not_reject_or_cancel() {
    let mut r = registry("mainnet", false);
    propose(&mut r, ALICE, "mende", T0);
    propose(&mut r, BOB, "bobby", T0);
    ctx_yocto(OWNER, T0);
    r.set_paused(true);
    ctx(REGISTRAR, T0);
    assert!(panics(|| {
        r.propose_link(acc("carol.testnet"), eoa("carol"), "carol".into());
    })
    .contains("paused"));
    ctx_self_yocto(ALICE, T0);
    assert!(panics(|| r.accept_link(eoa("mende"))).contains("paused"));
    ctx_self_yocto(ALICE, T0);
    r.reject_link();
    ctx(REGISTRAR, T0);
    assert!(r.cancel_link_proposal(acc(BOB)));
}

#[test]
fn pending_claims_are_capped() {
    let mut r = registry("mainnet", false);
    for i in 0..MAX_LINK_PROPOSALS {
        ctx(REGISTRAR, T0);
        r.propose_link(acc(&format!("a{i}.testnet")), format!("0x{i:040x}"), format!("h{i}"));
    }
    ctx(REGISTRAR, T0);
    assert!(panics(|| {
        r.propose_link(acc("one-more.testnet"), eoa("one-more"), "one-more".into());
    })
    .contains("too many pending link proposals"));
    // A refresh of an existing claim still works at the cap.
    ctx(REGISTRAR, T0 + 1);
    r.propose_link(acc("a0.testnet"), format!("0x{:040x}", 0), "h0".into());
    // Once they expire, proposing prunes and frees room.
    ctx(BOB, T0 + TTL + 1);
    assert_eq!(r.prune_link_proposals(1_000), 50, "at most 50 per call");
    propose(&mut r, "one-more.testnet", "one-more", T0 + TTL + 2);
}

/// Writes a 2.1.0 state with two handle-keyed links, the way 2.1.0 stored it.
fn write_v2_1_state(a: &Wallet, b: &Wallet) {
    ctx(CONTRACT, T0);
    let mut old = RegistryV2_1 {
        owner: acc(OWNER),
        pending_owner: None,
        registrar: acc(REGISTRAR),
        network: "testnet".into(),
        allow_registrar_checked: true,
        paused: false,
        reserve: NearToken::from_near(1),
        links: LookupMap::new(StorageKey::Links),
        link_count: 2,
        account_of: LookupMap::new(StorageKey::AccountOf),
        nonces: IterableMap::new(StorageKey::Nonces),
        raised_cap: Some((1000, T0 + 86_400)),
        unlink_day: 7,
        unlinks_today: 3,
    };
    for (w, handle) in [(a, "mende"), (b, "olls")] {
        old.links.insert(
            acc(&w.account()),
            VersionedLink::V1(Link {
                bonuz_id: handle.into(),
                public_key: w.public_key(),
                checked_by: "contract".into(),
                linked_at_block: U64(42),
                linked_at: U64(T0 * SEC),
            }),
        );
        old.account_of.insert(handle.into(), acc(&w.account()));
    }
    old.nonces.insert([9; 32], T0 + 480);
    old.links.flush();
    old.account_of.flush();
    old.nonces.flush();
    env::state_write(&old);
}

#[test]
fn migrate_keeps_2_1_links_readable_and_adopt_moves_them_to_the_wallet() {
    let (a, b) = (Wallet::new(1), Wallet::new(2));
    write_v2_1_state(&a, &b);
    let mut r = Registry::migrate();
    let s = r.status();
    assert_eq!((s.links.0, s.legacy_links.0, s.stored_nonces), (2, 2, 1));
    assert_eq!(s.raised_unlink_cap, Some((1000, U64(T0 + 86_400))));
    assert_eq!(s.version, "2.2.0");
    assert!(s.allow_registrar_checked);
    // Old links read as before.
    assert_eq!(r.bonuz_of(acc(&a.account())).as_deref(), Some("mende"));
    assert_eq!(r.account_of("olls".into()), Some(acc(&b.account())));
    let l = r.link_of(acc(&a.account())).unwrap();
    assert_eq!((l.bonuz_id.as_str(), l.bonuz_eoa.clone(), l.linked_at_block.0), ("mende", None, 42));
    // The used nonce is still refused.
    ctx(REGISTRAR, T0 + 1);
    let mut replay = signed(&a, "mende", 9, T0 + 480);
    replay.nonce = [9; 32];
    replay.signature = a.sign(&replay.message, [9; 32], CONTRACT);
    assert!(link_err(&mut r, &replay).contains("nonce was already used"));
    // A legacy handle cannot be relabelled or proposed for someone else before adoption.
    ctx(REGISTRAR, T0 + 1);
    assert!(panics(|| r.set_handle(eoa("x"), Some("mende".into()))).contains("2.1.0 link still names"));
    ctx(REGISTRAR, T0 + 1);
    assert!(panics(|| {
        r.propose_link(acc(ALICE), eoa("x"), "mende".into());
    })
    .contains("not all adopted yet"));
    ctx(REGISTRAR, T0 + 1);
    let mut other = signed(&Wallet::new(3), "mende", 3, T0 + 480);
    other.signature = Wallet::new(3).sign(&other.message, other.nonce, CONTRACT);
    assert!(link_err(&mut r, &other).contains("not all adopted yet"));
    // Adopt: the link moves to the wallet with its key and time.
    ctx(REGISTRAR, T0 + 2);
    r.adopt_legacy_link(acc(&a.account()), eoa("mende"));
    let l = r.link_of(acc(&a.account())).unwrap();
    assert_eq!((l.bonuz_id.as_str(), l.bonuz_eoa.clone(), l.linked_at_block.0, l.public_key.clone()), ("mende", Some(eoa("mende")), 42, Some(a.public_key())));
    assert_eq!(r.account_of("mende".into()), Some(acc(&a.account())));
    assert_eq!(r.status().legacy_links.0, 1);
    assert_eq!(r.status().links.0, 2);
    ctx(REGISTRAR, T0 + 3);
    assert!(panics(|| r.adopt_legacy_link(acc(&a.account()), eoa("mende"))).contains("no 2.1.0 link"));
    // A rename now keeps it.
    ctx(REGISTRAR, T0 + 4);
    r.set_handle(eoa("mende"), Some("matthias".into()));
    assert_eq!(r.bonuz_of(acc(&a.account())).as_deref(), Some("matthias"));
    // While one 2.1.0 link is left, new links wait; once it is adopted they work.
    ctx(REGISTRAR, T0 + 5);
    assert!(link_err(&mut r, &signed(&b, "olls", 5, T0 + 480)).contains("not all adopted yet"));
    ctx(REGISTRAR, T0 + 5);
    r.adopt_legacy_link(acc(&b.account()), eoa("olls"));
    assert_eq!(r.status().legacy_links.0, 0);
    ctx(REGISTRAR, T0 + 6);
    link(&mut r, &signed(&b, "olls", 6, T0 + 480));
    assert_eq!(r.account_of_eoa(eoa("olls")), Some(acc(&b.account())));
    assert_eq!(r.status().links.0, 2);
}

#[test]
fn unlinking_a_legacy_link_counts_down() {
    let (a, b) = (Wallet::new(1), Wallet::new(2));
    write_v2_1_state(&a, &b);
    let mut r = Registry::migrate();
    ctx_yocto(&a.account(), T0);
    r.unlink_by_owner();
    let s = r.status();
    assert_eq!((s.links.0, s.legacy_links.0), (1, 1));
    assert_eq!(r.account_of("mende".into()), None);
    assert!(events()[0]["data"]["bonuz_eoa"].is_null());
}

#[test]
fn migrate_twice_fails() {
    let (a, b) = (Wallet::new(1), Wallet::new(2));
    write_v2_1_state(&a, &b);
    let r = Registry::migrate();
    env::state_write(&r);
    assert!(panics(|| {
        Registry::migrate();
    })
    .contains("Cannot deserialize"));
}

// ---------- review fixes ----------

fn ctx_balance(caller: &str, now_s: u64, balance: NearToken) {
    testing_env!(VMContextBuilder::new()
        .current_account_id(CONTRACT.parse().unwrap())
        .predecessor_account_id(caller.parse().unwrap())
        .signer_account_id(caller.parse().unwrap())
        .block_timestamp(now_s * SEC)
        .account_balance(balance)
        .build());
}

#[test]
fn a_wallet_with_an_unadopted_2_1_link_cannot_get_a_second_account() {
    let (a, b) = (Wallet::new(1), Wallet::new(2));
    write_v2_1_state(&a, &b);
    let mut r = Registry::migrate();
    // The user behind 2.1.0 "mende" (wallet eoa("mende")) tries a second account.
    let c = Wallet::new(3);
    ctx(REGISTRAR, T0 + 1);
    let call = signed_for(&c, "fresh", &eoa("mende"), 3, T0 + 480);
    assert!(panics(|| {
        link_as(&mut r, &call, Some(eoa("mende")));
    })
    .contains("not all adopted yet"));
    ctx(REGISTRAR, T0 + 1);
    assert!(panics(|| {
        r.propose_link(acc(ALICE), eoa("mende"), "fresh".into());
    })
    .contains("not all adopted yet"));
    // The backfill can run behind a pause.
    ctx_yocto(OWNER, T0 + 2);
    r.set_paused(true);
    ctx(REGISTRAR, T0 + 3);
    r.adopt_legacy_link(acc(&a.account()), eoa("mende"));
    r.adopt_legacy_link(acc(&b.account()), eoa("olls"));
    assert_eq!(r.status().legacy_links.0, 0);
    ctx_yocto(OWNER, T0 + 4);
    r.set_paused(false);
    // Now the wallet's own link is visible and a second account is refused.
    ctx(REGISTRAR, T0 + 5);
    let mut call = signed(&c, "mende", 5, T0 + 480);
    call.signature = c.sign(&call.message, call.nonce, CONTRACT);
    assert!(link_err(&mut r, &call).contains("unlink it first"));
    assert_eq!(r.account_of_eoa(eoa("mende")), Some(acc(&a.account())));
}

#[test]
fn link_and_propose_never_move_a_handle_from_another_linked_wallet() {
    let mut r = registry("testnet", true);
    let (a, b) = (Wallet::new(1), Wallet::new(2));
    ctx(REGISTRAR, T0);
    link(&mut r, &signed(&a, "mende", 1, T0 + 480)); // eoa("mende") holds @mende, linked
    // Another wallet tries to take @mende through link: refused, as 2.1.0 did.
    let other_wallet = "0x00000000000000000000000000000000000000bb".to_string();
    let call = signed_for(&b, "mende", &other_wallet, 2, T0 + 480);
    assert!(panics(|| {
        link_as(&mut r, &call, Some(other_wallet.clone()));
    })
    .contains("unlink it first"));
    assert_eq!(r.eoa_of("mende".into()), Some(eoa("mende")), "the label did not move");
    ctx(REGISTRAR, T0 + 1);
    assert!(panics(|| {
        r.propose_link(acc(ALICE), other_wallet.clone(), "mende".into());
    })
    .contains("unlink it first"));
    // An explicit set_handle on the old wallet frees it; then it can be taken.
    ctx(REGISTRAR, T0 + 2);
    r.set_handle(eoa("mende"), Some("matthias".into()));
    propose(&mut r, ALICE, "x", T0 + 3); // unrelated claim still fine
    ctx(REGISTRAR, T0 + 4);
    r.propose_link(acc(BOB), other_wallet.clone(), "mende".into());
    assert_eq!(r.eoa_of("mende".into()), Some(other_wallet));
    // A label of a wallet WITHOUT a live link may move (nothing is lost).
    ctx(REGISTRAR, T0 + 5);
    r.set_handle(eoa("lonely"), Some("lonely".into()));
    propose(&mut r, "carol.testnet", "lonely2", T0 + 6);
    ctx(REGISTRAR, T0 + 7);
    r.propose_link(acc("dave.testnet"), "0x00000000000000000000000000000000000000cc".into(), "lonely".into());
    assert_eq!(r.handle_of(eoa("lonely")), None);
}

#[test]
fn label_writes_and_adoption_respect_the_reserve() {
    let (a, b) = (Wallet::new(1), Wallet::new(2));
    write_v2_1_state(&a, &b);
    let mut r = Registry::migrate();
    ctx_balance(REGISTRAR, T0, NearToken::from_millinear(500)); // reserve is 1 NEAR
    assert!(panics(|| r.set_handle(eoa("x"), Some("x".into()))).contains("under its reserve"));
    ctx_balance(REGISTRAR, T0, NearToken::from_millinear(500));
    assert!(panics(|| r.adopt_legacy_link(acc(&a.account()), eoa("mende"))).contains("under its reserve"));
    ctx(REGISTRAR, T0);
    r.adopt_legacy_link(acc(&a.account()), eoa("mende"));
}

#[test]
fn accept_through_a_contract_records_no_key() {
    let mut r = registry("mainnet", false);
    propose(&mut r, ALICE, "mende", T0);
    // alice.testnet calls accept_link from a contract call: the signer is someone else.
    testing_env!(VMContextBuilder::new()
        .current_account_id(CONTRACT.parse().unwrap())
        .predecessor_account_id(ALICE.parse().unwrap())
        .signer_account_id(BOB.parse().unwrap())
        .block_timestamp((T0 + 1) * SEC)
        .account_balance(NearToken::from_near(5))
        .attached_deposit(NearToken::from_yoctonear(1))
        .build());
    r.accept_link(eoa("mende"));
    let l = r.link_of(acc(ALICE)).unwrap();
    assert_eq!((l.checked_by.as_str(), l.public_key), ("account", None));
    assert!(events().iter().any(|e| e["event"] == "link" && e["data"]["public_key"].is_null()));
}

#[test]
fn reject_needs_one_yocto() {
    let mut r = registry("mainnet", false);
    propose(&mut r, ALICE, "mende", T0);
    ctx(ALICE, T0 + 1);
    assert!(panics(|| r.reject_link()).contains("1 yoctoNEAR"));
    assert!(r.link_proposal_of(acc(ALICE)).is_some());
    ctx_self_yocto(ALICE, T0 + 2);
    r.reject_link();
    assert!(r.link_proposal_of(acc(ALICE)).is_none());
}
