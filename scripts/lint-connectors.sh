#!/usr/bin/env bash
set -euo pipefail

python3 - <<'PY'
import pathlib, re, sys

root = pathlib.Path(".")
conn_dir = root / "girocrab-core/src/connectors"
allowlist = {
    "mod", "tests", "persist", "bank_directory", "fints_directory", "fints_presets",
    "types", "error", "http", "amazon_documents", "broker_documents",
    "comdirect_postbox", "dpop", "paypal_nvp",  # Submodule von paypal.rs (payPalSignatureKeys)
    "consorsbank_archive",  # Dokumentquelle neben FinTS (#1024), kein eigener Konnektor
}

modules = sorted(
    p.stem for p in conn_dir.glob("*.rs") if p.stem not in allowlist
) + sorted(
    p.parent.name for p in conn_dir.glob("*/mod.rs") if p.parent.name not in allowlist
)

other_rust = "\n".join(
    p.read_text(errors="ignore")
    for p in (root / "girocrab-core/src").rglob("*.rs")
    if "connectors" not in p.parts
)
ui_code = "\n".join(
    p.read_text(errors="ignore")
    for p in list((root / "GiroCrabApp/Sources").rglob("*.swift"))
             + list((root / "GiroCrabLinux").rglob("*.rs"))
             + list((root / "GiroCrabWindows").rglob("*.rs"))
)

missing = []
for m in dict.fromkeys(modules):
    alt = m.replace("_", "[-_]?")
    pat = re.compile(rf"\b{alt}\b", re.I)
    if not pat.search(other_rust) or not pat.search(ui_code):
        missing.append(m)

if missing:
    print("FEHLER: Konnektoren ohne vollstaendige Core/FFI- & UI/Katalog-Verdrahtung:", ", ".join(missing))
    sys.exit(1)

print("OK: Alle Bank-Konnektoren sind in Core und UI/Katalog verdrahtet.")
PY
