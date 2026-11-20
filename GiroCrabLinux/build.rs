use cxx_qt_build::{CxxQtBuilder, QmlFile, QmlModule};

fn main() {
    // BankingPolicy: the bundled policy is trusted for 90 days after the build.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo::rustc-env=GIROCRAB_BUILD_UNIX={now}");
    println!("cargo::rerun-if-changed=../policy/banking-policy.json");
    // The app carries the version of the GiroCrab workspace (core and Mac app), so a new release
    // upstream needs no edit here. A changed layout of Cargo.toml stops the build.
    let workspace = std::fs::read_to_string("../Cargo.toml").expect("../Cargo.toml");
    let version = workspace
        .split("[workspace.package]")
        .nth(1)
        .and_then(|section| {
            section
                .lines()
                .find_map(|l| l.trim().strip_prefix("version = "))
        })
        .map(|v| v.trim_matches('"').to_owned())
        .expect("version in [workspace.package] of ../Cargo.toml");
    println!("cargo::rustc-env=GIROCRAB_VERSION={version}");
    println!("cargo::rerun-if-changed=../Cargo.toml");
    // QML files live in qml/ but are registered at the root of module
    // `GiroCrab` (qrc:/qt/qml/GiroCrab/Main.qml): the build runs inside qml/,
    // so the implicit directory import of every file *is* the module and the
    // singletons (AppModel, Theme, Fmt, SF) resolve to one instance each.
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    std::env::set_current_dir(manifest.join("qml")).expect("qml/ directory");
    let mut files: Vec<_> = std::fs::read_dir(".")
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .filter(|n| n.to_string_lossy().ends_with(".qml"))
        .collect();
    files.sort();
    let qml: Vec<QmlFile> = files
        .iter()
        .map(|n| {
            let text = std::fs::read_to_string(n).unwrap_or_default();
            QmlFile::from(n).singleton(text.contains("pragma Singleton"))
        })
        .collect();
    println!("cargo::rerun-if-changed={}", manifest.join("qml").display());
    println!("cargo::rerun-if-changed={}", manifest.join("cpp").display());
    println!(
        "cargo::rerun-if-changed={}",
        manifest.join("qml-optional").display()
    );
    print_web_assets(&manifest);
    english_strings(&manifest);
    help_catalog(&manifest);
    CxxQtBuilder::new_qml_module(QmlModule::new("GiroCrab").qml_files(qml))
        // The default (`Some("")`) copies the crate's headers into the include path and watches the
        // crate directory itself, which Cargo scans recursively, `target/` included: every build
        // would then rerun this script (minutes of moc/qmlcachegen). The only crate header,
        // cpp/webcookies.h, is included from its own directory and watched as a `cpp_file`.
        .crate_include_root(None)
        .qt_module("Quick")
        .qt_module("QuickControls2")
        .qt_module("Widgets")
        .qt_module("PrintSupport")
        // web logins: QNetworkCookie of the QtWebEngine cookie store (cpp/webcookies.cpp)
        .qt_module("Network")
        // „Im Dateimanager anzeigen“: org.freedesktop.FileManager1 (cpp/filemanager.cpp)
        .qt_module("DBus")
        .qrc(manifest.join("assets.qrc"))
        .cpp_file(manifest.join("cpp/prepare.cpp"))
        .cpp_file(manifest.join("cpp/printing.cpp"))
        .cpp_file(manifest.join("cpp/webcookies.h"))
        .cpp_file(manifest.join("cpp/webcookies.cpp"))
        .cpp_file(manifest.join("cpp/translator.cpp"))
        .cpp_file(manifest.join("cpp/filemanager.cpp"))
        // GiroCode in dropped invoices: pages as grey images (QImageReader, QtPdf plugin)
        .cpp_file(manifest.join("cpp/qrscan.cpp"))
        .file(manifest.join("src/bridge.rs"))
        .build();
    // Our C++ (printing.cpp, prepare.cpp, …) sits in the static library cxx-qt links after its
    // Qt libraries. With `--as-needed` (Debian arm64) the linker has already dropped a Qt
    // library nothing needed up to that point, e.g. QPrinter / QWidget::winId go missing.
    // Naming the Qt libraries once more after that library keeps them resolvable.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        for lib in [
            "PrintSupport",
            "Widgets",
            "QuickControls2",
            "Quick",
            "Qml",
            "Network",
            "DBus",
            "Gui",
            "Core",
        ] {
            println!("cargo::rustc-link-lib=dylib=Qt6{lib}");
        }
    }
}

/// The print stylesheet and paginator script of the Mac app (PrintReport.swift `css` and
/// `paginator`), so the Linux printout (QtWebEngine) never drifts from it. Written to
/// OUT_DIR/print_web_assets.rs; fails the build if the Swift file no longer has that shape.
fn print_web_assets(manifest: &std::path::Path) {
    let swift = manifest.join("../GiroCrabApp/Sources/GiroCrabUI/PrintReport.swift");
    println!("cargo::rerun-if-changed={}", swift.display());
    let text = std::fs::read_to_string(&swift)
        .unwrap_or_else(|e| panic!("{}: {e} (print stylesheet of the Mac app)", swift.display()));
    let block = |start: &str, indent: usize| -> String {
        let from = text
            .find(start)
            .unwrap_or_else(|| panic!("PrintReport.swift: `{start}` not found"));
        let open = text[from..].find("\"\"\"\n").expect("opening \"\"\"") + from + 4;
        let close = text[open..].find("\"\"\"").expect("closing \"\"\"") + open;
        text[open..close]
            .trim_end()
            .lines()
            .map(|l| {
                l.get(indent..)
                    .filter(|_| l.starts_with(&" ".repeat(indent)))
                    .unwrap_or(l.trim_start())
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let css = block("private static let css = ", 4);
    let script = block("private static func paginator(", 8);
    // The first two lines are `(function(){` and the `var W=…` line with Swift
    // interpolations; QML writes them itself (A4, footer, "Seite {p} von {n}").
    let mut lines = script.lines();
    assert_eq!(
        lines.next(),
        Some("(function(){"),
        "PrintReport.swift: paginator start changed"
    );
    let vars = lines.next().unwrap_or_default();
    assert!(
        vars.starts_with("var W=") && vars.contains("FOOTER=") && vars.contains("PAGEOF="),
        "PrintReport.swift: paginator variables changed: {vars}"
    );
    let body: Vec<&str> = lines.collect();
    assert!(
        !css.contains("\\(") && !body.iter().any(|l| l.contains("\\(")),
        "PrintReport.swift: new Swift interpolation in css/paginator, extend build.rs"
    );
    let out =
        std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("print_web_assets.rs");
    std::fs::write(
        out,
        format!(
            "pub const PRINT_CSS: &str = {css:?};\npub const PRINT_PAGINATOR: &str = {:?};\n",
            body.join("\n")
        ),
    )
    .expect("write print_web_assets.rs");
}

/// English UI texts: the Mac app's string catalog (Localizable.xcstrings, German keys) plus
/// the Linux-only texts in i18n/en-linux.json (which win), both with Qt placeholders
/// (`%1`, `%2` …). Written to OUT_DIR/en.json (German → English) for src/i18n.rs and
/// cpp/translator.cpp; fails the build if the catalog no longer has the expected shape.
fn english_strings(manifest: &std::path::Path) {
    use serde_json::Value;
    use std::collections::BTreeMap;
    let catalog = manifest.join("../GiroCrabApp/Xcode/App/Localizable.xcstrings");
    let linux = manifest.join("i18n/en-linux.json");
    println!("cargo::rerun-if-changed={}", catalog.display());
    println!("cargo::rerun-if-changed={}", linux.display());
    let read = |path: &std::path::Path| -> Value {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{}: {e} (English UI texts)", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    };
    let root = read(&catalog);
    assert_eq!(
        root["sourceLanguage"], "de",
        "Localizable.xcstrings: sourceLanguage is no longer \"de\", extend build.rs"
    );
    let strings = root["strings"]
        .as_object()
        .expect("Localizable.xcstrings: `strings` is not an object, extend build.rs");
    // `stringUnit.value`, or the "other" form of a plural `variations`.
    let unit = |key: &str, v: &Value| -> String {
        if let Some(s) = v["stringUnit"]["value"].as_str() {
            return s.to_string();
        }
        v["variations"]["plural"]["other"]["stringUnit"]["value"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "Localizable.xcstrings: English of {key:?} has an unknown shape: {v}, \
                     extend build.rs"
                )
            })
            .to_string()
    };
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (key, entry) in strings {
        // No `localizations` at all: marked `"shouldTranslate": false` (placeholders
        // such as „DE00 …“) or not translated yet – Qt shows the German source
        // either way (scripts/i18n-report.py lists the Linux texts among them).
        let Some(localizations) = entry.get("localizations").and_then(Value::as_object) else {
            continue;
        };
        let Some(en) = localizations.get("en") else {
            continue;
        };
        let mut value = unit(key, en);
        // `%#@docs@` → the "other" form of substitution `docs`, `%arg` → its argument.
        if let Some(subs) = en.get("substitutions") {
            let subs = subs.as_object().unwrap_or_else(|| {
                panic!("Localizable.xcstrings: substitutions of {key:?} are not an object")
            });
            for (name, sub) in subs {
                let num = sub["argNum"].as_u64().unwrap_or_else(|| {
                    panic!("Localizable.xcstrings: substitution {name} of {key:?} has no argNum")
                });
                let spec = sub["formatSpecifier"].as_str().unwrap_or("@");
                let form = unit(key, sub).replace("%arg", &format!("%{num}${spec}"));
                value = value.replace(&format!("%#@{name}@"), &form);
            }
        }
        assert!(
            !value.contains("%#@"),
            "Localizable.xcstrings: unresolved substitution in {key:?}: {value}, extend build.rs"
        );
        let (key, value) = (qt_placeholders(key), qt_placeholders(&value));
        // A menu text escapes `&` (mnemonic): "Speichern && anwenden".
        if key.contains('&') {
            map.entry(key.replace('&', "&&"))
                .or_insert_with(|| value.replace('&', "&&"));
        }
        // "%@ von %@" and "%1$@ von %2$@" are the same Qt text: the first one wins.
        map.entry(key).or_insert(value);
    }
    let own = read(&linux);
    let own = own
        .as_object()
        .expect("i18n/en-linux.json: not an object (German → English)");
    let marks =
        |s: &str| -> Vec<u32> { (1..=9).filter(|n| s.contains(&format!("%{n}"))).collect() };
    for (key, value) in own {
        let value = value
            .as_str()
            .unwrap_or_else(|| panic!("i18n/en-linux.json: {key:?} is not a string"));
        assert_eq!(
            marks(key),
            marks(value),
            "i18n/en-linux.json: placeholders of {key:?} and {value:?} differ"
        );
        map.insert(key.clone(), value.to_string());
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("en.json");
    std::fs::write(out, serde_json::to_string(&map).expect("en.json")).expect("write en.json");
}

/// Swift format specifiers → Qt's numbered placeholders: `%1$@` → `%1`, otherwise numbered
/// in order (`%@ … %lld` → `%1 … %2`). `%%` becomes `%`; a lone `%` ("30 %") stays text.
fn qt_placeholders(s: &str) -> String {
    const SPECS: [&str; 9] = ["@", "lld", "llu", "ld", "lu", "d", "i", "u", "f"];
    let mut out = String::with_capacity(s.len());
    let mut next = 0;
    let mut rest = s;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        if let Some(t) = tail.strip_prefix('%') {
            out.push('%');
            rest = t;
            continue;
        }
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        let position = (digits > 0 && tail[digits..].starts_with('$')).then(|| &tail[..digits]);
        let spec_at = position.map_or(0, |_| digits + 1);
        match SPECS.iter().find(|p| tail[spec_at..].starts_with(*p)) {
            Some(spec) => {
                next += 1;
                match position {
                    Some(n) => out.push_str(&format!("%{n}")),
                    None => out.push_str(&format!("%{next}")),
                }
                rest = &tail[spec_at + spec.len()..];
            }
            None => {
                out.push('%');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The help & FAQ of the Mac app (HelpView.swift `HelpCatalog`: topics in German and English,
/// keyboard shortcut labels), so the Linux help window shows the same texts in both languages.
/// Written to OUT_DIR/help.json; fails the build if the Swift file no longer has that shape.
fn help_catalog(manifest: &std::path::Path) {
    let swift = manifest.join("../GiroCrabApp/Sources/GiroCrabUI/HelpView.swift");
    println!("cargo::rerun-if-changed={}", swift.display());
    let text = std::fs::read_to_string(&swift)
        .unwrap_or_else(|e| panic!("{}: {e} (help catalog of the Mac app)", swift.display()));
    fn fail(what: &str) -> ! {
        panic!("HelpView.swift: {what} – the file changed shape, extend build.rs (help_catalog)")
    }

    // A Swift string literal starting at `s` (`"…"` or a `"""` block): its value and the rest.
    fn literal(s: &str) -> Option<(String, &str)> {
        let s = s.trim_start();
        if let Some(rest) = s.strip_prefix("\"\"\"") {
            let rest = rest.strip_prefix('\n')?;
            let end = rest.find("\"\"\"")?;
            let block = &rest[..end];
            // indentation of the closing delimiter is removed from every line
            let indent = block.rsplit('\n').next().unwrap_or("").len();
            let body: Vec<&str> = block
                .trim_end_matches(' ')
                .trim_end_matches('\n')
                .lines()
                .collect();
            let value = body
                .iter()
                .map(|l| {
                    if l.len() >= indent {
                        &l[indent..]
                    } else {
                        l.trim_start()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            return Some((unescape(&value)?, &rest[end + 3..]));
        }
        let rest = s.strip_prefix('"')?;
        let mut end = 0;
        let bytes = rest.as_bytes();
        while end < bytes.len() {
            match bytes[end] {
                b'\\' => end += 2,
                b'"' => break,
                _ => end += 1,
            }
        }
        Some((unescape(&rest[..end])?, &rest[end + 1..]))
    }
    fn unescape(s: &str) -> Option<String> {
        if s.contains("\\(") {
            return None; // Swift interpolation: not a plain text any more
        }
        Some(
            s.replace("\\\"", "\"")
                .replace("\\n", "\n")
                .replace("\\\\", "\\"),
        )
    }
    // `key: ["de": "…", "en": "…"]` → (de, en)
    fn pair<'a>(s: &'a str, key: &str) -> Option<(String, String, &'a str)> {
        let s = &s[s.find(&format!("{key}: ["))? + key.len() + 3..];
        let s = &s[s.find("\"de\":")? + 5..];
        let (de, s) = literal(s)?;
        let s = &s[s.find("\"en\":")? + 5..];
        let (en, s) = literal(s)?;
        Some((de, en, s))
    }

    let topics_at = text
        .find("static let topics: [HelpTopic] = [")
        .unwrap_or_else(|| fail("`static let topics` not found"));
    let topics_end = topics_at
        + text[topics_at..]
            .find("\n    ]\n")
            .unwrap_or_else(|| fail("end of topics"));
    let mut topics = Vec::new();
    for chunk in text[topics_at..topics_end].split("HelpTopic(").skip(1) {
        let field = |name: &str| -> Option<String> {
            let at = chunk.find(&format!("{name}: \""))? + name.len() + 2;
            literal(&chunk[at..]).map(|(v, _)| v)
        };
        let Some(id) = field("id") else { break };
        let icon = field("icon").unwrap_or_else(|| fail("topic without icon"));
        let (title_de, title_en, rest) =
            pair(chunk, "title").unwrap_or_else(|| fail("topic title"));
        let (body_de, body_en, rest) = pair(rest, "body").unwrap_or_else(|| fail("topic body"));
        let kw = &rest[rest.find("keywords: [").unwrap_or_else(|| fail("keywords")) + 11..];
        let kw = &kw[..kw.find(']').unwrap_or_else(|| fail("keywords end"))];
        let keywords: Vec<String> = kw
            .split(',')
            .filter_map(|k| literal(k).map(|(v, _)| v))
            .collect();
        topics.push(serde_json::json!({
            "id": id, "icon": icon,
            "title": { "de": title_de, "en": title_en },
            "body": { "de": body_de, "en": body_en },
            "keywords": keywords,
        }));
    }
    if topics.len() < 3 {
        fail("fewer than three topics found");
    }

    let sc_at = text
        .find("static let shortcuts:")
        .unwrap_or_else(|| fail("`static let shortcuts` not found"));
    let sc_end = sc_at
        + text[sc_at..]
            .find("\n    ]")
            .unwrap_or_else(|| fail("end of shortcuts"));
    let mut shortcuts = serde_json::Map::new();
    for line in text[sc_at..sc_end]
        .lines()
        .filter(|l| l.trim_start().starts_with("(\""))
    {
        let mut parts = Vec::new();
        let mut rest = line.trim_start().strip_prefix('(').unwrap_or_default();
        while let Some((v, r)) = literal(rest) {
            parts.push(v);
            rest = r.trim_start().strip_prefix(',').unwrap_or(r);
        }
        if parts.len() != 3 {
            fail(&format!("shortcut line `{}`", line.trim()));
        }
        shortcuts.insert(
            parts[1].clone(),
            serde_json::Value::String(parts[2].clone()),
        );
    }

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("help.json");
    let json = serde_json::json!({ "topics": topics, "shortcuts": shortcuts });
    std::fs::write(out, json.to_string()).expect("write help.json");
}
