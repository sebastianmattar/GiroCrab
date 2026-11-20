#!/usr/bin/env bash
# Keeps hidden behaviour out of the Rust core and the MCP adapter (#948): no
# `unsafe` outside girocrab-core/src/sys.rs, no spawned processes, no embedded
# files beyond the data tables, no dynamically loaded code and no encoded blobs
# in the payment path. Every exception is listed in this script (or in
# girocrab-core/clippy.toml), so widening one is a visible change to it.
#
# Without arguments it reads the sources (seconds; pre-push and the CI lint step).
# Text patterns can be written around, so two checks back it up:
# - `--compiled [cargo args]` runs `cargo check -p girocrab-core --lib` with the given
#   flags and reads from Cargo's dep-info which files and env vars the compiler took in: only non-test sources,
#   the data tables and the build script's product ID, however they were named.
# - Clippy bans process and loader calls by resolved path (girocrab-core/clippy.toml).
set -euo pipefail
cd "$(dirname "$0")/.."

# Test code: exempt from the process and include rules below, and never part of
# the shipped library (`--compiled`). Data tables: girocrab-core/data/<name>.tsv.
test_re='(^|/)tests(\.rs$|/)|_tests\.rs$|(^|/)test_support\.rs$'
tables="merchants fints_directory bic_directory"

if [ "${1:-}" = --compiled ]; then
    shift
    TEST_RE=$test_re TABLES=$tables python3 -I - "$@" <<'PY'
import json, os, re, subprocess, sys

check = subprocess.run(
    ["cargo", "check", "-q", "-p", "girocrab-core", "--lib", "--message-format=json-render-diagnostics", *sys.argv[1:]],
    stdout=subprocess.PIPE, text=True)
if check.returncode:
    sys.exit(check.returncode)
# The library build (not a test build) names its .rmeta; its dep-info lies next to it.
# Its build script's OUT_DIR holds the generated product ID (the layout of build
# directories differs between cargo versions, so take it from cargo).
rmeta, out_dirs = None, {}
for line in check.stdout.splitlines():
    msg = json.loads(line)
    target = msg.get("target") or {}
    if msg["reason"] == "build-script-executed":
        out_dirs[msg["package_id"]] = msg["out_dir"]
    elif (msg["reason"] == "compiler-artifact" and target["name"] == "girocrab_core"
            and "lib" in target["kind"] and not msg["profile"]["test"]):
        rmeta = next(f for f in msg["filenames"] if f.endswith(".rmeta"))
        package = msg["package_id"]
if rmeta is None or package not in out_dirs:
    sys.exit("lint-no-obfuscation: cargo reported no girocrab_core library or no OUT_DIR for it")
dep_info = os.path.join(os.path.dirname(rmeta), os.path.basename(rmeta)[3:-6] + ".d")
product_id = os.path.normpath(os.path.join(out_dirs[package], "builtin_product_id.rs"))

root = os.getcwd()
test = re.compile(os.environ["TEST_RE"])
data = {f"girocrab-core/data/{t}.tsv" for t in os.environ["TABLES"].split()}
env_ok = {"CARGO_PKG_VERSION", "OUT_DIR"}
bad = []
with open(dep_info) as f:
    for line in f.read().splitlines():
        if line.startswith("# env-dep:"):
            name = line[len("# env-dep:"):].split("=", 1)[0]  # never print the value
            if name not in env_ok:
                bad.append(f"env!/option_env! {name}")
        elif line.endswith(":") and ": " not in line:  # one input per line
            path = os.path.normpath(os.path.join(root, line[:-1].replace("\\ ", " ")))
            rel = os.path.relpath(path, root)
            if re.fullmatch(r"girocrab-core/src/.+\.rs", rel) and not test.search(rel):
                continue
            if rel in data or path == product_id:
                continue
            bad.append(rel)
if bad:
    print("Read by the compiler for the shipped girocrab-core library, not allowed:", file=sys.stderr)
    print("\n".join(bad), file=sys.stderr)
    print("Test files, fixtures and other files stay out of it; a new data table or variable", file=sys.stderr)
    print("needs its entry in scripts/lint-no-obfuscation.sh (#948).", file=sys.stderr)
    sys.exit(1)
PY
    exit 0
fi

fail=0
# Runs <command>, which prints the offending lines (none = passed). A command that
# fails stops the script: a check that could not run must not pass.
report() { # <what is wrong> <command…>
    local what=$1 out
    shift
    out=$("$@") || { echo "lint-no-obfuscation: could not check: $what" >&2; exit 2; }
    [ -n "$out" ] || return 0
    printf '%s\n%s\n\n' "$what" "$out" >&2
    fail=1
}
# grep, where no match is a pass and only an error (2) fails.
matches() { grep "$@" || [ $? -eq 1 ]; }

# Prints `path:line: code` for each line matching the ERE <re>, skipping comment
# lines, text matching the ERE <skip> and the exceptions in <allow> ("path|code"
# per line, code trimmed). Regexes go through ENVIRON, which keeps backslashes.
find_code() { # <re> <skip> <allow> <files…>
    local re=$1 skip=$2 allow=$3
    shift 3
    [ $# -gt 0 ] || return 0 # awk would read stdin
    RE=$re SKIP=$skip ALLOW=$allow awk '
        BEGIN {
            re = ENVIRON["RE"]; skip = ENVIRON["SKIP"]
            n = split(ENVIRON["ALLOW"], a, "\n"); for (i = 1; i <= n; i++) ok[a[i]] = 1
        }
        # A line that matches neither as it is cannot match once trimmed or skipped.
        $0 !~ re && (skip == "" || $0 !~ skip) { next }
        {
            line = $0
            sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line)
            if (line ~ /^\/\//) next
            code = line
            if (skip != "") gsub(skip, "", code)
            if (code ~ re && !((FILENAME "|" line) in ok)) print FILENAME ":" FNR ": " line
        }' "$@"
}

# The level of `unsafe_code` for every workspace member, from its Cargo.toml
# `[lints]` (all targets, build scripts included): forbidden, and in girocrab-core
# denied so that sys.rs can lift it.
unsafe_levels() {
    python3 -I - <<'PY'
import json, os, re, subprocess
try:
    import tomllib
except ModuleNotFoundError:
    tomllib = None

meta = json.loads(subprocess.run(
    ["cargo", "metadata", "-q", "--no-deps", "--format-version", "1", "--offline"],
    check=True, capture_output=True, text=True).stdout)
root = meta["workspace_root"]

def load(manifest):
    if tomllib is not None:
        with open(manifest, "rb") as f:
            return tomllib.load(f)
    out, sec = {}, []
    for raw in open(manifest, encoding="utf-8"):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        m_sec = re.fullmatch(r"\[([^\[\]]+)\]", line)
        if m_sec:
            sec = [p.strip().strip('"') for p in m_sec.group(1).split(".")]
            cur = out
            for k in sec:
                cur = cur.setdefault(k, {})
            continue
        m_kv = re.match(r'^([A-Za-z0-9_.-]+)\s*=\s*(.+)$', line)
        if m_kv and sec:
            keys = sec + [p.strip().strip('"') for p in m_kv.group(1).split(".")]
            val_s = m_kv.group(2).strip()
            if val_s in ("true", "false"):
                val = (val_s == "true")
            elif val_s.startswith('"') and val_s.endswith('"'):
                val = val_s[1:-1]
            elif val_s.startswith("{") and "level" in val_s:
                ml = re.search(r'level\s*=\s*"([^"]+)"', val_s)
                val = {"level": ml.group(1)} if ml else {}
            else:
                val = val_s
            cur = out
            for k in keys[:-1]:
                cur = cur.setdefault(k, {})
            cur[keys[-1]] = val
    return out

workspace = load(os.path.join(root, "Cargo.toml")).get("workspace", {}).get("lints", {}).get("rust", {})
for package in meta["packages"]:
    lints = load(package["manifest_path"]).get("lints", {})
    level = (workspace if lints.get("workspace") else lints.get("rust", {})).get("unsafe_code")
    if isinstance(level, dict):
        level = level.get("level")
    want = "deny" if package["name"] == "girocrab-core" else "forbid"
    if level != want:
        print(f"{os.path.relpath(package['manifest_path'], root)}: unsafe_code = \"{want}\" (is {level})")
PY
}

# Tracked and new (not ignored) files that exist; paths contain no spaces, so
# the lists below are split on whitespace.
src=$(git ls-files --cached --others --exclude-standard 'girocrab-core/src/*.rs' girocrab-core/build.rs 'girocrab-mcp/src/*.rs' |
    sort -u | while read -r f; do if [ -f "$f" ]; then echo "$f"; fi; done)
prod=$(grep -Ev "$test_re" <<<"$src")
word='(^|[^A-Za-z0-9_])'

cargo_config=$(git ls-files '*.cargo/config*' '*Cargo.toml')

report "unsafe_code in Cargo.toml [lints]:" unsafe_levels
report "Lint caps in the Cargo configuration:" matches -nE 'cap-lints|force-warn' $cargo_config
report "unsafe_code lint changed outside girocrab-core/src/sys.rs:" \
    find_code 'unsafe_code' 'forbid[(]unsafe_code[)]' 'girocrab-core/src/sys.rs|#![allow(unsafe_code)]' $src
# Clippy keeps all of its process and loader bans, and nothing lifts them.
missing=""
for p in std::process::Command tokio::process::Command libc::fork libc::execv libc::execve libc::execvp \
    libc::execl libc::execle libc::execlp libc::posix_spawn libc::posix_spawnp libc::system libc::popen \
    libc::dlopen libc::dlsym; do
    grep -qF "\"$p\"" girocrab-core/clippy.toml || missing+="$p"$'\n'
done
report "Ban missing from girocrab-core/clippy.toml:" printf '%s' "${missing%$'\n'}"
report "Clippy bans lifted (disallowed_*, or the whole group):" \
    find_code 'clippy::(all|style|disallowed_)' '' 'girocrab-core/src/connectors/mod.rs|#[allow(clippy::disallowed_methods)]' $prod

# No processes: no `Command` at all, from std::process only id() and exit(),
# no spawning libc calls (clippy also catches them under another name).
report "Process spawning (std::process, Command, libc):" \
    find_code "${word}Command([^A-Za-z0-9_]|$)|process::|(std|tokio)::process|libc::(fork|vfork|exec|posix_spawn|system|popen)" \
        'process::(id|exit)[(]' '' $prod

# Files compiled into the binary: the data tables, the build script's FinTS
# product ID (masked, docs/fints-registrierung.md) and test fixtures. The macros
# also count when imported or renamed (`use std::include_str as …`).
included='include_str![(]"(\.\./)+data/('"${tables// /|}"')\.tsv"[)]|include![(]concat![(]env![(]"OUT_DIR"[)], "/builtin_product_id\.rs"[)][)]|include_(str|bytes)![(]"(\.\./)+tests/fixtures/[^"]+"[)]'
report "include!/include_str!/include_bytes! of another file:" \
    find_code '(^|[^A-Za-z0-9_@])include(_str|_bytes)?([!;,}]|[[:space:]]+as[[:space:]]|$)' "$included" '' $prod

# No code loaded at run time, no assembly, no native libraries or foreign
# functions declared by hand (UniFFI generates its own; tests included), and no
# build-script directive that links, sets cfgs or embeds an environment variable.
report "Dynamic loading, assembly or foreign code:" \
    find_code "dlopen|dlsym|libloading|${word}(global_|naked_)?asm!|#\\[link[(]|extern *(\"[^\"]*\" *)?[{]|rustc-(env|link|flags|cfg|cdylib)" '' '' $src
report "Crates that load code or start processes (Cargo.lock):" \
    matches -nE '^name = "(libloading|dlopen|dlopen2|ctor|linkme|inventory|duct|subprocess|open|opener|webbrowser|async-process|process-wrap|command-group|nix)"$' Cargo.lock

# The payment path builds SEPA/FinTS/bunq orders and GiroCodes from plain fields:
# nothing in it decodes Base64/hex or assembles text from escapes or byte values.
# Every file in its directories counts except the ones that decode for the FinTS
# and bunq transport, keys and statements; the listed files elsewhere too.
pay_exempt='connectors/fints/(mod|dialog|keyfile|bank_keys|rah10|camt|statements)\.rs|connectors/bunq/protocol\.rs'
pay_files=$(grep -E '^girocrab-core/src/(connectors/(fints|bunq)|mandates|payment_input)/' <<<"$prod" |
    grep -vE "^girocrab-core/src/($pay_exempt)$" || true)
gone=""
for f in payment_input.rs batch_csv.rs iban.rs mandates.rs connectors/transfer_session.rs connectors/persist.rs connectors/demo.rs; do
    if [ -f "girocrab-core/src/$f" ]; then pay_files+=$'\n'"girocrab-core/src/$f"; else gone+="girocrab-core/src/$f"$'\n'; fi
done
report "Payment path file moved or renamed (update the list in this script):" printf '%s' "${gone%$'\n'}"
report "Encoded data in the payment path:" \
    find_code "base64|BASE64|${word}hex([^A-Za-z0-9_]|$)|data_encoding|from_str_radix|char::from_u32|from_utf8[a-z_]*[(](&|vec!|&vec!)? *\\[|${word}0x[0-9A-Fa-f]|\\\\x[0-9A-Fa-f][0-9A-Fa-f]|\\\\u[{][0-9A-Fa-f]+[}]\\\\u[{]" '' '' $pay_files

if [ "$fail" -ne 0 ]; then
    echo "System calls go into girocrab-core/src/sys.rs; any other exception needs its entry in $0 (#948)." >&2
    exit 1
fi
