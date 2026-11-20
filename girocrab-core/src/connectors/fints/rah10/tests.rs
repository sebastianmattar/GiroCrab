use super::super::keyfile::UserKey;
use super::*;

fn bank_key(key: &PrivateKey, user: &str, kind: u8) -> BankKey {
    BankKey {
        blz: "12345678".into(),
        user_id: user.into(),
        kind,
        number: 1,
        version: 1,
        key: key.to_public_key(),
    }
}

fn peer(own: PrivateKey, remote: &PrivateKey, user: &str, other: &str) -> Rah10 {
    Rah10::new(
        KeyFile {
            identities: vec![Identity {
                blz: "12345678".into(),
                user_id: user.into(),
                customer_id: user.into(),
                system_id: "1".into(),
                host: "example.invalid".into(),
                signature_id: 1,
                signing: UserKey {
                    number: 1,
                    version: 1,
                    key: own.clone(),
                },
                encryption: UserKey {
                    number: 1,
                    version: 1,
                    key: own,
                },
            }],
            bank_keys: vec![bank_key(remote, other, 0), bank_key(remote, other, 1)],
        },
        user,
        2,
        100,
    )
    .unwrap()
}

#[test]
fn encrypted_signed_exchange_rejects_tampering_and_wrong_key() {
    let client_key = PrivateKey::new(&mut getrandom::SysRng, 2048).unwrap();
    let bank_key = PrivateKey::new(&mut getrandom::SysRng, 2048).unwrap();
    let client = peer(client_key.clone(), &bank_key, "client", "bank");
    let mut bank = peer(bank_key, &client_key, "bank", "client");
    let payload = vec![SegmentBuilder::new("HIRMG", 3, 2).raw("0010::OK").build()];
    let wire = bank.build(&payload, "1", "dialog", 1).unwrap();
    let result = client.unwrap(&wire, 1, "dialog").unwrap();
    assert_eq!(result[1].id(), "HIRMG");
    assert!(client.unwrap(&wire, 2, "dialog").is_err());
    assert!(client.unwrap(&wire, 1, "different-dialog").is_err());
    for len in 0..wire.len() {
        assert!(client.unwrap(&wire[..len], 1, "dialog").is_err());
    }
    let mut corrupted = wire.clone();
    let at = wire.windows(6).position(|b| b == b"HNVSD:").unwrap() + 25;
    corrupted[at] ^= 1;
    assert!(client.unwrap(&corrupted, 1, "dialog").is_err());
    let wrong = PrivateKey::new(&mut getrandom::SysRng, 2048).unwrap();
    let wrong_client = peer(client_key, &wrong, "client", "bank");
    assert!(wrong_client.unwrap(&wire, 1, "dialog").is_err());
    bank.next_signature = bank.signature_limit;
    assert!(bank.build(&payload, "1", "dialog", 2).is_err());
}

#[test]
fn signature_covers_exact_bytes_and_double_hash() {
    let private = PrivateKey::new(&mut getrandom::SysRng, 2048).unwrap();
    let public = private.to_public_key();
    let data = b"HNSHK:2:4+RAH:10'HIRMG:3:2+0010::OK'";
    let signature = sign(&private, data).unwrap();
    verify(&public, data, &signature).unwrap();
    assert!(verify(&public, b"changed", &signature).is_err());
    assert!(public.pss_verify::<Sha256>(data, &signature).is_err());
}

#[test]
fn segment_ranges_skip_binary_and_escapes() {
    let data = b"TEST:1:1+@3@a'b'TEST:2:1+foo?'bar'";
    let ranges = segment_ranges(data).unwrap();
    assert_eq!(ranges.len(), 2);
    assert_eq!(&data[ranges[0].0..ranges[0].1], b"TEST:1:1+@3@a'b'");
    for bad in [b"TEST+@99999@x'".as_slice(), b"TEST+?", b"TEST"] {
        assert!(segment_ranges(bad).is_err());
    }
}

#[test]
fn missing_bank_keys_require_setup() {
    let own = PrivateKey::new(&mut getrandom::SysRng, 2048).unwrap();
    let remote = PrivateKey::new(&mut getrandom::SysRng, 2048).unwrap();
    let client = peer(own, &remote, "client", "bank");
    let file = KeyFile {
        identities: vec![client.identity],
        bank_keys: vec![],
    };
    let error = Rah10::new(file, "client", 2, 100).err().unwrap();
    assert!(matches!(error, CoreError::InvalidInput { .. }));
    assert!(error.to_string().contains("Bankschlüssel"));
}

#[test]
fn independent_reference_vectors_match_transport_signature_and_cbc() {
    use rsa::pkcs8::DecodePublicKey;
    let v: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/rah10/reference.json"
    ))
    .unwrap();
    let bytes = |name: &str| hex::decode(v[name].as_str().unwrap()).unwrap();
    let public = PublicKey::from_public_key_der(&bytes("public_der")).unwrap();
    let number = |name: &str| {
        let text = v[name].as_str().unwrap();
        let text = if text.len().is_multiple_of(2) {
            text.to_string()
        } else {
            format!("0{text}")
        };
        BoxedUint::from_be_slice_vartime(&hex::decode(text).unwrap())
    };
    let private = PrivateKey::from_primes(vec![number("p"), number("q")], number("e")).unwrap();
    assert_eq!(private.to_public_key(), public);
    let client = peer(private.clone(), &private, "client", "bank");
    let response = client.unwrap(&bytes("wire"), 1, "dialog").unwrap();
    assert_eq!(response[1].id(), "HIRMG");
    assert_eq!(response[1].get(1, 0), "0010");
    let message = bytes("message");
    let signature = bytes("signature");
    verify(&public, &message, &signature).unwrap();
    assert!(verify(&public, b"changed", &signature).is_err());
    assert!(public.pss_verify::<Sha256>(&message, &signature).is_err());
    let session: [u8; 32] = bytes("session_key").try_into().unwrap();
    let wrapped = bytes("wrapped_key");
    assert_eq!(wrap_key(&public, &session).unwrap(), wrapped);
    assert_eq!(*unwrap_key(&private, &wrapped).unwrap(), session);
    assert!(unwrap_key(&private, &wrapped[1..]).is_err());
    assert!(unwrap_key(&private, &public.n_bytes()).is_err());
    let padded = bytes("padded_message");
    let encrypted = bytes("aes_ciphertext");
    assert_eq!(*aes(&padded, &session, Mode::Encrypt).unwrap(), encrypted);
    assert_eq!(*aes(&encrypted, &session, Mode::Decrypt).unwrap(), padded);
}

// Decrypts a response with the client key and returns the signed inner plaintext.
fn open_inner(client_key: &PrivateKey, wire: &[u8]) -> Vec<u8> {
    let outer = syntax::parse(wire).unwrap();
    let Some(Value::Binary(enc_key)) = outer[1].element(6).get(3) else {
        panic!("no session key");
    };
    let session = unwrap_key(client_key, enc_key).unwrap();
    let mut plain = aes(outer[2].binary(1).unwrap(), session.as_ref(), Mode::Decrypt)
        .unwrap()
        .to_vec();
    while plain.last() == Some(&0) {
        plain.pop();
    }
    assert_eq!(plain.pop(), Some(0x80));
    plain
}

// Encrypts an inner plaintext again with only the client's public key, as anyone can.
fn reseal(client: &PublicKey, inner: &[u8], dialog: &str, message: u32) -> Vec<u8> {
    let mut padded = inner.to_vec();
    padded.push(0x80);
    while !padded.len().is_multiple_of(16) {
        padded.push(0);
    }
    let session = [7u8; 32];
    let encrypted_key = wrap_key(client, &session).unwrap();
    let mut head = SegmentBuilder::new("HNVSK", 998, 3)
        .raw("RAH:10")
        .raw("4")
        .raw("1")
        .raw("1::1")
        .raw("1:20260101:000000")
        .build();
    head.pop();
    head.extend_from_slice(format!("+2:2:14:@{}@", encrypted_key.len()).as_bytes());
    head.extend_from_slice(&encrypted_key);
    head.extend_from_slice(b":6:1+280:12345678:client:V:1:1+0'");
    let data = SegmentBuilder::new("HNVSD", 999, 1)
        .binary(&aes(&padded, &session, Mode::Encrypt).unwrap())
        .build();
    let end = SegmentBuilder::new("HNHBS", 9, 1)
        .raw(&message.to_string())
        .build();
    let assemble = |size: usize| {
        let mut out = SegmentBuilder::new("HNHBK", 1, 3)
            .raw(&format!("{size:012}"))
            .raw("300")
            .de(&[dialog])
            .raw(&message.to_string())
            .build();
        out.extend_from_slice(&head);
        out.extend_from_slice(&data);
        out.extend_from_slice(&end);
        out
    };
    assemble(assemble(0).len())
}

#[test]
fn segment_inserted_before_signature_is_rejected_even_with_mid_text_binary_marker() {
    let client_key = PrivateKey::new(&mut getrandom::SysRng, 2048).unwrap();
    let bank_key = PrivateKey::new(&mut getrandom::SysRng, 2048).unwrap();
    let client = peer(client_key.clone(), &bank_key, "client", "bank");
    let mut bank = peer(bank_key, &client_key, "bank", "client");
    let payload = vec![SegmentBuilder::new("HIRMG", 3, 2).raw("0010::OK").build()];
    let wire = bank.build(&payload, "1", "dialog", 1).unwrap();
    let plain = open_inner(&client_key, &wire);
    let public = client_key.to_public_key();
    let split = plain.windows(6).position(|w| w == b"HNSHA:").unwrap();
    let (signed, trailer) = plain.split_at(split);
    // A text value ending in '@<length of HNSHA>@' must not hide the HNSHA framing.
    let mut forged = signed.to_vec();
    forged.extend_from_slice(format!("HISAL:4:5+X:Y:1,00@{}@'", trailer.len()).as_bytes());
    forged.extend_from_slice(trailer);
    assert!(
        syntax::parse(&forged)
            .unwrap()
            .iter()
            .any(|s| s.id() == "HISAL")
    );
    assert!(
        client
            .unwrap(&reseal(&public, &forged, "dialog", 1), 1, "dialog")
            .is_err()
    );
    // Control: the genuine plaintext, sealed again, still verifies.
    assert!(
        client
            .unwrap(&reseal(&public, &plain, "dialog", 1), 1, "dialog")
            .is_ok()
    );
}

#[test]
fn segment_ranges_follow_parser_binary_rule() {
    let data = b"A:1:1+x@3@'b'B:2:1+@2@'@'\r\nC:3:1+y'";
    let ranges = segment_ranges(data).unwrap();
    assert_eq!(ranges.len(), syntax::parse(data).unwrap().len());
    assert_eq!(&data[ranges[0].0..ranges[0].1], b"A:1:1+x@3@'");
}
