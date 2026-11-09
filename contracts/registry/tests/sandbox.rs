//! Sandbox tests for the bonuz ID registry, run against a local NEAR node via
//! near-workspaces. `cargo test` builds the contract to wasm first.

use near_workspaces::types::NearToken;
use near_workspaces::{Account, Contract};
use serde_json::{json, Value};

const DEPOSIT: NearToken = NearToken::from_millinear(10); // 0.01 NEAR, far more than needed

struct Env {
    sandbox: near_workspaces::Worker<near_workspaces::network::Sandbox>,
    contract: Contract,
    owner: Account,
    sponsor: Account,
    alice: Account,
    bob: Account,
}

const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);

async fn setup() -> anyhow::Result<Env> {
    let wasm = near_workspaces::compile_project("./").await?;
    let sandbox = near_workspaces::sandbox().await?;
    let contract = sandbox.dev_deploy(&wasm).await?;
    let root = sandbox.root_account()?;
    let owner = root
        .create_subaccount("owner")
        .initial_balance(NearToken::from_near(10))
        .transact()
        .await?
        .into_result()?;
    let sponsor = root
        .create_subaccount("sponsor")
        .initial_balance(NearToken::from_near(10))
        .transact()
        .await?
        .into_result()?;
    contract
        .call("new")
        .args_json(json!({ "owner_id": owner.id(), "sponsor_id": sponsor.id() }))
        .transact()
        .await?
        .into_result()?;

    let alice = root
        .create_subaccount("alice")
        .initial_balance(NearToken::from_near(10))
        .transact()
        .await?
        .into_result()?;
    let bob = root
        .create_subaccount("bob")
        .initial_balance(NearToken::from_near(10))
        .transact()
        .await?
        .into_result()?;
    Ok(Env { sandbox, contract, owner, sponsor, alice, bob })
}

async fn bonuz_of(env: &Env, account: &Account) -> anyhow::Result<Option<String>> {
    Ok(env
        .contract
        .view("bonuz_of")
        .args_json(json!({ "account_id": account.id() }))
        .await?
        .json()?)
}

async fn account_of(env: &Env, bonuz_id: &str) -> anyhow::Result<Option<String>> {
    Ok(env
        .contract
        .view("account_of")
        .args_json(json!({ "bonuz_id": bonuz_id }))
        .await?
        .json()?)
}

async fn link(account: &Account, env: &Env, bonuz_id: &str) -> anyhow::Result<near_workspaces::result::ExecutionFinalResult> {
    Ok(account
        .call(env.contract.id(), "link")
        .args_json(json!({ "bonuz_id": bonuz_id }))
        .deposit(DEPOSIT)
        .max_gas()
        .transact()
        .await?)
}

/// The NEP-297 event logged by a call, parsed.
fn event(logs: &[&str]) -> Value {
    let line = logs
        .iter()
        .find_map(|l| l.strip_prefix("EVENT_JSON:"))
        .expect("call emitted no EVENT_JSON log");
    serde_json::from_str(line).expect("event is valid JSON")
}

#[tokio::test]
async fn link_sets_both_views_and_emits_event() -> anyhow::Result<()> {
    let env = setup().await?;
    assert_eq!(bonuz_of(&env, &env.alice).await?, None);
    assert_eq!(account_of(&env, "mende").await?, None);

    let res = link(&env.alice, &env, "mende").await?;
    assert!(res.is_success(), "{res:#?}");

    assert_eq!(bonuz_of(&env, &env.alice).await?.as_deref(), Some("mende"));
    assert_eq!(account_of(&env, "mende").await?.as_deref(), Some(env.alice.id().as_str()));

    let ev = event(&res.logs());
    assert_eq!(ev["standard"], "bonuz_id_near");
    assert_eq!(ev["version"], "1.0.0");
    assert_eq!(ev["event"], "link");
    assert_eq!(ev["data"]["account_id"], env.alice.id().as_str());
    assert_eq!(ev["data"]["bonuz_id"], "mende");
    Ok(())
}

#[tokio::test]
async fn relink_replaces_the_old_handle() -> anyhow::Result<()> {
    let env = setup().await?;
    link(&env.alice, &env, "mende").await?.into_result()?;

    let res = link(&env.alice, &env, "mende_two").await?;
    assert!(res.is_success(), "{res:#?}");

    assert_eq!(bonuz_of(&env, &env.alice).await?.as_deref(), Some("mende_two"));
    assert_eq!(account_of(&env, "mende_two").await?.as_deref(), Some(env.alice.id().as_str()));
    assert_eq!(account_of(&env, "mende").await?, None, "old handle must be freed");

    let ev = event(&res.logs());
    assert_eq!(ev["data"]["previous_bonuz_id"], "mende");

    // The freed handle can now be taken by someone else.
    link(&env.bob, &env, "mende").await?.into_result()?;
    assert_eq!(account_of(&env, "mende").await?.as_deref(), Some(env.bob.id().as_str()));
    Ok(())
}

#[tokio::test]
async fn linking_the_same_handle_again_is_a_no_op() -> anyhow::Result<()> {
    let env = setup().await?;
    link(&env.alice, &env, "mende").await?.into_result()?;
    let res = link(&env.alice, &env, "mende").await?;
    assert!(res.is_success(), "{res:#?}");
    assert!(res.logs().iter().all(|l| !l.starts_with("EVENT_JSON:")), "no event for a no-op");
    assert_eq!(bonuz_of(&env, &env.alice).await?.as_deref(), Some("mende"));
    Ok(())
}

#[tokio::test]
async fn handle_of_another_account_is_refused() -> anyhow::Result<()> {
    let env = setup().await?;
    link(&env.alice, &env, "mende").await?.into_result()?;

    let res = link(&env.bob, &env, "mende").await?;
    assert!(res.is_failure());
    assert!(format!("{res:?}").contains("already linked to another NEAR account"));

    assert_eq!(account_of(&env, "mende").await?.as_deref(), Some(env.alice.id().as_str()));
    assert_eq!(bonuz_of(&env, &env.bob).await?, None);
    Ok(())
}

#[tokio::test]
async fn unlink_clears_both_views_and_refunds_storage() -> anyhow::Result<()> {
    let env = setup().await?;
    let empty = env.contract.view_account().await?.storage_usage;
    link(&env.alice, &env, "mende").await?.into_result()?;
    let used = env.contract.view_account().await?.storage_usage - empty;
    assert!(used > 0);

    let before = env.alice.view_account().await?.balance;
    let res = env
        .alice
        .call(env.contract.id(), "unlink")
        .deposit(ONE_YOCTO)
        .max_gas()
        .transact()
        .await?;
    assert!(res.is_success(), "{res:#?}");
    let after = env.alice.view_account().await?.balance;

    assert_eq!(bonuz_of(&env, &env.alice).await?, None);
    assert_eq!(account_of(&env, "mende").await?, None);
    assert_eq!(env.contract.view_account().await?.storage_usage, empty, "storage fully freed");

    // Storage refund is used * 10^19 yocto. Gas for the call is far smaller,
    // so alice must end up with more than half the refund on top.
    let refund = NearToken::from_yoctonear(u128::from(used) * 10u128.pow(19));
    assert!(
        after.as_yoctonear() > before.as_yoctonear() + refund.as_yoctonear() / 2,
        "expected a storage refund of ~{refund}, balance went {before} -> {after}"
    );

    let ev = event(&res.logs());
    assert_eq!(ev["event"], "unlink");
    assert_eq!(ev["data"]["bonuz_id"], "mende");

    // Unlink twice fails cleanly.
    let again = env
        .alice
        .call(env.contract.id(), "unlink")
        .deposit(ONE_YOCTO)
        .transact()
        .await?;
    assert!(again.is_failure());
    assert!(format!("{again:?}").contains("no bonuz ID linked"));
    Ok(())
}

#[tokio::test]
async fn excess_deposit_is_refunded_and_too_little_is_refused() -> anyhow::Result<()> {
    let env = setup().await?;

    // No deposit: refused.
    let res = env
        .alice
        .call(env.contract.id(), "link")
        .args_json(json!({ "bonuz_id": "mende" }))
        .transact()
        .await?;
    assert!(res.is_failure());
    assert!(format!("{res:?}").contains("attach at least"));

    // 1 NEAR attached: the contract keeps only the storage cost.
    let contract_before = env.contract.view_account().await?.balance;
    let storage_before = env.contract.view_account().await?.storage_usage;
    env.alice
        .call(env.contract.id(), "link")
        .args_json(json!({ "bonuz_id": "mende" }))
        .deposit(NearToken::from_near(1))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    let contract_after = env.contract.view_account().await?.balance;
    let used = env.contract.view_account().await?.storage_usage - storage_before;
    let cost = u128::from(used) * 10u128.pow(19);

    let kept = contract_after.as_yoctonear() - contract_before.as_yoctonear();
    // The contract also earns a share of gas fees, so allow a small margin.
    assert!(kept >= cost, "contract kept {kept}, storage costs {cost}");
    assert!(
        kept < cost + NearToken::from_millinear(1).as_yoctonear(),
        "contract kept {kept}, expected about {cost}: excess was not refunded"
    );

    // link_deposit() covers the real cost.
    let suggested: NearToken = env.contract.view("link_deposit").await?.json()?;
    assert!(suggested.as_yoctonear() >= cost);
    Ok(())
}

#[tokio::test]
async fn invalid_handles_are_refused() -> anyhow::Result<()> {
    let env = setup().await?;
    let long = "x".repeat(33);
    for (bad, why) in [
        ("a", "2 to 32"),
        ("", "2 to 32"),
        (long.as_str(), "2 to 32"),
        ("Mende", "lowercase"),
        ("mende.near", "lowercase"),
        ("me nde", "lowercase"),
    ] {
        let res = link(&env.alice, &env, bad).await?;
        assert!(res.is_failure(), "{bad:?} should be refused");
        assert!(format!("{res:?}").contains(why), "{bad:?}: expected '{why}' in {res:?}");
    }
    assert_eq!(bonuz_of(&env, &env.alice).await?, None);

    // The edges are fine.
    link(&env.alice, &env, "ab").await?.into_result()?;
    link(&env.bob, &env, &"x".repeat(32)).await?.into_result()?;
    Ok(())
}

#[tokio::test]
async fn changes_need_a_full_access_key_deposit() -> anyhow::Result<()> {
    let env = setup().await?;
    link(&env.alice, &env, "mende").await?.into_result()?;

    // unlink without the 1 yoctoNEAR is refused, so a function-call key cannot unlink.
    let res = env.alice.call(env.contract.id(), "unlink").transact().await?;
    assert!(res.is_failure());
    assert!(format!("{res:?}").contains("Requires attached deposit of exactly 1 yoctoNEAR"));
    assert_eq!(bonuz_of(&env, &env.alice).await?.as_deref(), Some("mende"));
    Ok(())
}

#[tokio::test]
async fn owner_can_clear_a_squatted_handle_others_cannot() -> anyhow::Result<()> {
    let env = setup().await?;
    // bob squats "mende".
    link(&env.bob, &env, "mende").await?.into_result()?;

    // alice is not the owner.
    let res = env
        .alice
        .call(env.contract.id(), "admin_unlink")
        .args_json(json!({ "bonuz_id": "mende" }))
        .deposit(ONE_YOCTO)
        .transact()
        .await?;
    assert!(res.is_failure());
    assert!(format!("{res:?}").contains("only the registry owner"));

    let owner: String = env.contract.view("owner").await?.json()?;
    assert_eq!(owner, env.owner.id().as_str());

    let res = env
        .owner
        .call(env.contract.id(), "admin_unlink")
        .args_json(json!({ "bonuz_id": "mende" }))
        .deposit(ONE_YOCTO)
        .max_gas()
        .transact()
        .await?;
    assert!(res.is_success(), "{res:#?}");
    let ev = event(&res.logs());
    assert_eq!(ev["event"], "unlink");
    assert_eq!(ev["data"]["account_id"], env.bob.id().as_str());
    assert_eq!(ev["data"]["removed_by"], env.owner.id().as_str());

    assert_eq!(account_of(&env, "mende").await?, None);
    assert_eq!(bonuz_of(&env, &env.bob).await?, None);

    // The real owner of the handle can now link it.
    link(&env.alice, &env, "mende").await?.into_result()?;
    assert_eq!(account_of(&env, "mende").await?.as_deref(), Some(env.alice.id().as_str()));
    Ok(())
}

#[tokio::test]
async fn only_the_contract_account_can_initialise() -> anyhow::Result<()> {
    let wasm = near_workspaces::compile_project("./").await?;
    let sandbox = near_workspaces::sandbox().await?;
    let contract = sandbox.dev_deploy(&wasm).await?;
    let mallory = sandbox.dev_create_account().await?;

    let res = mallory
        .call(contract.id(), "new")
        .args_json(json!({ "owner_id": mallory.id(), "sponsor_id": null }))
        .transact()
        .await?;
    assert!(res.is_failure(), "a stranger must not be able to initialise and become owner");
    assert!(format!("{res:?}").contains("is private"));
    Ok(())
}

// ---------------------------------------------------------------------------
// One-click path: wallets created in the bonuz app are NEAR implicit accounts.
// The app signs a NEP-413 message, the bonuz sponsor submits it and pays.
// ---------------------------------------------------------------------------

use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

/// A wallet as the bonuz app creates it: a fresh ed25519 key whose implicit
/// account id (hex of the public key) does not exist on chain yet.
struct AppWallet {
    key: SigningKey,
    account_id: String,
}

fn app_wallet() -> AppWallet {
    let key = SigningKey::generate(&mut rand::rngs::OsRng);
    let account_id = hex::encode(key.verifying_key().to_bytes());
    AppWallet { key, account_id }
}

#[derive(borsh::BorshSerialize)]
struct Nep413Payload {
    message: String,
    nonce: [u8; 32],
    recipient: String,
    callback_url: Option<String>,
}

/// NEP-413 signature, the way a NEAR wallet signs a message. Returns (nonce, signature) in base64.
fn nep413_sign(key: &SigningKey, message: &str, recipient: &str) -> (String, String) {
    let nonce: [u8; 32] = rand::random();
    let payload = Nep413Payload {
        message: message.to_owned(),
        nonce,
        recipient: recipient.to_owned(),
        callback_url: None,
    };
    let mut data = ((1u32 << 31) + 413).to_le_bytes().to_vec();
    data.extend(borsh::to_vec(&payload).unwrap());
    let signature = key.sign(&Sha256::digest(&data));
    let b64 = base64::engine::general_purpose::STANDARD;
    (b64.encode(nonce), b64.encode(signature.to_bytes()))
}

async fn signed_link_args(env: &Env, wallet: &AppWallet, bonuz_id: &str) -> anyhow::Result<Value> {
    let message: String = env
        .contract
        .view("signed_link_message")
        .args_json(json!({ "account_id": wallet.account_id, "bonuz_id": bonuz_id }))
        .await?
        .json()?;
    let (nonce, signature) = nep413_sign(&wallet.key, &message, env.contract.id().as_str());
    Ok(json!({ "account_id": wallet.account_id, "bonuz_id": bonuz_id, "nonce": nonce, "signature": signature }))
}

async fn submit(
    by: &Account,
    env: &Env,
    method: &str,
    args: &Value,
) -> anyhow::Result<near_workspaces::result::ExecutionFinalResult> {
    Ok(by
        .call(env.contract.id(), method)
        .args_json(args)
        .deposit(DEPOSIT)
        .max_gas()
        .transact()
        .await?)
}

async fn bonuz_of_id(env: &Env, account_id: &str) -> anyhow::Result<Option<String>> {
    Ok(env
        .contract
        .view("bonuz_of")
        .args_json(json!({ "account_id": account_id }))
        .await?
        .json()?)
}

#[tokio::test]
async fn app_wallet_links_in_one_click_without_any_near() -> anyhow::Result<()> {
    let env = setup().await?;
    let wallet = app_wallet();

    let args = signed_link_args(&env, &wallet, "mende").await?;
    let res = submit(&env.sponsor, &env, "link_signed", &args).await?;
    assert!(res.is_success(), "{res:#?}");

    assert_eq!(bonuz_of_id(&env, &wallet.account_id).await?.as_deref(), Some("mende"));
    assert_eq!(account_of(&env, "mende").await?.as_deref(), Some(wallet.account_id.as_str()));

    let ev = event(&res.logs());
    assert_eq!(ev["event"], "link");
    assert_eq!(ev["data"]["account_id"], wallet.account_id.as_str());
    assert_eq!(ev["data"]["sponsored_by"], env.sponsor.id().as_str());

    let payer: Option<String> = env
        .contract
        .view("payer_of")
        .args_json(json!({ "account_id": wallet.account_id }))
        .await?
        .json()?;
    assert_eq!(payer.as_deref(), Some(env.sponsor.id().as_str()));

    // The user's account still does not exist on chain: it never needed NEAR.
    let id: near_workspaces::AccountId = wallet.account_id.parse()?;
    assert!(env.sandbox.view_account(&id).await.is_err(), "implicit account should not exist on chain");
    Ok(())
}

#[tokio::test]
async fn a_signature_works_only_once() -> anyhow::Result<()> {
    let env = setup().await?;
    let wallet = app_wallet();
    let args = signed_link_args(&env, &wallet, "mende").await?;
    submit(&env.sponsor, &env, "link_signed", &args).await?.into_result()?;

    // The user later moves to another handle; replaying the old signature must fail.
    let args2 = signed_link_args(&env, &wallet, "mende_two").await?;
    submit(&env.sponsor, &env, "link_signed", &args2).await?.into_result()?;
    let replay = submit(&env.sponsor, &env, "link_signed", &args).await?;
    assert!(replay.is_failure());
    assert!(format!("{replay:?}").contains("invalid signature"));
    assert_eq!(bonuz_of_id(&env, &wallet.account_id).await?.as_deref(), Some("mende_two"));
    Ok(())
}

#[tokio::test]
async fn signed_link_refuses_wrong_key_tampered_handle_named_account_and_strangers() -> anyhow::Result<()> {
    let env = setup().await?;
    let wallet = app_wallet();
    let other = app_wallet();

    // Signed by another key.
    let mut args = signed_link_args(&env, &other, "mende").await?;
    args["account_id"] = json!(wallet.account_id);
    let res = submit(&env.sponsor, &env, "link_signed", &args).await?;
    assert!(format!("{res:?}").contains("invalid signature"));

    // Handle changed after signing.
    let mut args = signed_link_args(&env, &wallet, "mende").await?;
    args["bonuz_id"] = json!("someone_else");
    let res = submit(&env.sponsor, &env, "link_signed", &args).await?;
    assert!(format!("{res:?}").contains("invalid signature"));

    // A named account cannot use the signed path: its key is not in its name.
    let mut args = signed_link_args(&env, &wallet, "mende").await?;
    args["account_id"] = json!(env.alice.id());
    let res = submit(&env.sponsor, &env, "link_signed", &args).await?;
    assert!(format!("{res:?}").contains("only work for implicit NEAR accounts"));

    // A valid signature submitted by anyone but the sponsor.
    let args = signed_link_args(&env, &wallet, "mende").await?;
    let res = submit(&env.alice, &env, "link_signed", &args).await?;
    assert!(format!("{res:?}").contains("only the bonuz sponsor account"));

    assert_eq!(bonuz_of_id(&env, &wallet.account_id).await?, None);
    assert_eq!(account_of(&env, "mende").await?, None);

    // The untouched signature still works from the sponsor.
    submit(&env.sponsor, &env, "link_signed", &args).await?.into_result()?;
    assert_eq!(bonuz_of_id(&env, &wallet.account_id).await?.as_deref(), Some("mende"));
    Ok(())
}

#[tokio::test]
async fn sponsored_link_takes_a_handle_back_from_a_squatter() -> anyhow::Result<()> {
    let env = setup().await?;
    link(&env.bob, &env, "mende").await?.into_result()?; // bob squats

    let wallet = app_wallet();
    let args = signed_link_args(&env, &wallet, "mende").await?;
    let res = submit(&env.sponsor, &env, "link_signed", &args).await?;
    assert!(res.is_success(), "{res:#?}");

    assert_eq!(account_of(&env, "mende").await?.as_deref(), Some(wallet.account_id.as_str()));
    assert_eq!(bonuz_of(&env, &env.bob).await?, None);
    let unlink_log = res
        .logs()
        .into_iter()
        .find(|l| l.contains("\"event\":\"unlink\""))
        .expect("squatter's link removed with an unlink event");
    assert!(unlink_log.contains(env.bob.id().as_str()));
    assert!(unlink_log.contains("\"removed_by\""));
    Ok(())
}

#[tokio::test]
async fn signed_unlink_refunds_the_sponsor() -> anyhow::Result<()> {
    let env = setup().await?;
    let wallet = app_wallet();
    let empty = env.contract.view_account().await?.storage_usage;
    let args = signed_link_args(&env, &wallet, "mende").await?;
    submit(&env.sponsor, &env, "link_signed", &args).await?.into_result()?;

    let message: String = env
        .contract
        .view("signed_unlink_message")
        .args_json(json!({ "account_id": wallet.account_id }))
        .await?
        .json()?;
    let (nonce, signature) = nep413_sign(&wallet.key, &message, env.contract.id().as_str());
    let before = env.sponsor.view_account().await?.balance;
    let res = submit(
        &env.sponsor,
        &env,
        "unlink_signed",
        &json!({ "account_id": wallet.account_id, "nonce": nonce, "signature": signature }),
    )
    .await?;
    assert!(res.is_success(), "{res:#?}");
    let after = env.sponsor.view_account().await?.balance;

    assert_eq!(bonuz_of_id(&env, &wallet.account_id).await?, None);
    assert_eq!(account_of(&env, "mende").await?, None);
    // Only the replay counter stays (so old signatures stay dead).
    let left = env.contract.view_account().await?.storage_usage - empty;
    assert!(left > 0 && left < 150, "only the counter record should remain, {left} bytes left");
    // The sponsor got the link storage back, more than the gas it spent.
    assert!(after > before, "sponsor balance {before} -> {after}");
    Ok(())
}

#[tokio::test]
async fn only_the_owner_can_change_the_sponsor() -> anyhow::Result<()> {
    let env = setup().await?;
    let set = |by: &Account, who: Value| {
        by.call(env.contract.id(), "set_sponsor")
            .args_json(json!({ "sponsor_id": who }))
            .deposit(ONE_YOCTO)
            .transact()
    };
    let res = set(&env.alice, json!(env.alice.id())).await?;
    assert!(format!("{res:?}").contains("only the registry owner"));

    set(&env.owner, json!(env.alice.id())).await?.into_result()?;
    let sponsor: Option<String> = env.contract.view("sponsor").await?.json()?;
    assert_eq!(sponsor.as_deref(), Some(env.alice.id().as_str()));

    // The old sponsor is locked out.
    let wallet = app_wallet();
    let args = signed_link_args(&env, &wallet, "mende").await?;
    let res = submit(&env.sponsor, &env, "link_signed", &args).await?;
    assert!(format!("{res:?}").contains("only the bonuz sponsor account"));
    Ok(())
}
