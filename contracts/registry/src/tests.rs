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

fn signed_with(
    w: &Wallet,
    account: &str,
    handle: &str,
    network: &str,
    nonce: u8,
    expires: u64,
    recipient: &str,
) -> Call {
    let message = build_link_message(account, handle, network, expires);
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
    )
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
    assert_eq!(l.public_key, w.public_key());
    let ev = events();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["event"], "link");
    assert_eq!(ev[0]["version"], "2.0.0");
    assert_eq!(ev[0]["data"]["checked_by"], "contract");
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
    let ev = events();
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
    let base = build_link_message(&Wallet::new(1).account(), "mende", "testnet", T0 + 480);
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
                    l.swap(rand(3), rand(3));
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
            assert_eq!(build_link_message(p.account_id, p.bonuz_id, p.network, p.expires), m);
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
        let message = build_link_message(&w.account(), &handle, "mainnet", now + 300);
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
