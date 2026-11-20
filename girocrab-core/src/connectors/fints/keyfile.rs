//! Bounded, authenticated RDHX profile-10 keyfile reader.
//! Layout references: HBCI4Java rdhXfile/{TLV,FileHeader,HBCIAccount,BankKeys,RDHXFile}.
//! https://github.com/hbci4j/hbci4java/tree/master/src/main/java/org/kapott/hbci/passport/rdhXfile
//! Never changes the source file, prints data, or contacts a bank.

use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use hmac::{Hmac, KeyInit, Mac};
use pbkdf2::pbkdf2_hmac;
use rsa::{
    BoxedUint, PrivateKey, PublicKey,
    traits::{PrivateKeyParts, PublicKeyParts},
};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;

pub const MAX_FILE_SIZE: usize = 4 * 1024 * 1024;
const MAX_ITERATIONS: usize = 2_000_000;

fn invalid() -> CoreError {
    CoreError::invalid(tr(
        "Schlüsseldatei beschädigt oder Format noch nicht unterstützt.",
    ))
}
fn crypto(_: impl std::fmt::Debug) -> CoreError {
    invalid()
}

struct Field<'a> {
    tag: u16,
    data: &'a [u8],
    raw: &'a [u8],
}

fn fields(mut bytes: &[u8]) -> CoreResult<Vec<Field<'_>>> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        let length = usize::from(u16_at(bytes, 2)?);
        let raw = bytes.get(..4 + length).ok_or_else(invalid)?;
        result.push(Field {
            tag: u16_at(bytes, 0)?,
            data: &raw[4..],
            raw,
        });
        bytes = &bytes[raw.len()..];
    }
    Ok(result)
}

fn u16_at(bytes: &[u8], offset: usize) -> CoreResult<u16> {
    let b = bytes.get(offset..offset + 2).ok_or_else(invalid)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn text(bytes: &[u8], start: usize, length: usize) -> CoreResult<String> {
    let b = bytes.get(start..start + length).ok_or_else(invalid)?;
    Ok(b.iter()
        .map(|&b| char::from(b))
        .collect::<String>()
        .trim_matches([' ', '\0'])
        .into())
}

fn integer(bytes: &[u8], offset: &mut usize) -> CoreResult<BoxedUint> {
    let length = usize::from(u16_at(bytes, *offset)?);
    *offset += 2;
    if length == 0 || length > 1024 {
        return Err(invalid());
    }
    let mut value = Zeroizing::new(
        bytes
            .get(*offset..*offset + length)
            .ok_or_else(invalid)?
            .to_vec(),
    );
    *offset += length;
    value.reverse();
    BoxedUint::from_be_slice(&value, (length * 8) as u32).map_err(crypto)
}

// Deliberately no Debug/Serialize: these types contain credentials and private keys.
pub struct UserKey {
    pub number: u16,
    pub version: u16,
    pub key: PrivateKey,
}
pub struct BankKey {
    pub blz: String,
    pub user_id: String,
    pub kind: u8,
    pub number: u16,
    pub version: u16,
    pub key: PublicKey,
}
pub struct Identity {
    pub blz: String,
    pub user_id: String,
    pub customer_id: String,
    pub system_id: String,
    pub host: String,
    pub signature_id: u16,
    pub signing: UserKey,
    pub encryption: UserKey,
}
pub struct KeyFile {
    pub identities: Vec<Identity>,
    pub bank_keys: Vec<BankKey>,
}

fn user_key(data: &[u8], des_key: &[u8]) -> CoreResult<(u8, UserKey)> {
    let kind = *data.get(1).ok_or_else(invalid)?;
    if kind > 1 {
        return Err(invalid());
    }
    let mut offset = 6;
    let e = integer(data, &mut offset)?;
    let n = integer(data, &mut offset)?;
    let length = usize::from(u16_at(data, offset)?);
    offset += 2;
    let mut encrypted = Zeroizing::new(
        data.get(offset..offset + length)
            .ok_or_else(invalid)?
            .to_vec(),
    );
    if offset + length != data.len() {
        return Err(invalid());
    }
    encrypted.reverse();
    // Decrypt in a zeroizing buffer so an authentication/padding error also wipes it.
    let mut plain = encrypted;
    let used = cbc::Decryptor::<des::TdesEde3>::new_from_slices(des_key, &[0; 8])
        .map_err(crypto)?
        .decrypt_padded::<Pkcs7>(&mut plain)
        .map_err(crypto)?
        .len();
    plain.truncate(used);
    let mut offset = 0;
    let private_n = integer(&plain, &mut offset)?;
    let p = Zeroizing::new(integer(&plain, &mut offset)?);
    let q = Zeroizing::new(integer(&plain, &mut offset)?);
    let dp = Zeroizing::new(integer(&plain, &mut offset)?);
    let dq = Zeroizing::new(integer(&plain, &mut offset)?);
    // Stored CRT recombination factors; RustCrypto recomputes its own coefficients.
    let _ap = Zeroizing::new(integer(&plain, &mut offset)?);
    let _aq = Zeroizing::new(integer(&plain, &mut offset)?);
    if offset != plain.len() || private_n != n || n.bit_width() < 1984 || n.bit_width() > 8192 {
        return Err(invalid());
    }
    // PrivateKey::validate checks RSA congruences but not primality. Preserve
    // the old import's prime-factor check instead of accepting composite factors.
    if !crypto_primes::is_prime(crypto_primes::Flavor::Any, &*p)
        || !crypto_primes::is_prime(crypto_primes::Flavor::Any, &*q)
    {
        return Err(invalid());
    }
    let rsa = PrivateKey::from_primes(vec![(*p).clone(), (*q).clone()], e).map_err(crypto)?;
    rsa.validate().map_err(crypto)?;
    if rsa.n().as_ref() != &n || rsa.dp() != Some(&*dp) || rsa.dq() != Some(&*dq) {
        return Err(invalid());
    }
    Ok((
        kind,
        UserKey {
            number: u16_at(data, 2)?,
            version: u16_at(data, 4)?,
            key: rsa,
        },
    ))
}

impl KeyFile {
    /// Password bytes are UTF-8; HBCI4Java uses the same convention on modern systems.
    /// Authenticate the entire container before parsing or decrypting key payloads.
    pub fn open(bytes: &[u8], password: &[u8]) -> CoreResult<Self> {
        if bytes.len() > MAX_FILE_SIZE || password.is_empty() {
            return Err(invalid());
        }
        let records = fields(bytes)?;
        let header = records.first().ok_or_else(invalid)?;
        if header.tag != 0x4e56
            || header.data.len() != 28
            || u16_at(header.data, 0)? != 10
            || u16_at(header.data, 2)? != 1
        {
            return Err(invalid());
        }
        let salt = &header.data[4..24];
        let iterations =
            u32::from_le_bytes(header.data[24..28].try_into().map_err(|_| invalid())?) as usize;
        if iterations == 0 || iterations > MAX_ITERATIONS {
            return Err(invalid());
        }
        let macs: Vec<_> = records.iter().filter(|r| r.tag == 0x4d44).collect();
        if macs.len() != 1 || macs[0].data.len() != 32 {
            return Err(invalid());
        }
        let mut derived = Zeroizing::new([0u8; 32]);
        pbkdf2_hmac::<Sha256>(password, salt, iterations as u32, derived.as_mut());
        let mut mac = Hmac::<Sha256>::new_from_slice(derived.as_ref()).map_err(crypto)?;
        for record in records.iter().filter(|r| r.tag != 0x4d44) {
            mac.update(record.raw);
        }
        // Mac::verify_slice performs a constant-time tag comparison.
        if mac.verify_slice(macs[0].data).is_err() {
            return Err(CoreError::Authentication {
                message: tr("Passwort falsch oder Schlüsseldatei verändert.").into(),
            });
        }
        // PBKDF2 output prefix is identical for 24 and 32 byte requested keys.
        let des_key = &derived[..24];
        let mut identities = Vec::new();
        let mut bank_keys = Vec::new();
        let mut header_count = 0;
        for record in records {
            match record.tag {
                0x4e56 => {
                    header_count += 1;
                    if header_count != 1 {
                        return Err(invalid());
                    }
                }
                0x4d44 => {}
                0x4452 => {
                    if record.data.len() != 14 || !record.data.iter().all(u8::is_ascii_digit) {
                        return Err(invalid());
                    }
                }
                0x564b => {
                    let nested = fields(record.data)?;
                    let bank = nested.first().ok_or_else(invalid)?;
                    if bank.tag != 0x444b || bank.data.len() != 237 {
                        return Err(invalid());
                    }
                    if text(bank.data, 0, 3)? != "280" {
                        return Err(invalid());
                    }
                    let mut signing = None;
                    let mut encryption = None;
                    for key in nested.iter().skip(1) {
                        if key.tag != 0x4553 {
                            return Err(invalid());
                        }
                        let (kind, key) = user_key(key.data, des_key)?;
                        let slot = if kind == 0 {
                            &mut signing
                        } else {
                            &mut encryption
                        };
                        if slot.replace(key).is_some() {
                            return Err(invalid());
                        }
                    }
                    let blz = text(bank.data, 3, 30)?;
                    if blz.len() != 8 || !blz.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(invalid());
                    }
                    identities.push(Identity {
                        blz,
                        user_id: text(bank.data, 93, 30)?,
                        customer_id: text(bank.data, 123, 30)?,
                        system_id: text(bank.data, 153, 30)?,
                        host: text(bank.data, 184, 50)?,
                        signature_id: u16_at(bank.data, 234)?,
                        signing: signing.ok_or_else(invalid)?,
                        encryption: encryption.ok_or_else(invalid)?,
                    });
                }
                0x53d6 => {
                    let data = record.data;
                    if text(data, 0, 3)? != "280" {
                        return Err(invalid());
                    }
                    let kind = *data.get(34).ok_or_else(invalid)?;
                    if kind > 1 {
                        return Err(invalid());
                    }
                    let mut offset = 69;
                    let n = integer(data, &mut offset)?;
                    let e = integer(data, &mut offset)?;
                    if offset != data.len() || n.bit_width() < 1984 || n.bit_width() > 8192 {
                        return Err(invalid());
                    }
                    bank_keys.push(BankKey {
                        blz: text(data, 3, 30)?,
                        user_id: text(data, 39, 30)?,
                        kind,
                        number: u16_at(data, 37)?,
                        version: u16_at(data, 35)?,
                        key: PublicKey::new_with_size_limits(n, e, 1984..=8192).map_err(crypto)?,
                    });
                }
                _ => return Err(invalid()),
            }
        }
        if identities.is_empty() {
            return Err(invalid());
        }
        Ok(Self {
            identities,
            bank_keys,
        })
    }
}

#[cfg(test)]
mod tests;
