#!/usr/bin/env python3
"""Transforms behind scripts/sync-docs-and-website.sh (pure file edits, no git/network).

  sync_docs.py readme  <repo-root> --version vX.Y.Z   # README.md + docs/assets/download-macos.svg
  sync_docs.py site    <repo-root> <site-dir> --version vX.Y.Z

Only machine-derivable facts are rewritten; all prose stays untouched:
  * version strings (download button alt text / SVG, "Version vX", "Aktuelle Version", meta description)
  * the block between `<!-- sync:connectors:start -->` and `<!-- sync:connectors:end -->`
    (README DE/EN, website FAQ): the bank & capability matrix derived from `BANK_MATRIX` in
    girocrab-core/src/connectors/catalog.rs (interface, capabilities, test status) plus the
    institute count of girocrab-core/data/fints_directory.tsv.
Idempotent: a second run changes nothing.
"""
import argparse
import html
import re
import sys
from pathlib import Path

VERSION_RE = re.compile(r"^v\d+\.\d+\.\d+$")

START, END = "<!-- sync:connectors:start -->", "<!-- sync:connectors:end -->"
# The website uses its own markers (the filter input sits above them, outside the generated part).
SITE_START, SITE_END = "<!-- BEGIN GENERATED BANKS TABLE -->", "<!-- END GENERATED BANKS TABLE -->"
REPO_URL = "https://github.com/GiroCrab/GiroCrab"

CAPS = ["Transactions", "Balances", "Securities", "SecurityTransactions", "Transfers", "StandingOrders", "Documents"]
INTERFACES = {
    "Fints": "FinTS 3.0",
    "RestApi": "REST-API",
    "EnableBanking": "Enable Banking (PSD2)",
    "WebPortal": "Web-Portal",
    "FileImport": "Datei-Import",
}
STATUSES = ["Verified", "TestingWanted", "Limited"]

# catalog.rs entry names -> display names per language (everything else is shown as is).
NAMES = {
    "de": {"Bank per PSD2 (Enable Banking)": "Weitere Banken per PSD2 (Enable Banking)"},
    "en": {
        "Bank per PSD2 (Enable Banking)": "More banks via PSD2 (Enable Banking)",
        "Amazon Bestellungen": "Amazon orders",
        "FinTS-Bank (BLZ + URL)": "FinTS bank (bank code + URL)",
    },
}

TEXT = {
    "de": {
        "heading": ["Bank", "Schnittstelle", "Umsätze", "Salden", "Depot", "Depot-Aktivitäten", "Überweisung",
                    "Dauerauftrag", "PDF/Postfach", "Status"],
        "status": {"Verified": "✅ Verifiziert", "TestingWanted": "🧪 Test gesucht",
                   "Limited": "⚠️ Mit Einschränkungen"},
        "legend": ("✅ mit echten Konten getestet · 🧪 implementiert, Rückmeldungen aus der Praxis "
                   "willkommen (Issue verlinkt) · ⚠️ bekannte Einschränkungen · ✓ umgesetzt · "
                   "– nicht umgesetzt. Bei FinTS hängen Depot und PDF-Kontoauszüge vom Angebot "
                   "der jeweiligen Bank ab. Depot: Positionen; Depot-Aktivitäten: Käufe, Verkäufe und "
                   "Ausschüttungen als eigene Einträge. Anlageklasse und Instrument ermittelt GiroCrab "
                   "für jedes Depot selbst, einige Konnektoren übernehmen sie von der Bank."),
        "more": ("Weitere deutsche Banken mit FinTS 3.0 (PIN/TAN) lassen sich über BLZ und "
                 "FinTS-URL anbinden ({n} Institute im FinTS-Verzeichnis); europäische "
                 "PSD2-Banken über Enable Banking mit einer eigenen, kostenlosen Anwendung."),
    },
    "en": {
        "heading": ["Bank", "Interface", "Transactions", "Balances", "Securities", "Securities activity", "Transfers",
                    "Standing orders", "PDF/post box", "Status"],
        "status": {"Verified": "✅ Verified", "TestingWanted": "🧪 Testing wanted",
                   "Limited": "⚠️ Limited"},
        "legend": ("✅ tested with real accounts · 🧪 implemented, feedback from real accounts "
                   "welcome (issue linked) · ⚠️ known limitations · ✓ implemented · – not "
                   "implemented. For FinTS, securities and PDF statements depend on what the "
                   "individual bank offers. Securities: positions; securities activity: buys, sells "
                   "and distributions as entries of their own. GiroCrab works out asset class and "
                   "instrument for every depot itself; some connectors take them over from the bank."),
        "more": ("Further German banks with FinTS 3.0 (PIN/TAN) can be connected via bank code "
                 "and FinTS URL ({n} institutions in the FinTS directory); European PSD2 banks "
                 "via Enable Banking with your own free application."),
    },
}


def parse_matrix(text):
    """-> [dict(name, interface, official, capabilities, status, issues)] from `BANK_MATRIX` in catalog.rs."""
    pat = re.compile(
        r'MatrixEntry\s*\{\s*name:\s*"([^"]+)",\s*kind:\s*(?:Some\(ConnectorKind::\w+\)|None),\s*'
        r"interface:\s*Interface::(\w+),\s*(?://[^\n]*\s*)?official:\s*(Some\((?:true|false)\)|None),\s*capabilities:\s*&\[([^\]]*)\],\s*"
        r"status:\s*Status::(\w+),\s*issues:\s*&\[([^\]]*)\],?\s*\}"
    )
    out = []
    for name, iface, official, caps, status, issues in pat.findall(text):
        caps = re.findall(r"Capability::(\w+)", caps)
        if iface not in INTERFACES or status not in STATUSES or not set(caps) <= set(CAPS):
            sys.exit(f"error: unknown interface/status/capability in catalog.rs entry {name!r}")
        out.append({"name": name, "interface": iface, "capabilities": caps,
                    "official": None if official == "None" else official == "Some(true)", "status": status,
                    "issues": [int(i) for i in re.findall(r"\d+", issues)]})
    return out


NOTE_START, NOTE_END = "<!-- sync:connector-note:start -->", "<!-- sync:connector-note:end -->"
# docs/<key>-connector.md -> (BANK_MATRIX name, name shown in the notice)
CONNECTOR_DOCS = {
    "alte-leipziger": ("Alte Leipziger (Bausparen)", "Alte Leipziger"),
    "consorsbank": ("Consorsbank", "Consorsbank"),
    "easybank": ("easybank (ehem. Barclays)", "easybank"),
    "fnz": ("FNZ Bank", "FNZ Bank"),
    "miles-and-more": ("Miles & More Kreditkarte (Deutsche Bank)", "Deutsche Bank / Miles & More"),
    "quirion": ("quirion", "quirion"),
    "scalable-capital": ("Scalable Capital", "Scalable Capital"),
    "trade-republic": ("Trade Republic", "Trade Republic"),
    "volkswagen-bank": ("Volkswagen Bank", "Volkswagen Bank"),
    "uberspace": ("Uberspace", "Uberspace"),
    "kontist": ("Kontist", "Kontist"),
    "schwaebisch-hall": ("Schwäbisch Hall (Bausparen)", "Schwäbisch Hall"),
}


def connector_note(bank, official):
    """Notice block for the top of a connector doc, from the matrix field `official`."""
    de = {True: "offiziell", False: "inoffiziell", None: "noch zu klären"}[official]
    en = {True: "official", False: "unofficial", None: "to be confirmed"}[official]
    return (
        f"{NOTE_START}\n"
        f"> **Hinweis / Note:** Dieser Connector ist ein unabhängiges Community-Projekt und steht in keiner "
        f"Verbindung zur {bank}. Schnittstelle: {de}. Bei inoffiziellen Schnittstellen kann die Nutzung gegen "
        f"die Nutzungsbedingungen der Bank verstoßen.\n"
        f"> This connector is an independent community project and is not affiliated with {bank}. "
        f"Interface: {en}. Using unofficial interfaces may violate the bank's terms of service.\n"
        f"{NOTE_END}"
    )


def replace_connector_note(text, note):
    pat = re.compile(re.escape(NOTE_START) + r".*?" + re.escape(NOTE_END), re.S)
    if pat.search(text):
        return pat.sub(lambda m: note, text)
    nl = text.index("\n") + 1  # after the title line
    return text[:nl] + "\n" + note + "\n" + text[nl:]


def fints_count(tsv_text):
    return sum(1 for l in tsv_text.splitlines() if l.strip() and not l.startswith("#"))


def _cells(lang, e, fmt):
    esc = html.escape if fmt == "html" else (lambda s: s)
    t = TEXT[lang]
    name = NAMES[lang].get(e["name"], e["name"])
    status = t["status"][e["status"]]
    for i in e["issues"]:
        link = f"{REPO_URL}/issues/{i}"
        status += f' <a href="{link}">#{i}</a>' if fmt == "html" else f" [#{i}]({link})"
    caps = ["✓" if c in e["capabilities"] else "–" for c in CAPS]
    return [esc(name), esc(INTERFACES[e["interface"]])] + caps + [status]


def matrix_text(lang, matrix, n_fints, fmt="md"):
    """Matrix + note for one marker block. fmt: 'md' (README) or 'html' (website, no inline JS/CSS)."""
    # Keep the catalog's interface groups, alphabetize their displayed names.
    groups = {interface: i for i, interface in enumerate(dict.fromkeys(e["interface"] for e in matrix))}
    matrix = sorted(matrix, key=lambda e: (
        groups[e["interface"]], NAMES[lang].get(e["name"], e["name"]).casefold()))
    t = TEXT[lang]
    n = f"{n_fints:,}" if lang == "en" else f"{n_fints:,}".replace(",", ".")
    more, legend = t["more"].format(n=n), t["legend"]
    if fmt == "md":
        rows = ["| " + " | ".join(t["heading"]) + " |", "|" + "---|" * len(t["heading"])]
        rows += ["| " + " | ".join(_cells(lang, e, fmt)) + " |" for e in matrix]
        return "\n".join(rows) + f"\n\n{more}\n\n<sub>{legend}</sub>"
    head = "".join(f'<th scope="col">{html.escape(h)}</th>' for h in t["heading"])
    body = "".join(
        f'<tr class="status-{e["status"].lower()}">'
        + "".join(f"<td>{c}</td>" for c in _cells(lang, e, fmt)) + "</tr>"
        for e in matrix)
    return (f'<div class="matrix-wrap"><table class="bank-matrix"><thead><tr>{head}</tr></thead>'
            f"<tbody>{body}</tbody></table></div>"
            f'<p class="matrix-note">{html.escape(more)}</p>'
            f'<p class="matrix-note"><small>{html.escape(legend)}</small></p>')


def replace_block(text, new_inner, start=START, end=END):
    """Replace the content of every marker block; keeps the markers. Returns (text, n_blocks)."""
    pat = re.compile(re.escape(start) + r".*?" + re.escape(end), re.S)
    return pat.subn(lambda m: f"{start}{new_inner(m)}{end}", text)


def replace_version(text, version):
    """Version strings only; patterns are specific so prose is never touched."""
    subs = [
        (r"v\d+\.\d+\.\d+( · Apple Silicon)", rf"{version}\1"),
        (r"(✓ Version )v\d+\.\d+\.\d+", rf"\g<1>{version}"),
        (r"(Aktuelle Version: \*\*)v\d+\.\d+\.\d+(\*\*)", rf"\g<1>{version}\2"),
        (r"(Current version: \*\*)v\d+\.\d+\.\d+(\*\*)", rf"\g<1>{version}\2"),
        (r"(GiroCrab )v\d+\.\d+\.\d+( ist die freie)", rf"\g<1>{version}\2"),
    ]
    for pat, rep in subs:
        text = re.sub(pat, rep, text)
    return text


def read_facts(root):
    matrix = parse_matrix((root / "girocrab-core/src/connectors/catalog.rs").read_text())
    if not matrix:
        sys.exit("error: no BANK_MATRIX entries parsed from catalog.rs (format changed?)")
    n = fints_count((root / "girocrab-core/data/fints_directory.tsv").read_text())
    return matrix, n


def edit(path, fn):
    old = path.read_text()
    new = fn(old)
    if new != old:
        path.write_text(new)
        print(f"updated {path}")
    return new != old


def run_readme(root, version):
    matrix, n = read_facts(root)

    def fix(text):
        text = replace_version(text, version)
        # Two blocks in README order: DE first, EN second.
        langs = iter(["de", "en"])
        text, k = replace_block(text, lambda m: f"\n{matrix_text(next(langs, 'en'), matrix, n)}\n")
        if k != 2:
            sys.exit(f"error: README.md needs exactly 2 sync:connectors blocks (DE, EN), found {k}")
        return text

    edit(root / "README.md", fix)
    edit(root / "docs/assets/download-macos.svg", lambda t: replace_version(t, version))
    by_name = {e["name"]: e for e in matrix}
    for key, (entry, shown) in CONNECTOR_DOCS.items():
        if not (root / f"docs/{key}-connector.md").exists():
            continue
        if entry not in by_name:
            sys.exit(f"error: {entry!r} not in BANK_MATRIX (docs/{key}-connector.md)")
        note = connector_note(shown, by_name[entry]["official"])
        edit(root / f"docs/{key}-connector.md", lambda t, note=note: replace_connector_note(t, note))


def run_site(root, site, version):
    matrix, n = read_facts(root)

    def fix(text):
        text = replace_version(text, version)
        text, k = replace_block(text, lambda m: f"\n{matrix_text('de', matrix, n, 'html')}\n",
                                SITE_START, SITE_END)
        if k < 1:
            sys.exit("error: index.html has no BEGIN/END GENERATED BANKS TABLE block")
        return text

    edit(site / "index.html", fix)
    edit(site / "img/download-macos.svg", lambda t: replace_version(t, version))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["readme", "site"])
    ap.add_argument("root", type=Path)
    ap.add_argument("site", type=Path, nargs="?")
    ap.add_argument("--version", required=True)
    a = ap.parse_args()
    if not VERSION_RE.match(a.version):
        sys.exit(f"error: version must look like v1.2.3, got {a.version!r}")
    if a.mode == "readme":
        run_readme(a.root, a.version)
    else:
        if not a.site:
            sys.exit("error: site mode needs <site-dir>")
        run_site(a.root, a.site, a.version)


if __name__ == "__main__":
    main()
