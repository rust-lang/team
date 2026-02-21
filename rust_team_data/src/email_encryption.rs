//! This module implements the encryption scheme used to safely include private email addresses in
//! the team repository. It generates encrypted content that looks like this:
//!
//! ```text
//! encrypted+bfab2fae1acf74ed9f0df0d3f296f45a620f33ac249e35595dcdfe57ad96d01e54ff770a95111e6cc4d4c2c7f8b34feb42397e67b11d3136e380f1ca878c9a8e924de216d1253252363bbc8fa858cd3ce02bcc9c8f5142@rust-lang.invalid
//! ```
//!
//! The hex-encoded part of the email address is a concatenation of a 32-byte ephemeral x25519 public key,
//! a 24-byte random nonce and the XChaCha20Poly1305-encrypted email address. Utilities are provided
//! to both encrypt and decrypt.

use chacha20poly1305::aead::{Aead, NewAead};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hex::{FromHex, ToHex};
use std::convert::TryInto;
use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};

const PREFIX: &str = "encrypted+";
const SUFFIX: &str = "@rust-lang.invalid";
const KEY_LENGTH: usize = 32;
const NONCE_LENGTH: usize = 24;
// TODO ask an infra admin to generate one
const PUBLIC_KEY: &str = "d1734021de0af5cfeca64482f3c38b3350a38fd4be2e6a88b2c150be4416b261";
// Globally unique context (see here for details: https://docs.rs/blake3/latest/blake3/fn.derive_key.html)
const KDF_CONTEXT: &str = "rust-team 2026-02-14 email-encryption";

fn get_public_key(public_key: &str) -> PublicKey {
    PublicKey::from(<[u8; KEY_LENGTH]>::from_hex(public_key).expect(
        "invalid public key configured, ensure it was generated with the generate-key command",
    ))
}

fn get_private_key(key: &str) -> Result<StaticSecret, Error> {
    Ok(StaticSecret::from(
        <[u8; KEY_LENGTH]>::from_hex(key).map_err(Error::Hex)?,
    ))
}

/// Encrypt an email address with x25519-dalek, with blake3 for KDF and ChaCha20Poly1305 for AEAD.
/// The encryption process follows this flow:
/// 1. an ephemeral x25519 key is generated;
/// 2. a shared secret is computed against the public key defined by an infra admin as a constant above;
/// 3. the shared secret is used with a key derivation function (blake3) to generate a uniform symmetric key;
/// 4. the symmetric key is finally used to encrypt the email;
/// 5. the hex-encoded information required for decryption (public key, nonce, encrypted email) is returned as part of a fake email address.
pub fn encrypt_with_public_key(email: &str, public_key: &str) -> Result<String, Error> {
    let ephemeral_secret = EphemeralSecret::random();
    let ephemeral_public_key = PublicKey::from(&ephemeral_secret);
    let backend_public_key = get_public_key(public_key);
    // Generate the shared secret
    let shared_secret = ephemeral_secret.diffie_hellman(&backend_public_key);
    // Generate random nonce every time something is encrypted
    let mut nonce = [0u8; NONCE_LENGTH];
    getrandom::getrandom(&mut nonce).map_err(Error::GetRandom)?;
    let nonce = XNonce::from_slice(&nonce);
    let shared_key = blake3::derive_key(KDF_CONTEXT, shared_secret.as_bytes());

    let mut encrypted = init_cipher(&shared_key)
        .encrypt(nonce, email.as_bytes())
        .map_err(|_| Error::EncryptionFailed)?;

    // Concatenate ephemeral public key, nonce, and payload, as all three will be needed for decryption.
    let mut payload = ephemeral_public_key.as_bytes().to_vec();
    payload.append(&mut nonce.to_vec());
    payload.append(&mut encrypted);

    Ok(format!("{}{}{}", PREFIX, hex::encode(payload), SUFFIX))
}

pub fn encrypt(email: &str) -> Result<String, Error> {
    encrypt_with_public_key(email, PUBLIC_KEY)
}

/// Try decrypting an email address encrypted by this module with the provided x25519 private key.
///
/// If the email address was not encrypted by this module it will returned as-is. Because of that
/// you can pass all the email addresses you have through this function.
pub fn try_decrypt(private_key: &str, email: &str) -> Result<String, Error> {
    let combined = match email
        .strip_prefix(PREFIX)
        .and_then(|e| e.strip_suffix(SUFFIX))
    {
        Some(encrypted) => hex::decode(encrypted).map_err(Error::Hex)?,
        None => return Ok(email.to_string()),
    };
    if combined.len() < KEY_LENGTH + NONCE_LENGTH {
        return Err(Error::WrongKeyLength);
    }

    let (public_key, rest) = combined.split_at(KEY_LENGTH);
    let public_key: &[u8; KEY_LENGTH] = public_key.try_into().unwrap(); // Safe unwrap as the length is verified above
    let (nonce, encrypted) = rest.split_at(NONCE_LENGTH);
    let nonce = XNonce::from_slice(nonce);

    let private_key = get_private_key(private_key)?;
    let shared_secret = private_key.diffie_hellman(&PublicKey::from(public_key.to_owned()));
    let shared_key = blake3::derive_key(KDF_CONTEXT, shared_secret.as_bytes());

    String::from_utf8(
        init_cipher(&shared_key)
            .decrypt(nonce, encrypted)
            .map_err(|_| Error::EncryptionFailed)?,
    )
    .map_err(|_| Error::InvalidUtf8)
}

fn init_cipher(key: &[u8; KEY_LENGTH]) -> XChaCha20Poly1305 {
    let key = Key::from_slice(key);
    XChaCha20Poly1305::new(key)
}

pub fn generate_x25519_keypair() -> (String, String) {
    let ephemeral_secret = StaticSecret::random();
    let ephemeral_public_key = PublicKey::from(&ephemeral_secret);
    (
        ephemeral_secret.encode_hex(),
        ephemeral_public_key.encode_hex(),
    )
}

#[derive(Debug)]
pub enum Error {
    GetRandom(getrandom::Error),
    Hex(hex::FromHexError),
    EncryptionFailed,
    DecryptionFailed,
    WrongKeyLength,
    InvalidUtf8,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::GetRandom(e) => write!(f, "{e}"),
            Error::Hex(e) => write!(f, "{e}"),
            Error::EncryptionFailed => write!(f, "encryption failed"),
            Error::DecryptionFailed => write!(f, "encryption failed"),
            Error::InvalidUtf8 => write!(f, "invalid UTF-8"),
            Error::WrongKeyLength => write!(f, "expected 32-bytes key"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt() -> Result<(), Error> {
        const PRIVATE_KEY: &str =
            "73cd73133b310671933f020b957594960bc046410765a1e145f144f88f379408";
        const PUBLIC_KEY: &str = "d1734021de0af5cfeca64482f3c38b3350a38fd4be2e6a88b2c150be4416b261";
        const ADDRESS: &str = "foo@example.com";

        let encrypted = encrypt_with_public_key(ADDRESS, PUBLIC_KEY)?;
        assert!(
            !encrypted.contains(ADDRESS),
            "the encrypted version did contain the plaintext!"
        );

        assert_eq!(ADDRESS, try_decrypt(PRIVATE_KEY, &encrypted)?);

        Ok(())
    }
}
