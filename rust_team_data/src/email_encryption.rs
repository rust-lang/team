//! This module implements the encryption scheme used to safely include private email addresses in
//! the team repository. It generates encrypted content that looks like this:
//!
//! ```text
//! encrypted+3eeedb8887004d9a8266e9df1b82a2d52dcce82c4fa1d277c5f14e261e8155acc8a66344edc972fa58b678dc2bcad2e8f7c201a1eede9c16639fe07df8bac5aa1097b2ad9699a700edb32ef192eaa74bf7af0a@rust-lang.invalid
//! ```
//!
//! The hex-encoded part of the email address is a concatenation of a 24-byte random nonce and the
//! XChaCha20Poly1305-encrypted email address. Utilities are provided to both encrypt and decrypt.

use chacha20poly1305::aead::{Aead, NewAead};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

const PREFIX: &str = "encrypted+";
const SUFFIX: &str = "@rust-lang.invalid";
const KEY_LENGTH: usize = 32;
const NONCE_LENGTH: usize = 24;

/// Encrypt an email address with the provided key.
pub fn encrypt(key: &str, email: &str) -> Result<String, Error> {
    // Generate a random nonce every time something is encrypted.
    let mut nonce = [0u8; NONCE_LENGTH];
    getrandom::getrandom(&mut nonce).map_err(Error::GetRandom)?;
    let nonce = XNonce::from_slice(&nonce);

    let mut encrypted = init_cipher(key)?
        .encrypt(nonce, email.as_bytes())
        .map_err(|_| Error::EncryptionFailed)?;

    // Concatenate both the nonce and the payload, as both will be needed for decryption.
    let mut payload = nonce.to_vec();
    payload.append(&mut encrypted);

    Ok(format!("{}{}{}", PREFIX, hex::encode(payload), SUFFIX))
}

/// Try decrypting an email address encrypted by this module with the provided key.
///
/// If the email address was not encrypted by this module it will returned as-is. Because of that
/// you can pass all the email addresses you have through this function.
pub fn try_decrypt(key: &str, email: &str) -> Result<String, Error> {
    let combined = match email
        .strip_prefix(PREFIX)
        .and_then(|e| e.strip_suffix(SUFFIX))
    {
        Some(encrypted) => hex::decode(encrypted).map_err(Error::Hex)?,
        None => return Ok(email.to_string()),
    };

    let (nonce, encrypted) = combined.split_at(NONCE_LENGTH);
    let nonce = XNonce::from_slice(nonce);

    String::from_utf8(
        init_cipher(key)?
            .decrypt(nonce, encrypted)
            .map_err(|_| Error::EncryptionFailed)?,
    )
    .map_err(|_| Error::InvalidUtf8)
}

fn init_cipher(key: &str) -> Result<XChaCha20Poly1305, Error> {
    if key.len() != KEY_LENGTH {
        return Err(Error::WrongKeyLength);
    }
    let key = Key::from_slice(key.as_bytes());
    Ok(XChaCha20Poly1305::new(key))
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
            Error::GetRandom(e) => write!(f, "{}", e),
            Error::Hex(e) => write!(f, "{}", e),
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
        const KEY: &str = "rxrtZ4uQ7uYJnikmUVxdcxrBmazEiH0k";
        const ADDRESS: &str = "foo@example.com";

        let encrypted = encrypt(KEY, ADDRESS)?;
        assert!(
            !encrypted.contains(ADDRESS),
            "the encrypted version did contain the plaintext!"
        );

        assert_eq!(ADDRESS, try_decrypt(KEY, &encrypted)?);

        Ok(())
    }
}
