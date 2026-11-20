#!/usr/bin/env python3
"""Copies the browser scripts of EquatePlusUI.swift into
GiroCrabLinux/qml/EquatePlusWebLoginFlow.qml, between the "BEGIN generated" and
"END generated" markers, as JavaScript template literals:

- hook:    EquatePlusScripts.hook (user script, main frame, document start, page world)
- probe:   EquatePlusScripts.probe
- guard:   EquatePlusWebSheet.runScript › guardScript
- collect: the script of EquatePlusWebSheet.start()

Run after changing the Swift scripts:  python3 -I scripts/gen-equateplus-qml.py
scripts/test-equateplus-linux.cjs (CI) fails while both differ.
"""
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent
SWIFT = ROOT / "GiroCrabApp/Sources/GiroCrabUI/EquatePlusUI.swift"
QML = ROOT / "GiroCrabLinux/qml/EquatePlusWebLoginFlow.qml"
NAMES = ["hook", "probe", "guard", "collect"]
BEGIN, END = "    // BEGIN generated: EquatePlusScripts", "    // END generated"


def dedent(body, indent):
    """Swift strips the closing delimiter's indentation from every line of a multi-line string."""
    pad = " " * indent
    return "\n".join(l[indent:] if l.startswith(pad) else l.lstrip(" ") for l in body.split("\n"))


def plain(m, what):
    """A one-line Swift string literal without escapes or interpolation."""
    if not m:
        raise SystemExit(f"{SWIFT.name}: {what} not found")
    if "\\" in m.group(1):
        raise SystemExit(f"{SWIFT.name}: {what} has escapes; extend the generator")
    return m.group(1)


def swift_scripts():
    src = SWIFT.read_text()
    hook = re.search(r'static let hook = #"""\n([\s\S]*?)\n    """#', src)
    guard = re.search(r'let guardScript = """\n([\s\S]*?)\n        """', src)
    if not hook or not guard:
        raise SystemExit(f"{SWIFT.name}: hook or guardScript not found")
    if "\\" in guard.group(1):
        raise SystemExit(f"{SWIFT.name}: guardScript has escapes; extend the generator")
    return {
        "hook": dedent(hook.group(1), 4),
        "probe": plain(re.search(r'static let probe = "([^"\n]*)"', src), "probe"),
        "guard": dedent(guard.group(1), 8),
        "collect": plain(re.search(r'callAsyncJavaScript\(\n\s*"([^"\n]*collect\(\);)"', src), "collect"),
    }


def template_literal(s):
    return s.replace("\\", "\\\\").replace("`", "\\`").replace("${", "\\${")


def main():
    scripts = swift_scripts()
    block = "\n".join(f"    readonly property string {n}: `{template_literal(scripts[n])}`" for n in NAMES)
    text = QML.read_text()
    i, j = text.index(BEGIN), text.index(END)
    QML.write_text(text[:i] + BEGIN + " (EquatePlusUI.swift)\n" + block + "\n" + text[j:])
    print(f"updated {QML.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
