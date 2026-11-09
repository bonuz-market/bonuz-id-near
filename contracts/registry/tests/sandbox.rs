//! End-to-end tests against a local NEAR sandbox node (near-workspaces), with
//! the real wasm. The exact time, cap and relink rules are pinned by the unit
//! tests in `src/tests.rs`; these prove the deployed shape works:
//! the registrar signs with a function-call key that cannot attach deposits,
//! the contract pays storage from its own balance, and the user's implicit
//! account never exists on chain. They also print gas and storage per call.

use bonuz_id_registry::proof::{build_link_message, nep413_preimage};
use ed25519_dalek::{Signer, SigningKey};
use near_workspaces::network::Sandbox;
use near_workspaces::types::{AccessKey, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, Contract, Worker};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);

struct Env {
    sandbox: Worker<Sandbox>,
    contract: Contract,
    owner: Account,
    /// The registrar account, acting through a function-call key only.
    registrar_fc: Account,
}

async fn setup(reserve_near: u128) -> anyhow::Result<Env> {
    let wasm = near_workspaces::compile_project("./").await?;
    let sandbox = near_workspaces::sandbox().await?;
    let root = sandbox.root_account()?;
    let contract = root
        .create_subaccount("registry")
        .initial_balance(NearToken::from_near(10))
        .transact()
        .await?
        .into_result()?
        .deploy(&wasm)
        .await?
        .into_result()?;
    let owner = root
        .create_subaccount("owner")
        .initial_balance(NearToken::from_near(5))
        .transact()
        .await?
        .into_result()?;
    let registrar = root
        .create_subaccount("registrar")
        .initial_balance(NearToken::from_near(5))
        .transact()
        .await?
        .into_result()?;

    // Function-call key: this contract, two methods, 2 NEAR gas allowance.
    let fc_key = SecretKey::from_random(KeyType::ED25519);
    registrar
        .batch(registrar.id())
        .add_key(
            fc_key.public_key(),
            AccessKey::function_call_access(
                contract.id(),
                &["link", "unlink_by_registrar"],
                Some(NearToken::from_near(2)),
            ),
        )
        .transact()
        .await?
        .into_result()?;
    let registrar_fc = Account::from_secret_key(registrar.id().clone(), fc_key, &sandbox);

    contract
        .call("new")
        .args_json(json!({
            "owner": owner.id(),
            "registrar": registrar.id(),
            "network": "testnet",
            "allow_registrar_checked": true,
            "reserve": NearToken::from_near(reserve_near),
        }))
        .transact()
        .await?
        .into_result()?;
    Ok(Env { sandbox, contract, owner, registrar_fc })
}

/// An app-style wallet: a fresh ed25519 key whose implicit account was never funded.
struct Wallet(SigningKey);

impl Wallet {
    fn fresh() -> Self {
        Self(SigningKey::generate(&mut rand::rngs::OsRng))
    }
    fn account(&self) -> String {
        hex::encode(self.0.verifying_key().to_bytes())
    }
    fn public_key(&self) -> String {
        format!("ed25519:{}", bs58::encode(self.0.verifying_key().to_bytes()).into_string())
    }
    /// The args for `link`, signed like the app signs.
    fn link_args(&self, handle: &str, recipient: &str, expires: u64) -> Value {
        let message = build_link_message(&self.account(), handle, "testnet", expires);
        let nonce: [u8; 32] = rand::random();
        let hash = Sha256::digest(nep413_preimage(&message, &nonce, recipient));
        let b64 = |b: &[u8]| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b);
        json!({
            "account_id": self.account(),
            "bonuz_id": handle,
            "public_key": self.public_key(),
            "nonce": b64(&nonce),
            "signature": b64(&self.0.sign(&hash).to_bytes()),
            "message": message,
        })
    }
}

async fn chain_now_s(env: &Env) -> anyhow::Result<u64> {
    Ok(env.sandbox.view_block().await?.timestamp() / 1_000_000_000)
}

fn events(logs: &[&str]) -> Vec<Value> {
    logs.iter()
        .filter_map(|l| l.strip_prefix("EVENT_JSON:"))
        .map(|j| serde_json::from_str(j).unwrap())
        .collect()
}

#[tokio::test]
async fn one_click_link_for_an_unfunded_app_wallet() -> anyhow::Result<()> {
    let env = setup(1).await?;
    let w = Wallet::fresh();
    assert!(env.sandbox.view_account(&w.account().parse()?).await.is_err(), "account must not exist");

    let before = env.contract.view_account().await?;
    let now = chain_now_s(&env).await?;
    let res = env
        .registrar_fc
        .call(env.contract.id(), "link")
        .args_json(w.link_args("mende", env.contract.id().as_str(), now + 480))
        .gas(near_workspaces::types::Gas::from_tgas(30))
        .transact()
        .await?;
    assert!(res.is_success(), "{res:#?}");
    let after = env.contract.view_account().await?;

    let bonuz: Option<String> = env
        .contract
        .view("bonuz_of")
        .args_json(json!({ "account_id": w.account() }))
        .await?
        .json()?;
    assert_eq!(bonuz.as_deref(), Some("mende"));
    let account: Option<String> =
        env.contract.view("account_of").args_json(json!({ "bonuz_id": "mende" })).await?.json()?;
    assert_eq!(account.as_deref(), Some(w.account().as_str()));
    let link: Value = env
        .contract
        .view("link_of")
        .args_json(json!({ "account_id": w.account() }))
        .await?
        .json()?;
    assert_eq!(link["checked_by"], "contract");

    let ev = events(&res.logs());
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["standard"], "bonuz_id_near");
    assert_eq!(ev[0]["event"], "link");

    // The user's account still does not exist: nothing was charged to it.
    assert!(env.sandbox.view_account(&w.account().parse()?).await.is_err());
    // The contract paid the storage; nothing was attached.
    let bytes = after.storage_usage - before.storage_usage;
    println!(
        "MEASURE link: gas burnt {} Tgas, storage +{} bytes ({} yoctoNEAR)",
        res.total_gas_burnt.as_gas() as f64 / 1e12,
        bytes,
        u128::from(bytes) * 10u128.pow(19)
    );
    assert!(bytes > 0 && bytes < 600);

    // Remove it again through the same function-call key.
    let res = env
        .registrar_fc
        .call(env.contract.id(), "unlink_by_registrar")
        .args_json(json!({ "account_id": w.account() }))
        .transact()
        .await?;
    assert!(res.is_success(), "{res:#?}");
    println!("MEASURE unlink_by_registrar: gas burnt {} Tgas", res.total_gas_burnt.as_gas() as f64 / 1e12);
    assert_eq!(events(&res.logs())[0]["data"]["reason"], "registrar");
    assert!(res.json::<bool>()?);
    Ok(())
}

#[tokio::test]
async fn function_call_key_cannot_reach_owner_methods_and_replay_fails() -> anyhow::Result<()> {
    let env = setup(1).await?;
    let w = Wallet::fresh();
    let now = chain_now_s(&env).await?;
    let args = w.link_args("mende", env.contract.id().as_str(), now + 480);

    let first = env.registrar_fc.call(env.contract.id(), "link").args_json(&args).transact().await?;
    assert!(first.is_success(), "{first:#?}");
    let replay = env.registrar_fc.call(env.contract.id(), "link").args_json(&args).transact().await?;
    assert!(format!("{:?}", replay.into_result().unwrap_err()).contains("nonce was already used"));

    // The function-call key is limited to link and unlink_by_registrar.
    let pause = env
        .registrar_fc
        .call(env.contract.id(), "set_paused")
        .args_json(json!({ "paused": true }))
        .deposit(ONE_YOCTO)
        .transact()
        .await;
    assert!(pause.is_err() || !pause.unwrap().is_success());

    // The owner can pause; then the registrar cannot link.
    env.owner
        .call(env.contract.id(), "set_paused")
        .args_json(json!({ "paused": true }))
        .deposit(ONE_YOCTO)
        .transact()
        .await?
        .into_result()?;
    let w2 = Wallet::fresh();
    let res = env
        .registrar_fc
        .call(env.contract.id(), "link")
        .args_json(w2.link_args("other", env.contract.id().as_str(), now + 480))
        .transact()
        .await?;
    assert!(format!("{:?}", res.into_result().unwrap_err()).contains("paused"));
    Ok(())
}

#[tokio::test]
async fn link_refused_when_the_contract_balance_is_under_its_reserve() -> anyhow::Result<()> {
    let env = setup(50).await?; // contract holds 10 NEAR, reserve 50
    let w = Wallet::fresh();
    let now = chain_now_s(&env).await?;
    let res = env
        .registrar_fc
        .call(env.contract.id(), "link")
        .args_json(w.link_args("mende", env.contract.id().as_str(), now + 480))
        .transact()
        .await?;
    assert!(format!("{:?}", res.into_result().unwrap_err()).contains("under its reserve"));
    let status: Value = env.contract.view("status").await?.json()?;
    assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(status["unlink_daily_cap"], 100);
    Ok(())
}
