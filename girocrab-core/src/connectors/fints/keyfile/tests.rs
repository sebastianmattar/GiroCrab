use super::*;
use aes::cipher::BlockModeEncrypt;

fn field(tag: u16, data: &[u8]) -> Vec<u8> {
    [
        tag.to_le_bytes().as_slice(),
        (data.len() as u16).to_le_bytes().as_slice(),
        data,
    ]
    .concat()
}
fn number(n: &BoxedUint) -> Vec<u8> {
    let mut bytes = n.to_be_bytes_trimmed_vartime().to_vec();
    bytes.reverse();
    [(bytes.len() as u16).to_le_bytes().as_slice(), &bytes].concat()
}
fn fixture() -> Vec<u8> {
    fixture_with_bank_keys(true)
}

fn fixture_with_bank_keys(include_bank_keys: bool) -> Vec<u8> {
    let password = b"invented-test-password";
    let salt = [7u8; 20];
    let iterations = 1000u32;
    let header = [
        10u16.to_le_bytes().as_slice(),
        1u16.to_le_bytes().as_slice(),
        &salt,
        &iterations.to_le_bytes(),
    ]
    .concat();
    let mut derived = [0u8; 32];
    pbkdf2_hmac::<Sha256>(password, &salt, iterations, &mut derived);
    let rsa = PrivateKey::new(&mut getrandom::SysRng, 2048).unwrap();
    let mut private = Vec::new();
    for n in [
        rsa.n().as_ref(),
        rsa.primes()[0].as_ref(),
        rsa.primes()[1].as_ref(),
        rsa.dp().unwrap(),
        rsa.dq().unwrap(),
        rsa.dp().unwrap(),
        rsa.dq().unwrap(),
    ] {
        private.extend(number(n));
    }
    let mut encrypted = cbc::Encryptor::<des::TdesEde3>::new_from_slices(&derived[..24], &[0; 8])
        .unwrap()
        .encrypt_padded_vec::<Pkcs7>(&private);
    encrypted.reverse();
    let mut bank = vec![b' '; 237];
    bank[..3].copy_from_slice(b"280");
    bank[3..11].copy_from_slice(b"12345678");
    bank[93..97].copy_from_slice(b"demo");
    bank[153] = b'1';
    bank[183] = 2;
    let host = b"hbci.commerzbank.de";
    bank[184..184 + host.len()].copy_from_slice(host);
    bank[234..237].copy_from_slice(&[1, 0, 4]);
    let mut account = field(0x444b, &bank);
    for kind in [0u8, 1] {
        let key = [
            vec![2, kind, 1, 0, 1, 0],
            number(rsa.e()),
            number(rsa.n().as_ref()),
            (encrypted.len() as u16).to_le_bytes().to_vec(),
            encrypted.clone(),
        ]
        .concat();
        account.extend(field(0x4553, &key));
    }
    let mut bytes = field(0x4e56, &header);
    bytes.extend(field(0x564b, &account));
    for kind in if include_bank_keys {
        vec![0u8, 1]
    } else {
        vec![]
    } {
        let mut bank_key = vec![b' '; 69];
        bank_key[..3].copy_from_slice(b"280");
        bank_key[3..11].copy_from_slice(b"12345678");
        bank_key[33..39].copy_from_slice(&[2, kind, 1, 0, 1, 0]);
        bank_key[39..43].copy_from_slice(b"bank");
        bank_key.extend(number(rsa.n().as_ref()));
        bank_key.extend(number(rsa.e()));
        bytes.extend(field(0x53d6, &bank_key));
    }
    bytes.extend(field(0x4452, b"20261113000000"));
    let mut mac = Hmac::<Sha256>::new_from_slice(&derived).unwrap();
    mac.update(&bytes);
    bytes.extend(field(0x4d44, &mac.finalize().into_bytes()));
    bytes
}

#[test]
fn authenticates_decrypts_and_checks_rsa() {
    let bytes = fixture();
    let parsed = KeyFile::open(&bytes, b"invented-test-password").unwrap();
    assert_eq!(parsed.identities.len(), 1);
    assert_eq!(parsed.identities[0].blz, "12345678");
    parsed.identities[0].signing.key.validate().unwrap();
    assert!(matches!(
        KeyFile::open(&bytes, b"wrong"),
        Err(CoreError::Authentication { .. })
    ));
    let mut changed = bytes.clone();
    changed[45] ^= 1;
    assert!(matches!(
        KeyFile::open(&changed, b"invented-test-password"),
        Err(CoreError::Authentication { .. })
    ));
    // Framing is checked without panicking on every truncated prefix.
    for len in 0..bytes.len() {
        assert!(KeyFile::open(&bytes[..len], b"invented-test-password").is_err());
    }
}

#[test]
fn rejects_unbounded_work_before_password_derivation() {
    let mut bytes = field(
        0x4e56,
        &[
            10, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255,
            255,
        ],
    );
    bytes.extend(field(0x4d44, &[0; 32]));
    assert!(KeyFile::open(&bytes, b"password").is_err());
    assert!(KeyFile::open(&vec![0; MAX_FILE_SIZE + 1], b"password").is_err());
}

#[test]
fn reads_independent_rdhx_and_rejects_authenticated_invalid_crt() {
    let v: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/rah10/reference.json"
    ))
    .unwrap();
    let bytes = hex::decode(v["keyfile"].as_str().unwrap()).unwrap();
    let password = b"invented-reference-password";
    let file = KeyFile::open(&bytes, password).unwrap();
    assert_eq!(file.identities[0].user_id, "demo");
    assert_eq!(file.identities[0].blz, "12345678");
    assert!(file.bank_keys.is_empty());
    file.identities[0].signing.key.validate().unwrap();
    let composite = hex::decode(v["composite_factor_keyfile"].as_str().unwrap()).unwrap();
    assert!(matches!(
        KeyFile::open(&composite, password),
        Err(CoreError::InvalidInput { .. })
    ));
    let invalid_crt = hex::decode(v["invalid_crt_keyfile"].as_str().unwrap()).unwrap();
    assert!(matches!(
        KeyFile::open(&invalid_crt, password),
        Err(CoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        KeyFile::open(&bytes, b"wrong"),
        Err(CoreError::Authentication { .. })
    ));
}

#[test]
fn vault_import_is_atomic_and_reserves_counters_before_sync() {
    use crate::{Vault, test_support::TempDir};
    let directory = TempDir::new("rah-import");
    let vault = Vault::open(
        directory.join("vault.girocrab").to_string_lossy().into(),
        vec![7; 32],
    )
    .unwrap();
    let bytes = fixture();
    assert!(
        vault
            .add_rah10_connection(bytes.clone(), "wrong".into(), "Demo".into())
            .is_err()
    );
    assert!(vault.list_bank_connections().unwrap().is_empty());
    let id = vault
        .add_rah10_connection(
            bytes.clone(),
            "invented-test-password".into(),
            "Demo".into(),
        )
        .unwrap();
    assert!(vault.is_rah10_connection(id).unwrap());
    assert!(
        vault
            .add_rah10_connection(bytes, "invented-test-password".into(), "Demo".into())
            .is_err()
    );
    let counter = format!("fints.rah10.counter.{id}");
    assert_eq!(
        vault.get_setting(counter.clone()).unwrap().as_deref(),
        Some("2")
    );
    let first = vault
        .clone()
        .begin_sync(id, "invented-test-password".into(), true)
        .unwrap();
    drop(first); // no fetch, no network, no finish
    assert_eq!(
        vault.get_setting(counter.clone()).unwrap().as_deref(),
        Some("10002")
    );
    drop(
        vault
            .clone()
            .begin_sync(id, "invented-test-password".into(), true)
            .unwrap(),
    );
    assert_eq!(
        vault.get_setting(counter).unwrap().as_deref(),
        Some("20002")
    );
    vault.delete_bank_connection(id).unwrap();
    assert!(!vault.is_rah10_connection(id).unwrap());
}

#[tokio::test]
async fn separately_confirmed_bank_keys_are_persisted_and_revalidated() {
    use crate::{
        Vault,
        connectors::{
            BankConnector,
            fints::{FinTsConnector, bank_keys},
        },
        test_support::TempDir,
    };
    let directory = TempDir::new("rah-pins");
    let vault = Vault::open(
        directory.join("vault.girocrab").to_string_lossy().into(),
        vec![7; 32],
    )
    .unwrap();
    let bytes = fixture_with_bank_keys(false);
    let secret = "invented-test-password";
    let reference = KeyFile::open(&fixture(), secret.as_bytes()).unwrap();
    let candidates: Vec<_> = reference
        .bank_keys
        .iter()
        .map(|key| bank_keys::key_candidate(key).unwrap())
        .collect();
    let expected: Vec<_> = candidates.iter().map(|c| c.fingerprint.clone()).collect();
    assert!(
        vault
            .add_rah10_connection_with_bank_keys(
                bytes.clone(),
                secret.into(),
                "Demo".into(),
                candidates.clone(),
                vec!["00".repeat(32); 2]
            )
            .is_err()
    );
    assert!(vault.list_bank_connections().unwrap().is_empty());
    let id = vault
        .add_rah10_connection_with_bank_keys(
            bytes,
            secret.into(),
            "Demo".into(),
            candidates,
            expected,
        )
        .unwrap();
    let pin_key = format!("fints.rah10.bank_keys.{id}");
    let saved = vault.get_setting(pin_key.clone()).unwrap().unwrap();
    let mut pins: bank_keys::SavedBankKeys = serde_json::from_str(&saved).unwrap();
    assert_eq!(
        bank_keys::confirmed_keys("12345678", &pins.candidates, &pins.expected)
            .unwrap()
            .len(),
        2
    );
    pins.expected[0] = "00".repeat(32);
    vault
        .set_setting(pin_key.clone(), serde_json::to_string(&pins).unwrap())
        .unwrap();
    let session = vault.clone().begin_sync(id, secret.into(), true).unwrap();
    let ctx = session.clone_context().unwrap();
    let mut connector = FinTsConnector::for_key("fints:12345678:rah10");
    let error = connector.begin(&ctx).await.err().unwrap();
    assert!(error.to_string().contains("Prüfwert")); // stops before any bank contact
    vault.delete_bank_connection(id).unwrap();
    assert!(vault.get_setting(pin_key).unwrap().is_none());
}
