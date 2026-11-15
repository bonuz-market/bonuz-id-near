//! Runs the shared vectors in `packages/near-link/vectors.json` through the
//! contract's own proof code (`proof::check_link`). The TypeScript package runs
//! the same file, so the two implementations must agree on every case.

use base64::Engine as _;
use bonuz_id_registry::proof::{check_link, Context, LinkInput};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[test]
fn shared_vectors() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/near-link/vectors.json");
    let file: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let cases = file["cases"].as_array().unwrap();
    assert!(cases.len() >= 20, "expected the full vector set");
    let b64 = base64::engine::general_purpose::STANDARD;

    for c in cases {
        let name = c["name"].as_str().unwrap();
        let i = &c["input"];
        let nonce = b64.decode(i["nonce"].as_str().unwrap()).unwrap();
        let signature = b64.decode(i["signature"].as_str().unwrap()).unwrap();
        let input = LinkInput {
            account_id: i["accountId"].as_str().unwrap(),
            bonuz_id: i["bonuzId"].as_str().unwrap(),
            public_key: i["publicKey"].as_str().unwrap(),
            nonce: &nonce,
            signature: &signature,
            message: i["message"].as_str().unwrap(),
        };
        let x = &c["context"];
        let ctx = Context {
            recipient: x["recipient"].as_str().unwrap(),
            network: x["network"].as_str().unwrap(),
            now_ns: x["now_ns"].as_str().unwrap().parse().unwrap(),
            allow_registrar_checked: x["allow_registrar_checked"].as_bool().unwrap(),
        };
        let got = check_link(
            &input,
            &ctx,
            |d| Sha256::digest(d).into(),
            |sig, hash, key| {
                VerifyingKey::from_bytes(key)
                    .map(|k| k.verify(hash, &Signature::from_bytes(sig)).is_ok())
                    .unwrap_or(false)
            },
        );
        let e = &c["expect"];
        if e["ok"].as_bool().unwrap() {
            let checked = got.unwrap_or_else(|err| panic!("{name}: expected ok, got {err:?}"));
            assert_eq!(checked.checked_by.as_str(), e["checked_by"].as_str().unwrap(), "{name}");
            assert_eq!(checked.bonuz_eoa, e["bonuz_eoa"].as_str(), "{name}");
        } else {
            assert_eq!(got.map(|_| ()), Err(e["error"].as_str().unwrap()), "{name}");
        }
    }
}
