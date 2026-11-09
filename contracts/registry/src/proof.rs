//! The proof rules, kept free of NEAR runtime calls so the contract, the unit
//! tests and the shared test vectors (`packages/near-link/vectors.json`) all
//! run the exact same code. The contract passes in the runtime's sha256 and
//! ed25519 functions; the tests pass in `sha2` and `ed25519-dalek`.

/// NEP-413 tag: 2^31 + 413, written little-endian before the payload.
pub const NEP413_TAG: u32 = (1 << 31) + 413;

/// How far ahead a message may expire, measured from the block time.
pub const MAX_EXPIRY_AHEAD_SECONDS: u64 = 10 * 60;

/// Longest bonuz ID handle the registry accepts.
pub const MAX_HANDLE_LEN: usize = 64;

const LINE1_PREFIX: &str = "Link NEAR account ";
const LINE1_MIDDLE: &str = " to bonuz ID @";
const LINE2_PREFIX: &str = "Network: ";
const LINE3_PREFIX: &str = "Expires: ";

/// Who checked that the signing key belongs to the NEAR account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckedBy {
    /// Implicit account: the account id is the key, checked here.
    Contract,
    /// Named account: bonuz checked the key over RPC before calling.
    Registrar,
}

impl CheckedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckedBy::Contract => "contract",
            CheckedBy::Registrar => "registrar",
        }
    }
}

/// The three values a link message names.
#[derive(Debug, PartialEq, Eq)]
pub struct LinkMessage<'a> {
    pub account_id: &'a str,
    pub bonuz_id: &'a str,
    pub network: &'a str,
    pub expires: u64,
}

/// What a `link` call carries.
pub struct LinkInput<'a> {
    pub account_id: &'a str,
    pub bonuz_id: &'a str,
    /// `ed25519:<base58>`
    pub public_key: &'a str,
    pub nonce: &'a [u8],
    pub signature: &'a [u8],
    pub message: &'a str,
}

/// Where and when the check runs.
pub struct Context<'a> {
    /// NEP-413 recipient: the registry contract's own account id.
    pub recipient: &'a str,
    /// `testnet` or `mainnet`, fixed at init.
    pub network: &'a str,
    /// Block time in nanoseconds.
    pub now_ns: u64,
    /// When false, only implicit accounts can be linked.
    pub allow_registrar_checked: bool,
}

/// One rule for both "is this message expired" and "may this nonce be pruned".
/// A message expiring at second `expires` is valid while `now < expires`.
pub fn is_expired(expires: u64, now_ns: u64) -> bool {
    u128::from(now_ns) >= u128::from(expires) * 1_000_000_000
}

/// The text the app shows and signs. Inputs must already be canonical
/// (lowercase); `check_link` refuses anything else.
pub fn build_link_message(account_id: &str, bonuz_id: &str, network: &str, expires: u64) -> String {
    format!("{LINE1_PREFIX}{account_id}{LINE1_MIDDLE}{bonuz_id}\n{LINE2_PREFIX}{network}\n{LINE3_PREFIX}{expires}")
}

/// Parses the exact three-line message. Lines are separated by `\n` only;
/// `\r`, any other control character, a trailing newline, extra lines or
/// extra text are refused.
pub fn parse_link_message(message: &str) -> Result<LinkMessage<'_>, &'static str> {
    if message.chars().any(|c| c.is_control() && c != '\n') {
        return Err("message contains a control character");
    }
    let mut lines = message.split('\n');
    let (l1, l2, l3) = match (lines.next(), lines.next(), lines.next(), lines.next()) {
        (Some(a), Some(b), Some(c), None) => (a, b, c),
        _ => return Err("message must have exactly three lines"),
    };
    let rest = l1.strip_prefix(LINE1_PREFIX).ok_or("line 1 must start with 'Link NEAR account '")?;
    let (account_id, bonuz_id) = rest
        .split_once(LINE1_MIDDLE)
        .ok_or("line 1 must read 'Link NEAR account <account> to bonuz ID @<handle>'")?;
    let network = l2.strip_prefix(LINE2_PREFIX).ok_or("line 2 must start with 'Network: '")?;
    let expires_text = l3.strip_prefix(LINE3_PREFIX).ok_or("line 3 must start with 'Expires: '")?;
    let expires = parse_seconds(expires_text).ok_or("Expires must be unix seconds in plain digits")?;
    if account_id.is_empty() || account_id.contains(' ') || bonuz_id.is_empty() || bonuz_id.contains(' ') {
        return Err("line 1 must name one account and one handle");
    }
    Ok(LinkMessage { account_id, bonuz_id, network, expires })
}

/// Plain ASCII digits, no sign, no leading zero, fits in u64.
fn parse_seconds(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > 19 || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return None;
    }
    text.parse().ok()
}

/// bonuz ID handle in canonical form: 1 to 64 chars of `a-z 0-9 _ . -`.
pub fn check_bonuz_id(bonuz_id: &str) -> Result<(), &'static str> {
    if bonuz_id.is_empty() || bonuz_id.len() > MAX_HANDLE_LEN {
        return Err("bonuz ID must be 1 to 64 characters");
    }
    if !bonuz_id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'))
    {
        return Err("bonuz ID may only contain a-z, 0-9, _, . and - (lowercase)");
    }
    Ok(())
}

/// The ed25519 key of a NEAR implicit account (exactly 64 lowercase hex chars).
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

/// `ed25519:<base58 of 32 bytes>` to raw bytes.
pub fn parse_public_key(text: &str) -> Result<[u8; 32], &'static str> {
    let b58 = text.strip_prefix("ed25519:").ok_or("public key must be ed25519:<base58>")?;
    let raw = bs58::decode(b58).into_vec().map_err(|_| "public key is not valid base58")?;
    raw.try_into().map_err(|_| "public key must be 32 bytes")
}

/// The bytes NEP-413 hashes: tag, then borsh(message, nonce, recipient, callback_url = None).
pub fn nep413_preimage(message: &str, nonce: &[u8; 32], recipient: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 4 + message.len() + 32 + 4 + recipient.len() + 1);
    out.extend_from_slice(&NEP413_TAG.to_le_bytes());
    out.extend_from_slice(&(message.len() as u32).to_le_bytes());
    out.extend_from_slice(message.as_bytes());
    out.extend_from_slice(nonce);
    out.extend_from_slice(&(recipient.len() as u32).to_le_bytes());
    out.extend_from_slice(recipient.as_bytes());
    out.push(0); // callback_url: None
    out
}

/// Checks everything about a link that does not need contract state: the
/// signature, the message text against the call arguments, network, expiry
/// and the key-to-account rule. Nonce reuse and the one-to-one rules are the
/// contract's job.
pub fn check_link(
    input: &LinkInput,
    ctx: &Context,
    sha256: impl Fn(&[u8]) -> [u8; 32],
    ed25519_verify: impl Fn(&[u8; 64], &[u8; 32], &[u8; 32]) -> bool,
) -> Result<(CheckedBy, u64), &'static str> {
    check_bonuz_id(input.bonuz_id)?;
    if input.account_id.starts_with("0x") {
        return Err("Ethereum-style NEAR accounts (0x...) cannot be linked yet");
    }
    if looks_like_uppercase_implicit(input.account_id) {
        return Err("implicit accounts must be lowercase hex");
    }
    if input.account_id.parse::<near_sdk::AccountId>().is_err() {
        return Err("this is not a valid NEAR account id");
    }
    let nonce: &[u8; 32] = input.nonce.try_into().map_err(|_| "nonce must be 32 bytes")?;
    let signature: &[u8; 64] = input.signature.try_into().map_err(|_| "signature must be 64 bytes")?;
    let public_key = parse_public_key(input.public_key)?;

    let hash = sha256(&nep413_preimage(input.message, nonce, ctx.recipient));
    if !ed25519_verify(signature, &hash, &public_key) {
        return Err("invalid signature (NEP-413, recipient = this contract)");
    }

    let msg = parse_link_message(input.message)?;
    if msg.account_id != input.account_id {
        return Err("the signed message names a different NEAR account");
    }
    if msg.bonuz_id != input.bonuz_id {
        return Err("the signed message names a different bonuz ID");
    }
    if msg.network != ctx.network {
        return Err("the signed message is for a different network");
    }
    if is_expired(msg.expires, ctx.now_ns) {
        return Err("the signed message has expired");
    }
    let latest = u128::from(ctx.now_ns) + u128::from(MAX_EXPIRY_AHEAD_SECONDS) * 1_000_000_000;
    if u128::from(msg.expires) * 1_000_000_000 > latest {
        return Err("the signed message expires more than 10 minutes ahead");
    }

    let checked_by = match implicit_public_key(input.account_id) {
        Some(key) if key == public_key => CheckedBy::Contract,
        Some(_) => return Err("the key does not belong to this implicit account"),
        None if ctx.allow_registrar_checked => CheckedBy::Registrar,
        None => return Err("this registry only links implicit accounts"),
    };
    Ok((checked_by, msg.expires))
}

/// 64 hex characters with at least one uppercase letter.
fn looks_like_uppercase_implicit(account_id: &str) -> bool {
    account_id.len() == 64
        && account_id.bytes().all(|b| b.is_ascii_hexdigit())
        && account_id.bytes().any(|b| b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_parses_the_same_message() {
        let text = build_link_message("ab".repeat(32).as_str(), "mende", "testnet", 1_763_000_000);
        let acc = "ab".repeat(32);
        assert_eq!(
            parse_link_message(&text),
            Ok(LinkMessage { account_id: &acc, bonuz_id: "mende", network: "testnet", expires: 1_763_000_000 })
        );
    }

    #[test]
    fn refuses_every_mutation() {
        let good = build_link_message("alice.testnet", "mende", "testnet", 1_763_000_000);
        assert!(parse_link_message(&good).is_ok());
        let bad = [
            good.replace('\n', "\r\n"),
            format!("{good}\n"),
            format!("{good} "),
            format!(" {good}"),
            good.replace("Link NEAR", "Link  NEAR"),
            good.replace("Network: ", "Network:"),
            good.replace("Expires: ", "Expires: +"),
            good.replace("1763000000", "01763000000"),
            good.replace("1763000000", "1763000000.0"),
            good.replace("@mende", "@mende x"),
            good.replace("@mende", "\u{FF20}mende"),
            good.replace("Link", "\u{041B}ink"),
            good.replace("\nNetwork: testnet", ""),
            {
                let mut l: Vec<&str> = good.split('\n').collect();
                l.swap(1, 2);
                l.join("\n")
            },
            format!("{good}\nExtra: 1"),
            good.replace("mende", "men\u{0000}de"),
            good.replace("mende", "men\u{200B}de"),
        ];
        for m in bad {
            // Zero-width space is not a control char; it is caught later as a
            // handle that does not match the call argument or the handle rule.
            if let Ok(parsed) = parse_link_message(&m) {
                assert!(
                    parsed.bonuz_id != "mende" || parsed.account_id != "alice.testnet",
                    "accepted mutation: {m:?}"
                );
            }
        }
    }

    #[test]
    fn expiry_is_one_rule() {
        assert!(!is_expired(100, 99_999_999_999));
        assert!(is_expired(100, 100_000_000_000));
        assert!(is_expired(100, 100_000_000_001));
    }

    #[test]
    fn handle_rules() {
        assert!(check_bonuz_id("mende").is_ok());
        assert!(check_bonuz_id("a.b-c_1").is_ok());
        assert!(check_bonuz_id(&"x".repeat(64)).is_ok());
        assert!(check_bonuz_id("").is_err());
        assert!(check_bonuz_id(&"x".repeat(65)).is_err());
        assert!(check_bonuz_id("Mende").is_err());
        assert!(check_bonuz_id("me nde").is_err());
    }

    #[test]
    fn implicit_key_decodes_lowercase_hex_only() {
        assert_eq!(implicit_public_key(&"0f".repeat(32)), Some([0x0f; 32]));
        assert_eq!(implicit_public_key("alice.near"), None);
        assert_eq!(implicit_public_key(&"0F".repeat(32)), None);
    }
}
