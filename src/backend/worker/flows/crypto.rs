//! Encryption of WhatsApp Flows data exchange, as the Flows runtime does it:
//! the request is sealed with a fresh AES-128-GCM key and 16-byte IV, the key
//! is wrapped with RSA-OAEP (SHA-256) for the business's endpoint, and the
//! reply comes back under the same key with every bit of the IV flipped.

use aes_gcm::aead::consts::U16;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::aes::Aes128;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use num_bigint::BigUint;
use sha2::{Digest, Sha256};

type Gcm = aes_gcm::AesGcm<Aes128, U16>;

/// A sealed request and the key needed to open its reply.
pub struct Sealed {
    pub data: String,
    pub key: String,
    pub iv: String,
    secret: [u8; 16],
    nonce: [u8; 16],
}

/// Seals `plaintext` for the endpoint whose key is `public_key`: base64 of a
/// PEM `PUBLIC KEY` (SPKI) block, as the metadata query returns it.
pub fn seal(public_key: &str, plaintext: &str) -> Result<Sealed, &'static str> {
    let (modulus, exponent) = rsa_key(public_key)?;
    let secret: [u8; 16] = rand::random();
    let nonce: [u8; 16] = rand::random();
    let data = Gcm::new(&secret.into())
        .encrypt(&nonce.into(), plaintext.as_bytes())
        .map_err(|_| "encryption failed")?;
    let key = oaep_encrypt(&modulus, &exponent, &secret, &rand::random())?;
    Ok(Sealed {
        data: B64.encode(data),
        key: B64.encode(key),
        iv: B64.encode(nonce),
        secret,
        nonce,
    })
}

impl Sealed {
    /// Opens the endpoint's base64 reply. Data API version "100" kept the IV;
    /// every later version flips it.
    pub fn open(&self, reply: &str, version: &str) -> Result<String, &'static str> {
        let mut nonce = self.nonce;
        if version != "100" {
            nonce.iter_mut().for_each(|byte| *byte = !*byte);
        }
        let reply = B64
            .decode(reply.trim())
            .map_err(|_| "the business sent a reply that is not base64")?;
        let plain = Gcm::new(&self.secret.into())
            .decrypt(&nonce.into(), reply.as_slice())
            .map_err(|_| "the business's reply could not be decrypted")?;
        String::from_utf8(plain).map_err(|_| "the business's reply is not text")
    }
}

/// The modulus and exponent of an RSA key given as base64 of its PEM block.
fn rsa_key(public_key: &str) -> Result<(BigUint, BigUint), &'static str> {
    let invalid = || "the business's public key is invalid";
    let pem = B64.decode(public_key.trim()).map_err(|_| invalid())?;
    let pem = String::from_utf8(pem).map_err(|_| invalid())?;
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<String>()
        .split_whitespace()
        .collect();
    let der = B64.decode(body).map_err(|_| invalid())?;
    spki_rsa(&der).ok_or_else(invalid)
}

/// Reads one DER element: its tag and contents, and the bytes after it.
fn der(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, mut rest) = rest.split_first()?;
    let len = if first < 0x80 {
        usize::from(first)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        let (bytes, tail) = rest.split_at(count);
        rest = tail;
        bytes
            .iter()
            .fold(0usize, |len, &byte| (len << 8) | usize::from(byte))
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// SubjectPublicKeyInfo { AlgorithmIdentifier, BIT STRING { RSAPublicKey } }.
fn spki_rsa(spki: &[u8]) -> Option<(BigUint, BigUint)> {
    const RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
    let (0x30, info, _) = der(spki)? else {
        return None;
    };
    let (0x30, algorithm, rest) = der(info)? else {
        return None;
    };
    let (0x06, oid, _) = der(algorithm)? else {
        return None;
    };
    if oid != RSA_ENCRYPTION {
        return None;
    }
    let (0x03, bits, _) = der(rest)? else {
        return None;
    };
    let (0, key) = bits.split_first()? else {
        return None;
    };
    let (0x30, key, _) = der(key)? else {
        return None;
    };
    let (0x02, modulus, rest) = der(key)? else {
        return None;
    };
    let (0x02, exponent, _) = der(rest)? else {
        return None;
    };
    Some((
        BigUint::from_bytes_be(modulus),
        BigUint::from_bytes_be(exponent),
    ))
}

/// MGF1 with SHA-256.
fn mgf1(seed: &[u8], len: usize) -> Vec<u8> {
    let mut mask = Vec::with_capacity(len + 32);
    let mut counter = 0u32;
    while mask.len() < len {
        mask.extend(
            Sha256::new()
                .chain_update(seed)
                .chain_update(counter.to_be_bytes())
                .finalize(),
        );
        counter += 1;
    }
    mask.truncate(len);
    mask
}

/// RSAES-OAEP encryption (RFC 8017 7.1.1) with SHA-256, MGF1-SHA-256 and an
/// empty label, as WebCrypto's `RSA-OAEP` with `SHA-256` does.
fn oaep_encrypt(
    modulus: &BigUint,
    exponent: &BigUint,
    message: &[u8],
    seed: &[u8; 32],
) -> Result<Vec<u8>, &'static str> {
    const HASH: usize = 32;
    let k = modulus.bits().div_ceil(8) as usize;
    if k < 2 * HASH + 2 || message.len() > k - 2 * HASH - 2 {
        return Err("the business's public key is too small");
    }
    let mut db = Sha256::digest([]).to_vec();
    db.resize(k - HASH - 1 - message.len() - 1, 0);
    db.push(1);
    db.extend_from_slice(message);
    for (byte, mask) in db.iter_mut().zip(mgf1(seed, k - HASH - 1)) {
        *byte ^= mask;
    }
    let mut masked_seed = *seed;
    for (byte, mask) in masked_seed.iter_mut().zip(mgf1(&db, HASH)) {
        *byte ^= mask;
    }
    let mut encoded = vec![0u8];
    encoded.extend_from_slice(&masked_seed);
    encoded.extend_from_slice(&db);
    let cipher = BigUint::from_bytes_be(&encoded).modpow(exponent, modulus);
    let bytes = cipher.to_bytes_be();
    let mut out = vec![0u8; k - bytes.len()];
    out.extend_from_slice(&bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway 2048-bit test key made with `openssl genrsa`; never used
    // for anything else.
    const PUBLIC_PEM: &str = include_str!("test_key.pub.pem");
    const PRIVATE_EXPONENT: &str = include_str!("test_key.d.hex");

    fn public_key() -> String {
        B64.encode(PUBLIC_PEM)
    }

    /// RFC 8017 7.1.2 decryption, only to check the encryption above.
    fn oaep_decrypt(modulus: &BigUint, cipher: &[u8]) -> Vec<u8> {
        let d = BigUint::parse_bytes(PRIVATE_EXPONENT.trim().as_bytes(), 16).unwrap();
        let k = modulus.bits().div_ceil(8) as usize;
        let bytes = BigUint::from_bytes_be(cipher)
            .modpow(&d, modulus)
            .to_bytes_be();
        let mut encoded = vec![0u8; k - bytes.len()];
        encoded.extend_from_slice(&bytes);
        assert_eq!(encoded[0], 0);
        let (masked_seed, masked_db) = encoded[1..].split_at(32);
        let seed: Vec<u8> = masked_seed
            .iter()
            .zip(mgf1(masked_db, 32))
            .map(|(a, b)| a ^ b)
            .collect();
        let db: Vec<u8> = masked_db
            .iter()
            .zip(mgf1(&seed, k - 33))
            .map(|(a, b)| a ^ b)
            .collect();
        assert_eq!(db[..32], Sha256::digest([])[..]);
        let one = 32 + db[32..].iter().position(|&b| b != 0).unwrap();
        assert_eq!(db[one], 1);
        db[one + 1..].to_vec()
    }

    #[test]
    fn the_key_is_wrapped_so_the_endpoint_can_unwrap_it() {
        let (modulus, exponent) = rsa_key(&public_key()).unwrap();
        assert_eq!(modulus.bits(), 2048);
        assert_eq!(exponent, BigUint::from(65537u32));
        let secret = [7u8; 16];
        let cipher = oaep_encrypt(&modulus, &exponent, &secret, &[9; 32]).unwrap();
        assert_eq!(cipher.len(), 256);
        assert_eq!(oaep_decrypt(&modulus, &cipher), secret);
    }

    #[test]
    fn a_reply_sealed_with_the_flipped_iv_opens() {
        let sealed = seal(&public_key(), r#"{"action":"INIT"}"#).unwrap();
        let (modulus, _) = rsa_key(&public_key()).unwrap();
        let key = oaep_decrypt(&modulus, &B64.decode(&sealed.key).unwrap());
        assert_eq!(key, sealed.secret);
        let cipher = Gcm::new(sealed.secret.as_slice().try_into().unwrap());
        let request = cipher
            .decrypt(
                &sealed.nonce.into(),
                B64.decode(&sealed.data).unwrap().as_slice(),
            )
            .unwrap();
        assert_eq!(request, br#"{"action":"INIT"}"#);
        let flipped = sealed.nonce.map(|byte| !byte);
        let reply = cipher
            .encrypt(&flipped.into(), br#"{"screen":"PIN"}"#.as_slice())
            .unwrap();
        assert_eq!(
            sealed.open(&B64.encode(&reply), "3.0").unwrap(),
            r#"{"screen":"PIN"}"#
        );
        assert!(sealed.open(&B64.encode(&reply), "100").is_err());
        assert!(sealed.open("not base64!", "3.0").is_err());
    }

    #[test]
    fn keys_that_are_not_rsa_spki_are_refused() {
        assert!(seal("", "{}").is_err());
        assert!(
            seal(
                &B64.encode("-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----"),
                "{}"
            )
            .is_err()
        );
    }
}
