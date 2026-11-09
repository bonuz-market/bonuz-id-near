//! Sandbox tests for the bonuz ID registry, run against a local NEAR node via
//! near-workspaces. `cargo test` builds the contract to wasm first.

use near_workspaces::types::NearToken;
use near_workspaces::{Account, Contract};
use serde_json::{json, Value};

const DEPOSIT: NearToken = NearToken::from_millinear(10); // 0.01 NEAR, far more than needed

struct Env {
    contract: Contract,
    owner: Account,
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
    contract
        .call("new")
        .args_json(json!({ "owner_id": owner.id() }))
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
    Ok(Env { contract, owner, alice, bob })
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
    assert_eq!(ev["data"][0]["account_id"], env.alice.id().as_str());
    assert_eq!(ev["data"][0]["bonuz_id"], "mende");
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
    assert_eq!(ev["data"][0]["previous_bonuz_id"], "mende");

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
    assert_eq!(ev["data"][0]["bonuz_id"], "mende");

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
    assert_eq!(ev["data"][0]["account_id"], env.bob.id().as_str());
    assert_eq!(ev["data"][0]["removed_by"], env.owner.id().as_str());

    assert_eq!(account_of(&env, "mende").await?, None);
    assert_eq!(bonuz_of(&env, &env.bob).await?, None);

    // The real owner of the handle can now link it.
    link(&env.alice, &env, "mende").await?.into_result()?;
    assert_eq!(account_of(&env, "mende").await?.as_deref(), Some(env.alice.id().as_str()));
    Ok(())
}
