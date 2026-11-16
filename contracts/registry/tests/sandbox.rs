//! End-to-end tests against a local NEAR sandbox node (near-workspaces), with
//! the real wasm. The exact time, cap and relink rules are pinned by the unit
//! tests in `src/tests.rs`; these prove the deployed shape works:
//! the registrar signs with a function-call key that cannot attach deposits,
//! the contract pays storage from its own balance, and the user's implicit
//! account never exists on chain. They also print gas and storage per call.
//! 2.2.0: a named account confirms a pending claim itself (`accept_link`), and
//! a live 2.1.0 registry upgrades in one transaction (deploy + migrate).

use bonuz_id_registry::proof::{build_link_message, build_wallet_link_message, nep413_preimage};
use ed25519_dalek::{Signer, SigningKey};
use near_workspaces::network::Sandbox;
use near_workspaces::types::{AccessKey, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, Contract, Worker};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
/// What the registrar's function-call key may call from 2.2.0.
const REGISTRAR_METHODS: &[&str] =
    &["link", "unlink_by_registrar", "propose_link", "cancel_link_proposal", "set_handle", "adopt_legacy_link"];
const V2_1_WASM: &str = "res/bonuz_id_registry-2.1.0.wasm";

/// A stable test creation wallet per handle.
fn eoa(handle: &str) -> String {
    format!("0x{}", &hex::encode(Sha256::digest(handle.as_bytes()))[..40])
}

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
                REGISTRAR_METHODS,
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
    /// The args for 2.2.0 `link`, signed like the app signs: the message names
    /// the wallet (line 4) and the call repeats it in `bonuz_eoa`.
    fn link_args(&self, handle: &str, recipient: &str, expires: u64) -> Value {
        let message = build_wallet_link_message(&self.account(), handle, "testnet", expires, &eoa(handle));
        let mut args = self.signed_args(handle, recipient, message);
        args["bonuz_eoa"] = json!(eoa(handle));
        args
    }
    /// The 2.1.0 form: three-line message, no `bonuz_eoa`.
    fn link_args_v2_1(&self, handle: &str, recipient: &str, expires: u64) -> Value {
        let message = build_link_message(&self.account(), handle, "testnet", expires);
        self.signed_args(handle, recipient, message)
    }
    fn signed_args(&self, handle: &str, recipient: &str, message: String) -> Value {
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

    let ev: Vec<Value> = events(&res.logs()).into_iter().filter(|e| e["event"] == "link").collect();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["data"]["bonuz_eoa"], eoa("mende"));
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
    assert!(bytes > 0 && bytes < 1000);

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

    // The function-call key is limited to the registrar methods.
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

async fn view(contract: &Contract, method: &str, args: Value) -> anyhow::Result<Value> {
    Ok(contract.view(method).args_json(args).await?.json()?)
}

#[tokio::test]
async fn named_account_links_only_after_it_accepts_itself() -> anyhow::Result<()> {
    let env = setup(1).await?;
    let root = env.sandbox.root_account()?;
    let alice = root.create_subaccount("alice").initial_balance(NearToken::from_near(2)).transact().await?.into_result()?;
    let mallory = root.create_subaccount("mallory").initial_balance(NearToken::from_near(2)).transact().await?.into_result()?;
    let user = eoa("mende");

    let before = env.contract.view_account().await?;
    let res = env
        .registrar_fc
        .call(env.contract.id(), "propose_link")
        .args_json(json!({ "account_id": alice.id(), "bonuz_eoa": user, "bonuz_id": "mende" }))
        .gas(near_workspaces::types::Gas::from_tgas(30))
        .transact()
        .await?;
    assert!(res.is_success(), "{res:#?}");
    let mid = env.contract.view_account().await?;
    println!(
        "MEASURE propose_link: gas burnt {} Tgas, storage +{} bytes",
        res.total_gas_burnt.as_gas() as f64 / 1e12,
        mid.storage_usage - before.storage_usage
    );
    let pending = view(&env.contract, "link_proposal_of", json!({ "account_id": alice.id() })).await?;
    assert_eq!(pending["bonuz_eoa"], user);
    assert_eq!(pending["bonuz_id"], "mende");
    assert!(view(&env.contract, "bonuz_of", json!({ "account_id": alice.id() })).await?.is_null());

    // Another account cannot accept alice's claim, nor can alice accept without a deposit.
    let res = mallory.call(env.contract.id(), "accept_link").args_json(json!({ "bonuz_eoa": user })).deposit(ONE_YOCTO).transact().await?;
    assert!(format!("{:?}", res.into_result().unwrap_err()).contains("no pending link proposal"));
    let res = alice.call(env.contract.id(), "accept_link").args_json(json!({ "bonuz_eoa": user })).transact().await?;
    assert!(format!("{:?}", res.into_result().unwrap_err()).contains("1 yoctoNEAR"));
    // The registrar's function-call key cannot accept for alice (it is not alice).
    let res = env.registrar_fc.call(env.contract.id(), "accept_link").args_json(json!({ "bonuz_eoa": user })).deposit(ONE_YOCTO).transact().await;
    assert!(res.is_err() || !res.unwrap().is_success());

    let res = alice
        .call(env.contract.id(), "accept_link")
        .args_json(json!({ "bonuz_eoa": user }))
        .deposit(ONE_YOCTO)
        .transact()
        .await?;
    assert!(res.is_success(), "{res:#?}");
    let after = env.contract.view_account().await?;
    println!(
        "MEASURE accept_link: gas burnt {} Tgas, storage {:+} bytes",
        res.total_gas_burnt.as_gas() as f64 / 1e12,
        after.storage_usage as i64 - mid.storage_usage as i64
    );
    let link = view(&env.contract, "link_of", json!({ "account_id": alice.id() })).await?;
    assert_eq!(link["checked_by"], "account");
    assert_eq!(link["bonuz_eoa"], user);
    assert_eq!(link["public_key"], alice.secret_key().public_key().to_string());
    assert_eq!(view(&env.contract, "account_of", json!({ "bonuz_id": "mende" })).await?, json!(alice.id()));
    assert_eq!(view(&env.contract, "account_of_eoa", json!({ "bonuz_eoa": user })).await?, json!(alice.id()));

    // A rename keeps the link.
    let res = env
        .registrar_fc
        .call(env.contract.id(), "set_handle")
        .args_json(json!({ "bonuz_eoa": user, "bonuz_id": "matthias" }))
        .transact()
        .await?;
    assert!(res.is_success(), "{res:#?}");
    assert_eq!(view(&env.contract, "bonuz_of", json!({ "account_id": alice.id() })).await?, json!("matthias"));
    assert!(view(&env.contract, "account_of", json!({ "bonuz_id": "mende" })).await?.is_null());

    // alice removes it herself.
    let res = alice.call(env.contract.id(), "unlink_by_owner").deposit(ONE_YOCTO).transact().await?;
    assert!(res.is_success(), "{res:#?}");
    assert!(view(&env.contract, "link_of", json!({ "account_id": alice.id() })).await?.is_null());
    Ok(())
}

/// Deploys the frozen 2.1.0 wasm (the one live on mainnet), writes 2.1.0 links,
/// then upgrades to this build with deploy + migrate in ONE transaction.
#[tokio::test]
async fn upgrade_from_2_1_with_migrate_keeps_every_link() -> anyhow::Result<()> {
    let old_wasm = std::fs::read(V2_1_WASM)?;
    let new_wasm = near_workspaces::compile_project("./").await?;
    let sandbox = near_workspaces::sandbox().await?;
    let root = sandbox.root_account()?;
    let contract = root
        .create_subaccount("registry")
        .initial_balance(NearToken::from_near(10))
        .transact()
        .await?
        .into_result()?
        .deploy(&old_wasm)
        .await?
        .into_result()?;
    let registrar = root.create_subaccount("registrar").initial_balance(NearToken::from_near(5)).transact().await?.into_result()?;
    contract
        .call("new")
        .args_json(json!({
            "owner": root.id(), "registrar": registrar.id(), "network": "testnet",
            "allow_registrar_checked": false, "reserve": NearToken::from_near(1),
        }))
        .transact()
        .await?
        .into_result()?;
    let status: Value = contract.view("status").await?.json()?;
    assert_eq!(status["version"], "2.1.0");

    let now = sandbox.view_block().await?.timestamp() / 1_000_000_000;
    let (a, b, c) = (Wallet::fresh(), Wallet::fresh(), Wallet::fresh());
    // b's call already carries bonuz_eoa, as an updated backend sends it: 2.1.0 must ignore it.
    let mut b_args = b.link_args_v2_1("olls", contract.id().as_str(), now + 480);
    b_args["bonuz_eoa"] = json!(eoa("olls"));
    for args in [a.link_args_v2_1("mende", contract.id().as_str(), now + 480), b_args] {
        registrar
            .call(contract.id(), "link")
            .args_json(args)
            .transact()
            .await?
            .into_result()?;
    }
    // 2.1.0 refuses the four-line 2.2.0 message: the backend must switch formats
    // with the upgrade (it reads status().version).
    let res = registrar.call(contract.id(), "link").args_json(c.link_args("fresh", contract.id().as_str(), now + 480)).transact().await?;
    assert!(format!("{:?}", res.into_result().unwrap_err()).contains("exactly three lines"));

    let res = contract
        .as_account()
        .batch(contract.id())
        .deploy(&new_wasm)
        .call(near_workspaces::operations::Function::new("migrate").args_json(json!({})).gas(near_workspaces::types::Gas::from_tgas(100)))
        .transact()
        .await?;
    assert!(res.is_success(), "{res:#?}");
    println!("MEASURE deploy+migrate: gas burnt {} Tgas", res.total_gas_burnt.as_gas() as f64 / 1e12);

    let status: Value = contract.view("status").await?.json()?;
    assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(status["links"], "2");
    assert_eq!(status["legacy_links"], "2");
    assert_eq!(status["allow_registrar_checked"], false);
    assert_eq!(status["owner"], json!(root.id()));
    for (w, handle) in [(&a, "mende"), (&b, "olls")] {
        assert_eq!(view(&contract, "bonuz_of", json!({ "account_id": w.account() })).await?, json!(handle));
        assert_eq!(view(&contract, "account_of", json!({ "bonuz_id": handle })).await?, json!(w.account()));
        let link = view(&contract, "link_of", json!({ "account_id": w.account() })).await?;
        assert!(link["bonuz_eoa"].is_null());
        assert_eq!(link["checked_by"], "contract");
    }

    // A second migrate fails and changes nothing.
    let again = contract.call("migrate").args_json(json!({})).transact().await?;
    assert!(again.into_result().is_err());

    // Adopt one, unlink the other.
    registrar
        .call(contract.id(), "adopt_legacy_link")
        .args_json(json!({ "account_id": a.account(), "bonuz_eoa": eoa("mende") }))
        .transact()
        .await?
        .into_result()?;
    let link = view(&contract, "link_of", json!({ "account_id": a.account() })).await?;
    assert_eq!(link["bonuz_eoa"], eoa("mende"));
    assert_eq!(link["bonuz_id"], "mende");
    registrar.call(contract.id(), "unlink_by_registrar").args_json(json!({ "account_id": b.account() })).transact().await?.into_result()?;
    let status: Value = contract.view("status").await?.json()?;
    assert_eq!((status["links"].as_str(), status["legacy_links"].as_str()), (Some("1"), Some("0")));

    // From 2.2.0 the three-line message is refused, the four-line one links the signed wallet.
    let now = sandbox.view_block().await?.timestamp() / 1_000_000_000;
    let mut old_form = c.link_args_v2_1("fresh", contract.id().as_str(), now + 480);
    old_form["bonuz_eoa"] = json!(eoa("fresh"));
    let res = registrar.call(contract.id(), "link").args_json(old_form).transact().await?;
    assert!(format!("{:?}", res.into_result().unwrap_err()).contains("must name the bonuz wallet"));
    registrar
        .call(contract.id(), "link")
        .args_json(c.link_args("fresh", contract.id().as_str(), now + 480))
        .transact()
        .await?
        .into_result()?;
    assert_eq!(view(&contract, "account_of_eoa", json!({ "bonuz_eoa": eoa("fresh") })).await?, json!(c.account()));
    Ok(())
}

/// A bare code deploy over 2.1.0 state (no migrate) leaves the registry unable
/// to read its state. This is what deploy + migrate in one transaction prevents.
#[tokio::test]
async fn upgrade_without_migrate_would_break_reads() -> anyhow::Result<()> {
    let old_wasm = std::fs::read(V2_1_WASM)?;
    let new_wasm = near_workspaces::compile_project("./").await?;
    let sandbox = near_workspaces::sandbox().await?;
    let root = sandbox.root_account()?;
    let contract = root.create_subaccount("registry").initial_balance(NearToken::from_near(10)).transact().await?.into_result()?.deploy(&old_wasm).await?.into_result()?;
    contract
        .call("new")
        .args_json(json!({
            "owner": root.id(), "registrar": root.id(), "network": "testnet",
            "allow_registrar_checked": false, "reserve": NearToken::from_near(1),
        }))
        .transact()
        .await?
        .into_result()?;
    contract.as_account().deploy(&new_wasm).await?.into_result()?;
    assert!(contract.view("status").await.is_err());
    // migrate repairs it.
    contract.call("migrate").args_json(json!({})).transact().await?.into_result()?;
    let status: Value = contract.view("status").await?.json()?;
    assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
    Ok(())
}
