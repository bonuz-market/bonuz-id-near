//! Acts like the bonuz app wallet, to try the one-click path by hand.
//!
//!   cargo run --example app_wallet -- new  <key-file>
//!       Makes a fresh ed25519 key (a NEAR implicit account), saves its 32-byte
//!       seed as hex to <key-file> (owner-only), prints the account id.
//!
//!   cargo run --example app_wallet -- sign <key-file> <message> <recipient>
//!       Signs <message> with NEP-413 for <recipient> and prints the JSON fields
//!       `account_id`, `nonce`, `signature` that link_signed / unlink_signed take.
//!
//! Testnet only. The key file is a throwaway test key; never use it for real funds.

use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use std::{env, fs};

#[derive(borsh::BorshSerialize)]
struct Nep413Payload {
    message: String,
    nonce: [u8; 32],
    recipient: String,
    callback_url: Option<String>,
}

fn load(path: &str) -> SigningKey {
    let hex_seed = fs::read_to_string(path).expect("read key file");
    let seed: [u8; 32] = hex::decode(hex_seed.trim())
        .expect("key file is hex")
        .try_into()
        .expect("key file holds 32 bytes");
    SigningKey::from_bytes(&seed)
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["new", path] => {
            let key = SigningKey::generate(&mut rand::rngs::OsRng);
            fs::write(path, hex::encode(key.to_bytes())).expect("write key file");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            println!("{}", hex::encode(key.verifying_key().to_bytes()));
        }
        ["sign", path, message, recipient] => {
            let key = load(path);
            let nonce: [u8; 32] = rand::random();
            let payload = Nep413Payload {
                message: message.to_string(),
                nonce,
                recipient: recipient.to_string(),
                callback_url: None,
            };
            let mut data = ((1u32 << 31) + 413).to_le_bytes().to_vec();
            data.extend(borsh::to_vec(&payload).unwrap());
            let signature = key.sign(&Sha256::digest(&data));
            let b64 = base64::engine::general_purpose::STANDARD;
            println!(
                "{}",
                serde_json::json!({
                    "account_id": hex::encode(key.verifying_key().to_bytes()),
                    "nonce": b64.encode(nonce),
                    "signature": b64.encode(signature.to_bytes()),
                })
            );
        }
        _ => {
            eprintln!("usage: app_wallet new <key-file> | app_wallet sign <key-file> <message> <recipient>");
            std::process::exit(2);
        }
    }
}
