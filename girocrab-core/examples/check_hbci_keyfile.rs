//! Offline check of an existing keyfile. No vault, registration or bank access.
//! Run: cargo run -q -p girocrab-core --example check_hbci_keyfile -- FILE
use std::io::Read;

use girocrab_core::connectors::fints::keyfile::{KeyFile, MAX_FILE_SIZE};
use zeroize::Zeroizing;

// A developer tool outside the app: it may start osascript (#948 bans processes in the core).
#[allow(clippy::disallowed_types)]
fn password() -> Result<Zeroizing<String>, &'static str> {
    // Static AppleScript only: no filename or password is put on the command line.
    // Capture the hidden dialog's result in memory, never forward its stdout.
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", r#"text returned of (display dialog "Passwort der HBCI-Schlüsseldatei" default answer "" with hidden answer buttons {"Abbrechen", "Prüfen"} default button "Prüfen" cancel button "Abbrechen" with title "GiroCrab · Offline-Prüfung")"#])
        .output().map_err(|_| "Passwortdialog konnte nicht geöffnet werden.")?;
    let stdout = Zeroizing::new(output.stdout);
    if !output.status.success() {
        return Err("Abgebrochen oder Passwortdialog nicht verfügbar.");
    }
    let value =
        std::str::from_utf8(&stdout).map_err(|_| "Passwort konnte nicht übernommen werden.")?;
    // osascript appends one newline; preserve spaces and all password characters.
    let value = value.strip_suffix('\n').unwrap_or(value);
    Ok(Zeroizing::new(value.to_string()))
}

fn check() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        return Err(
            "Aufruf: cargo run -q -p girocrab-core --example check_hbci_keyfile -- DATEIPFAD"
                .into(),
        );
    }
    let file = std::fs::File::open(&args[0]).map_err(|_| "Datei konnte nicht geöffnet werden.")?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take((MAX_FILE_SIZE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Datei konnte nicht gelesen werden.")?;
    if bytes.len() > MAX_FILE_SIZE {
        return Err("Datei größer als 4 MiB.".into());
    }
    let secret = password()?;
    let file = KeyFile::open(&bytes, secret.as_bytes()).map_err(|e| e.to_string())?;
    println!(
        "Offline-Prüfung erfolgreich: Passwort und Datei-Prüfwert stimmen; enthaltene private RSA-Schlüssel sind mathematisch konsistent."
    );
    println!(
        "Bankzugänge: {}. Bankschlüssel: {}. Keine Bank kontaktiert, keine Datei verändert.",
        file.identities.len(),
        file.bank_keys.len()
    );
    if file.identities.iter().any(|identity| {
        [0, 1].iter().any(|kind| {
            !file
                .bank_keys
                .iter()
                .any(|k| k.blz == identity.blz && k.kind == *kind)
        })
    }) {
        println!(
            "Für einen Bankabruf fehlen öffentliche Bankschlüssel. Diese im Einrichtungsdialog abrufen und anhand der offiziellen Commerzbank-Prüfwerte bestätigen."
        );
    }
    println!("Dies bestätigt noch keine erfolgreiche RAH-10-Anmeldung bei der Bank.");
    Ok(())
}

fn main() {
    if let Err(message) = check() {
        eprintln!("{message}");
        std::process::exit(1);
    }
}
