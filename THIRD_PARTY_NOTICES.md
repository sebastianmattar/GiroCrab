# Third-Party Open Source Licenses & Notices

GiroCrab is built on top of open-source software in the Rust and Swift ecosystems.
The macOS user interface uses only native Apple system frameworks (`SwiftUI`, `AppKit`, `PDFKit`, `Vision`, `Security`).

## Core Direct Dependencies (`girocrab-core`)

| Crate | Version | License | Repository |
|---|---|---|---|
| **`async-trait`** | `0.1.92` | `MIT OR Apache-2.0` | [https://github.com/dtolnay/async-trait](https://github.com/dtolnay/async-trait) |
| **`base64`** | `0.22.1` | `MIT OR Apache-2.0` | [https://github.com/marshallpierce/rust-base64](https://github.com/marshallpierce/rust-base64) |
| **`base64`** | `0.23.1` | `MIT OR Apache-2.0` | [https://github.com/marshallpierce/rust-base64](https://github.com/marshallpierce/rust-base64) |
| **`chrono`** | `0.4.45` | `MIT OR Apache-2.0` | [https://github.com/chronotope/chrono](https://github.com/chronotope/chrono) |
| **`futures-util`** | `0.3.34` | `MIT OR Apache-2.0` | [https://github.com/rust-lang/futures-rs](https://github.com/rust-lang/futures-rs) |
| **`getrandom`** | `0.4.3` | `MIT OR Apache-2.0` | [https://github.com/rust-random/getrandom](https://github.com/rust-random/getrandom) |
| **`hex`** | `0.4.3` | `MIT OR Apache-2.0` | [https://github.com/KokaKiwi/rust-hex](https://github.com/KokaKiwi/rust-hex) |
| **`jsonwebtoken`** | `12.0.0` | `MIT` | [https://github.com/Keats/jsonwebtoken](https://github.com/Keats/jsonwebtoken) |
| **`miniz_oxide`** | `0.9.1` | `MIT OR Zlib OR Apache-2.0` | [https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide](https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide) |
| **`libc`** | `0.2.191` | `MIT OR Apache-2.0` | [https://github.com/rust-lang/libc](https://github.com/rust-lang/libc) |
| **`p256`** | `0.14.0` | `Apache-2.0 OR MIT` | [https://github.com/RustCrypto/elliptic-curves](https://github.com/RustCrypto/elliptic-curves/tree/master/p256) |
| **`rand`** | `0.9.5` | `MIT OR Apache-2.0` | [https://github.com/rust-random/rand](https://github.com/rust-random/rand) |
| **`rand`** | `0.10.3` | `MIT OR Apache-2.0` | [https://github.com/rust-random/rand](https://github.com/rust-random/rand) |
| **`regex`** | `1.13.1` | `MIT OR Apache-2.0` | [https://github.com/rust-lang/regex](https://github.com/rust-lang/regex) |
| **`reqwest`** | `0.13.5` | `MIT OR Apache-2.0` | [https://github.com/seanmonstar/reqwest](https://github.com/seanmonstar/reqwest) |
| **`rsa`** | `0.10.0` | `MIT OR Apache-2.0` | [https://github.com/RustCrypto/RSA](https://github.com/RustCrypto/RSA) |
| **`rusqlite`** | `0.42.0` | `MIT` | [https://github.com/rusqlite/rusqlite](https://github.com/rusqlite/rusqlite) |
| **`serde`** | `1.0.229` | `MIT OR Apache-2.0` | [https://github.com/serde-rs/serde](https://github.com/serde-rs/serde) |
| **`serde_json`** | `1.0.154` | `MIT OR Apache-2.0` | [https://github.com/serde-rs/json](https://github.com/serde-rs/json) |
| **`sha2`** | `0.11.1` | `MIT OR Apache-2.0` | [https://github.com/RustCrypto/hashes](https://github.com/RustCrypto/hashes) |
| **`simpleicons-rs`** | `16.37.0` | `MIT AND CC0-1.0` | [https://github.com/cscnk52/simpleicons-rs](https://github.com/cscnk52/simpleicons-rs) |
| **`thiserror`** | `2.0.21` | `MIT OR Apache-2.0` | [https://github.com/dtolnay/thiserror](https://github.com/dtolnay/thiserror) |
| **`tokio`** | `1.54.1` | `MIT` | [https://github.com/tokio-rs/tokio](https://github.com/tokio-rs/tokio) |
| **`tokio-tungstenite`** | `0.30.0` | `MIT` | [https://github.com/snapview/tokio-tungstenite](https://github.com/snapview/tokio-tungstenite) |
| **`uniffi`** | `0.32.2` | `MPL-2.0` | [https://github.com/mozilla/uniffi-rs](https://github.com/mozilla/uniffi-rs) |
| **`url`** | `2.5.8` | `MIT OR Apache-2.0` | [https://github.com/servo/rust-url](https://github.com/servo/rust-url) |
| **`uuid`** | `1.29.2` | `Apache-2.0 OR MIT` | [https://github.com/uuid-rs/uuid](https://github.com/uuid-rs/uuid) |
| **`zeroize`** | `1.9.1` | `Apache-2.0 OR MIT` | [https://github.com/RustCrypto/utils](https://github.com/RustCrypto/utils) |

## Build Tooling (not shipped in the app)

| Crate | Version | License | Repository |
|---|---|---|---|
| **`uniffi`** (`uniffi-bindgen`, feature `cli`) | `0.32.2` | `MPL-2.0` | [https://github.com/mozilla/uniffi-rs](https://github.com/mozilla/uniffi-rs) |

## Swift Packages (`GiroCrabApp`)

| Package | Version | License | Repository |
|---|---|---|---|
| **Sparkle** (`GiroCrabMac` only, macOS auto-update) | `2.10.1` | MIT (with bundled third-party components under MIT / BSD-style licenses; see the `LICENSE` file shipped inside `Sparkle.framework`) | [https://github.com/sparkle-project/Sparkle](https://github.com/sparkle-project/Sparkle) |
| **GiroCrabCorePackage** | – | GPL-3.0-or-later (this project's own generated UniFFI package) | local path `generated/GiroCrabCorePackage` |

## Bundled Native Code & Data

| Component | License | Notes |
|---|---|---|
| **SQLCipher 4.20.0 / SQLite 3.54.1** (via `rusqlite` feature `bundled-sqlcipher-vendored-openssl`, crate `sqlcipher-src`) | BSD-style (Zetetic LLC), SQLite public domain | Encrypted vault storage |
| **OpenSSL** (crate `openssl-src`) | Apache-2.0 | Crypto backend for SQLCipher |
| **Simple Icons** (via `simpleicons-rs`) | CC0-1.0 (icon data); brand logos remain trademarks of their owners | Merchant/bank icons |
| **Bundesbank Bankleitzahlendatei** (`tools/gen-bic-directory`) | Public data of Deutsche Bundesbank | BLZ → BIC table |

## Complete Transitive Dependency Tree

<details>
<summary>Click to expand all transitive Rust crates (331 packages)</summary>

| Crate | Version | License | Authors / Repository |
|---|---|---|---|
| `adler2` | `2.0.1` | `0BSD OR MIT OR Apache-2.0` | [Repo](https://github.com/oyvindln/adler2) (Jonas Schievink <jonasschievink@gmail.com>, oyvindln <oyvind) |
| `aho-corasick` | `1.1.5` | `Unlicense OR MIT` | [Repo](https://github.com/BurntSushi/aho-corasick) (Andrew Gallant <jamslam@gmail.com>) |
| `android_system_properties` | `0.1.6` | `MIT OR Apache-2.0` | [Repo](https://github.com/nical/android_system_properties) (Nicolas Silva <nical@fastmail.com>) |
| `anstyle` | `1.0.14` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-cli/anstyle.git) |
| `anyhow` | `1.0.104` | `MIT OR Apache-2.0` | [Repo](https://github.com/dtolnay/anyhow) (David Tolnay <dtolnay@gmail.com>) |
| `askama` | `0.16.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/askama-rs/askama) |
| `askama_derive` | `0.16.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/askama-rs/askama) |
| `askama_macros` | `0.16.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/askama-rs/askama) |
| `askama_parser` | `0.16.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/askama-rs/askama) |
| `async-compat` | `0.2.6` | `Apache-2.0 OR MIT` | [Repo](https://github.com/smol-rs/async-compat) (Stjepan Glavina <stjepang@gmail.com>) |
| `async-trait` | `0.1.92` | `MIT OR Apache-2.0` | [Repo](https://github.com/dtolnay/async-trait) (David Tolnay <dtolnay@gmail.com>) |
| `atomic-waker` | `1.1.2` | `Apache-2.0 OR MIT` | [Repo](https://github.com/smol-rs/atomic-waker) (Stjepan Glavina <stjepang@gmail.com>, Contributors to future) |
| `autocfg` | `1.5.1` | `Apache-2.0 OR MIT` | [Repo](https://github.com/cuviper/autocfg) (Josh Stone <cuviper@gmail.com>) |
| `aws-lc-rs` | `1.19.0` | `ISC AND (Apache-2.0 OR ISC)` | [Repo](https://github.com/aws/aws-lc-rs) (AWS-LibCrypto) |
| `aws-lc-sys` | `0.47.0` | `ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0)` | [Repo](https://github.com/aws/aws-lc-rs) (AWS-LC) |
| `base16ct` | `1.0.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/formats) (RustCrypto Developers) |
| `base64` | `0.22.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/marshallpierce/rust-base64) (Marshall Pierce <marshall@mpierce.org>) |
| `base64` | `0.23.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/marshallpierce/rust-base64) (Marshall Pierce <marshall@mpierce.org>) |
| `base64ct` | `1.8.3` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/formats) (RustCrypto Developers) |
| `basic-toml` | `0.1.10` | `MIT OR Apache-2.0` | [Repo](https://github.com/dtolnay/basic-toml) (Alex Crichton <alex@alexcrichton.com>, David Tolnay <dtolnay) |
| `bitflags` | `2.13.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/bitflags/bitflags) (The Rust Project Developers) |
| `block-buffer` | `0.12.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/RustCrypto/utils) (RustCrypto Developers) |
| `bumpalo` | `3.20.3` | `MIT OR Apache-2.0` | [Repo](https://github.com/fitzgen/bumpalo) (Nick Fitzgerald <fitzgen@gmail.com>) |
| `bytes` | `1.12.1` | `MIT` | [Repo](https://github.com/tokio-rs/bytes) (Carl Lerche <me@carllerche.com>, Sean McArthur <sean@seanmon) |
| `camino` | `1.2.6` | `MIT OR Apache-2.0` | [Repo](https://github.com/camino-rs/camino) (Without Boats <saoirse@without.boats>, Ashley Williams <ashl) |
| `cargo-platform` | `0.3.3` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/cargo) |
| `cargo_metadata` | `0.23.1` | `MIT` | [Repo](https://github.com/oli-obk/cargo_metadata) (Oliver Schneider <git-spam-no-reply9815368754983@oli-obk.de>) |
| `cc` | `1.11.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/cc-rs) |
| `cfg-if` | `1.0.5` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/cfg-if) (Alex Crichton <alex@alexcrichton.com>) |
| `cfg_aliases` | `0.2.2` | `MIT` | [Repo](https://github.com/katharostech/cfg_aliases) (Zicklag <zicklag@katharostech.com>) |
| `chacha20` | `0.10.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/RustCrypto/stream-ciphers) (RustCrypto Developers) |
| `chrono` | `0.4.45` | `MIT OR Apache-2.0` | [Repo](https://github.com/chronotope/chrono) |
| `clap` | `4.6.7` | `MIT OR Apache-2.0` | [Repo](https://github.com/clap-rs/clap) |
| `clap_builder` | `4.6.7` | `MIT OR Apache-2.0` | [Repo](https://github.com/clap-rs/clap) |
| `clap_derive` | `4.6.7` | `MIT OR Apache-2.0` | [Repo](https://github.com/clap-rs/clap) |
| `clap_lex` | `1.1.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/clap-rs/clap) |
| `cmake` | `0.1.58` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/cmake-rs) (Alex Crichton <alex@alexcrichton.com>) |
| `cmov` | `0.5.4` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/utils) (RustCrypto Developers) |
| `combine` | `4.6.9` | `MIT` | [Repo](https://github.com/Marwes/combine) (Markus Westerlind <marwes91@gmail.com>) |
| `const-oid` | `0.10.2` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/formats) (RustCrypto Developers) |
| `cookie` | `0.18.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/SergioBenitez/cookie-rs) (Sergio Benitez <sb@sergio.bz>, Alex Crichton <alex@alexcrich) |
| `cookie_store` | `0.22.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/pfernie/cookie_store) (Patrick Fernie <patrick.fernie@gmail.com>) |
| `core-foundation` | `0.10.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/servo/core-foundation-rs) (The Servo Project Developers) |
| `core-foundation-sys` | `0.8.7` | `MIT OR Apache-2.0` | [Repo](https://github.com/servo/core-foundation-rs) (The Servo Project Developers) |
| `cpubits` | `0.1.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/RustCrypto/utils) (RustCrypto Developers) |
| `cpufeatures` | `0.3.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/RustCrypto/utils) (RustCrypto Developers) |
| `crypto-bigint` | `0.7.11` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/crypto-bigint) (RustCrypto Developers) |
| `crypto-common` | `0.2.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/RustCrypto/traits) (RustCrypto Developers) |
| `crypto-primes` | `0.7.2` | `Apache-2.0 OR MIT` | [Repo](https://github.com/entropyxyz/crypto-primes) |
| `csscolorparser` | `0.8.4` | `MIT OR Apache-2.0` | [Repo](https://github.com/mazznoer/csscolorparser-rs) (Nor Khasyatillah <mazznoer@ymail.com>) |
| `ctutils` | `0.4.8` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/utils) (RustCrypto Developers) |
| `curve25519-dalek` | `5.0.0` | `BSD-3-Clause` | [Repo](https://github.com/dalek-cryptography/curve25519-dalek/tree/main/curve25519-dalek) (Isis Lovecruft <isis@patternsinthevoid.net>, Henry de Valenc) |
| `curve25519-dalek-derive` | `0.1.1` | `MIT/Apache-2.0` | [Repo](https://github.com/dalek-cryptography/curve25519-dalek) |
| `data-encoding` | `2.11.1` | `MIT` | [Repo](https://github.com/ia0/data-encoding) |
| `der` | `0.8.3` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/formats) (RustCrypto Developers) |
| `deranged` | `0.5.9` | `MIT OR Apache-2.0` | [Repo](https://github.com/jhpratt/deranged) (Jacob Pratt <jacob@jhpratt.dev>) |
| `digest` | `0.11.3` | `MIT OR Apache-2.0` | [Repo](https://github.com/RustCrypto/traits) (RustCrypto Developers) |
| `displaydoc` | `0.2.7` | `MIT OR Apache-2.0` | [Repo](https://github.com/yaahc/displaydoc) (Jane Lusby <jlusby@yaah.dev>) |
| `document-features` | `0.2.12` | `MIT OR Apache-2.0` | [Repo](https://github.com/slint-ui/document-features) (Slint Developers <info@slint.dev>) |
| `dunce` | `1.0.5` | `CC0-1.0 OR MIT-0 OR Apache-2.0` | [Repo](https://gitlab.com/kornelski/dunce) (Kornel <kornel@geekhood.net>) |
| `ecdsa` | `0.17.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/signatures) (RustCrypto Developers) |
| `ed25519` | `3.0.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/signatures) (RustCrypto Developers) |
| `ed25519-dalek` | `3.0.0` | `BSD-3-Clause` | [Repo](https://github.com/dalek-cryptography/curve25519-dalek/tree/main/ed25519-dalek) (isis lovecruft <isis@patternsinthevoid.net>, Tony Arcieri <b) |
| `elliptic-curve` | `0.14.3` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/traits) (RustCrypto Developers) |
| `equivalent` | `1.0.3` | `Apache-2.0 OR MIT` | [Repo](https://github.com/indexmap-rs/equivalent) |
| `errno` | `0.3.14` | `MIT OR Apache-2.0` | [Repo](https://github.com/lambda-fairy/rust-errno) (Chris Wong <lambda.fairy@gmail.com>, Dan Gohman <dev@sunfish) |
| `fallible-iterator` | `0.3.0` | `MIT/Apache-2.0` | [Repo](https://github.com/sfackler/rust-fallible-iterator) (Steven Fackler <sfackler@gmail.com>) |
| `fallible-streaming-iterator` | `0.1.9` | `MIT/Apache-2.0` | [Repo](https://github.com/sfackler/fallible-streaming-iterator) (Steven Fackler <sfackler@gmail.com>) |
| `fastrand` | `2.5.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/smol-rs/fastrand) (Stjepan Glavina <stjepang@gmail.com>) |
| `ff` | `0.14.0` | `MIT/Apache-2.0` | [Repo](https://github.com/zkcrypto/ff) (Sean Bowe <ewillbefull@gmail.com>, Jack Grigg <thestr4d@gmai) |
| `fiat-crypto` | `0.3.0` | `MIT OR Apache-2.0 OR BSD-1-Clause` | [Repo](https://github.com/mit-plv/fiat-crypto) (Fiat Crypto library authors <jgross@mit.edu>) |
| `find-msvc-tools` | `0.1.15` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/cc-rs) |
| `foldhash` | `0.2.0` | `Zlib` | [Repo](https://github.com/orlp/foldhash) (Orson Peters <orsonpeters@gmail.com>) |
| `form_urlencoded` | `1.2.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/servo/rust-url) (The rust-url developers) |
| `fs-err` | `3.3.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/andrewhickman/fs-err) (Andrew Hickman <andrew.hickman1@sky.com>) |
| `fs_extra` | `1.3.0` | `MIT` | [Repo](https://github.com/webdesus/fs_extra) (Denis Kurilenko <webdesus@gmail.com>) |
| `futures-channel` | `0.3.34` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/futures-rs) |
| `futures-core` | `0.3.34` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/futures-rs) |
| `futures-io` | `0.3.34` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/futures-rs) |
| `futures-sink` | `0.3.34` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/futures-rs) |
| `futures-task` | `0.3.34` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/futures-rs) |
| `futures-util` | `0.3.34` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/futures-rs) |
| `getrandom` | `0.2.17` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-random/getrandom) (The Rand Project Developers) |
| `getrandom` | `0.3.4` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-random/getrandom) (The Rand Project Developers) |
| `getrandom` | `0.4.3` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-random/getrandom) (The Rand Project Developers) |
| `glob` | `0.3.4` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/glob) (The Rust Project Developers) |
| `goblin` | `0.8.2` | `MIT` | [Repo](https://github.com/m4b/goblin) (m4b <m4b.github.io@gmail.com>, seu <seu@panopticon.re>, Will) |
| `group` | `0.14.0` | `MIT/Apache-2.0` | [Repo](https://github.com/zkcrypto/group) (Sean Bowe <ewillbefull@gmail.com>, Jack Grigg <jack@z.cash>) |
| `hashbrown` | `0.17.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/hashbrown) |
| `hashlink` | `0.12.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/djc/hashlink) |
| `heck` | `0.5.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/withoutboats/heck) |
| `hex` | `0.4.3` | `MIT OR Apache-2.0` | [Repo](https://github.com/KokaKiwi/rust-hex) (KokaKiwi <kokakiwi@kokakiwi.net>) |
| `hmac` | `0.13.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/RustCrypto/MACs) (RustCrypto Developers) |
| `http` | `1.5.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/hyperium/http) (Alex Crichton <alex@alexcrichton.com>, Carl Lerche <me@carll) |
| `http-body` | `1.1.0` | `MIT` | [Repo](https://github.com/hyperium/http-body) (Carl Lerche <me@carllerche.com>, Lucio Franco <luciofranco14) |
| `http-body-util` | `0.1.5` | `MIT` | [Repo](https://github.com/hyperium/http-body) (Carl Lerche <me@carllerche.com>, Lucio Franco <luciofranco14) |
| `httparse` | `1.10.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/seanmonstar/httparse) (Sean McArthur <sean@seanmonstar.com>) |
| `hybrid-array` | `0.4.15` | `MIT OR Apache-2.0` | [Repo](https://github.com/RustCrypto/hybrid-array) (RustCrypto Developers) |
| `hyper` | `1.12.0` | `MIT` | [Repo](https://github.com/hyperium/hyper) (Sean McArthur <sean@seanmonstar.com>) |
| `hyper-rustls` | `0.27.10` | `Apache-2.0 OR ISC OR MIT` | [Repo](https://github.com/rustls/hyper-rustls) |
| `hyper-util` | `0.1.21` | `MIT` | [Repo](https://github.com/hyperium/hyper-util) (Sean McArthur <sean@seanmonstar.com>) |
| `iana-time-zone` | `0.1.65` | `MIT OR Apache-2.0` | [Repo](https://github.com/strawlab/iana-time-zone) (Andrew Straw <strawman@astraw.com>, René Kijewski <rene.kije) |
| `iana-time-zone-haiku` | `0.1.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/strawlab/iana-time-zone) (René Kijewski <crates.io@k6i.de>) |
| `icu_collections` | `2.4.0` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `icu_locale_core` | `2.4.0` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `icu_normalizer` | `2.4.0` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `icu_normalizer_data` | `2.4.0` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `icu_properties` | `2.4.0` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `icu_properties_data` | `2.4.0` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `icu_provider` | `2.4.0` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `idna` | `1.1.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/servo/rust-url/) (The rust-url developers) |
| `idna_adapter` | `1.2.2` | `Apache-2.0 OR MIT` | [Repo](https://github.com/hsivonen/idna_adapter) (The rust-url developers) |
| `indexmap` | `2.14.2` | `Apache-2.0 OR MIT` | [Repo](https://github.com/indexmap-rs/indexmap) |
| `ipnet` | `2.12.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/krisprice/ipnet) (Kris Price <kris@krisprice.nz>) |
| `itoa` | `1.0.19` | `MIT OR Apache-2.0` | [Repo](https://github.com/dtolnay/itoa) (David Tolnay <dtolnay@gmail.com>) |
| `jni` | `0.22.4` | `MIT OR Apache-2.0` | [Repo](https://github.com/jni-rs/jni-rs) (jni team) |
| `jni-macros` | `0.22.4` | `MIT OR Apache-2.0` | [Repo](https://github.com/jni-rs/jni-rs) |
| `jni-sys` | `0.4.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/jni-rs/jni-sys) (Steven Fackler <sfackler@gmail.com>, Robert Bragg <robert@si) |
| `jni-sys-macros` | `0.4.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/jni-rs/jni-sys) (Robert Bragg <robert@sixbynine.org>) |
| `jobserver` | `0.1.35` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/jobserver-rs) (Alex Crichton <alex@alexcrichton.com>) |
| `js-sys` | `0.3.107` | `MIT OR Apache-2.0` | [Repo](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/js-sys) (The wasm-bindgen Developers) |
| `jsonwebtoken` | `12.0.0` | `MIT` | [Repo](https://github.com/Keats/jsonwebtoken) (Vincent Prouillet <hello@vincentprouillet.com>) |
| `libc` | `0.2.191` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/libc) |
| `libm` | `0.2.16` | `MIT` | [Repo](https://github.com/rust-lang/compiler-builtins) (Alex Crichton <alex@alexcrichton.com>, Amanieu d'Antras <ama) |
| `libsqlite3-sys` | `0.40.0` | `MIT` | [Repo](https://github.com/rusqlite/rusqlite) (The rusqlite developers) |
| `linux-raw-sys` | `0.12.1` | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | [Repo](https://github.com/sunfishcode/linux-raw-sys) (Dan Gohman <dev@sunfishcode.online>) |
| `litemap` | `0.8.3` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `litrs` | `1.0.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/LukasKalbertodt/litrs) (Lukas Kalbertodt <lukas.kalbertodt@gmail.com>) |
| `log` | `0.4.35` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/log) (The Rust Project Developers) |
| `lru-slab` | `0.1.3` | `MIT OR Apache-2.0 OR Zlib` | [Repo](https://github.com/Ralith/lru-slab) (Benjamin Saunders <ben.e.saunders@gmail.com>) |
| `memchr` | `2.8.3` | `Unlicense OR MIT` | [Repo](https://github.com/BurntSushi/memchr) (Andrew Gallant <jamslam@gmail.com>, bluss) |
| `minimal-lexical` | `0.2.1` | `MIT/Apache-2.0` | [Repo](https://github.com/Alexhuszagh/minimal-lexical) (Alex Huszagh <ahuszagh@gmail.com>) |
| `miniz_oxide` | `0.9.1` | `MIT OR Zlib OR Apache-2.0` | [Repo](https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide) (Frommi <daniil.liferenko@gmail.com>, oyvindln <oyvindln@user) |
| `mio` | `1.2.4` | `MIT` | [Repo](https://github.com/tokio-rs/mio) (Carl Lerche <me@carllerche.com>, Thomas de Zeeuw <thomasdeze) |
| `nom` | `7.1.3` | `MIT` | [Repo](https://github.com/Geal/nom) (contact@geoffroycouprie.com) |
| `num-bigint` | `0.4.8` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-num/num-bigint) (The Rust Project Developers) |
| `num-conv` | `0.2.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/jhpratt/num-conv) (Jacob Pratt <jacob@jhpratt.dev>) |
| `num-integer` | `0.1.47` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-num/num-integer) (The Rust Project Developers) |
| `num-traits` | `0.2.19` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-num/num-traits) (The Rust Project Developers) |
| `once_cell` | `1.21.4` | `MIT OR Apache-2.0` | [Repo](https://github.com/matklad/once_cell) (Aleksey Kladov <aleksey.kladov@gmail.com>) |
| `openssl-probe` | `0.2.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/rustls/openssl-probe) (Alex Crichton <alex@alexcrichton.com>) |
| `openssl-src` | `300.6.1+3.6.3` | `MIT/Apache-2.0` | [Repo](https://github.com/alexcrichton/openssl-src-rs) (Alex Crichton <alex@alexcrichton.com>) |
| `openssl-sys` | `0.9.118` | `MIT` | [Repo](https://github.com/rust-openssl/rust-openssl) (Alex Crichton <alex@alexcrichton.com>, Steven Fackler <sfack) |
| `p256` | `0.14.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/elliptic-curves) (RustCrypto Developers) |
| `p384` | `0.14.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/elliptic-curves) (RustCrypto Developers, Frank Denis <github@pureftpd.org>) |
| `pem` | `4.0.0` | `MIT` | [Repo](https://github.com/jcreekmore/pem-rs.git) (Jonathan Creekmore <jonathan@thecreekmores.org>) |
| `pem-rfc7468` | `1.0.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/formats) (RustCrypto Developers) |
| `percent-encoding` | `2.3.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/servo/rust-url/) (The rust-url developers) |
| `phf` | `0.13.1` | `MIT` | [Repo](https://github.com/rust-phf/rust-phf) (Steven Fackler <sfackler@gmail.com>) |
| `phf_generator` | `0.13.1` | `MIT` | [Repo](https://github.com/rust-phf/rust-phf) (Steven Fackler <sfackler@gmail.com>) |
| `phf_macros` | `0.13.1` | `MIT` | [Repo](https://github.com/rust-phf/rust-phf) (Steven Fackler <sfackler@gmail.com>) |
| `phf_shared` | `0.13.1` | `MIT` | [Repo](https://github.com/rust-phf/rust-phf) (Steven Fackler <sfackler@gmail.com>) |
| `pin-project-lite` | `0.2.17` | `Apache-2.0 OR MIT` | [Repo](https://github.com/taiki-e/pin-project-lite) |
| `pkcs1` | `0.8.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/formats) (RustCrypto Developers) |
| `pkcs8` | `0.11.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/formats) (RustCrypto Developers) |
| `pkg-config` | `0.3.34` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/pkg-config-rs) (Alex Crichton <alex@alexcrichton.com>) |
| `plain` | `0.2.3` | `MIT/Apache-2.0` | [Repo](https://github.com/randomites/plain) (jzr) |
| `potential_utf` | `0.1.6` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `powerfmt` | `0.2.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/jhpratt/powerfmt) (Jacob Pratt <jacob@jhpratt.dev>) |
| `ppv-lite86` | `0.2.21` | `MIT OR Apache-2.0` | [Repo](https://github.com/cryptocorrosion/cryptocorrosion) (The CryptoCorrosion Contributors) |
| `primefield` | `0.14.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/elliptic-curves) (RustCrypto Developers) |
| `primeorder` | `0.14.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/elliptic-curves) (RustCrypto Developers) |
| `proc-macro2` | `1.0.107` | `MIT OR Apache-2.0` | [Repo](https://github.com/dtolnay/proc-macro2) (David Tolnay <dtolnay@gmail.com>, Alex Crichton <alex@alexcr) |
| `psl-types` | `2.0.11` | `MIT/Apache-2.0` | [Repo](https://github.com/addr-rs/psl-types) (rushmorem <rushmore@webenchanter.com>) |
| `publicsuffix` | `2.3.0` | `MIT/Apache-2.0` | [Repo](https://github.com/rushmorem/publicsuffix) (rushmorem <rushmore@webenchanter.com>) |
| `quinn` | `0.11.12` | `MIT OR Apache-2.0` | [Repo](https://github.com/quinn-rs/quinn) |
| `quinn-proto` | `0.11.19` | `MIT OR Apache-2.0` | [Repo](https://github.com/quinn-rs/quinn) |
| `quinn-udp` | `0.5.16` | `MIT OR Apache-2.0` | [Repo](https://github.com/quinn-rs/quinn) |
| `quote` | `1.0.47` | `MIT OR Apache-2.0` | [Repo](https://github.com/dtolnay/quote) (David Tolnay <dtolnay@gmail.com>) |
| `r-efi` | `5.3.0` | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | [Repo](https://github.com/r-efi/r-efi) |
| `r-efi` | `6.0.0` | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | [Repo](https://github.com/r-efi/r-efi) |
| `rand` | `0.9.5` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-random/rand) (The Rand Project Developers, The Rust Project Developers) |
| `rand` | `0.10.3` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-random/rand) (The Rand Project Developers, The Rust Project Developers) |
| `rand_chacha` | `0.9.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-random/rand) (The Rand Project Developers, The Rust Project Developers, Th) |
| `rand_core` | `0.9.5` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-random/rand) (The Rand Project Developers, The Rust Project Developers) |
| `rand_core` | `0.10.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-random/rand_core) (The Rand Project Developers) |
| `rand_pcg` | `0.10.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-random/rngs) (The Rand Project Developers) |
| `regex` | `1.13.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/regex) (The Rust Project Developers, Andrew Gallant <jamslam@gmail.c) |
| `regex-automata` | `0.4.18` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/regex) (The Rust Project Developers, Andrew Gallant <jamslam@gmail.c) |
| `regex-syntax` | `0.8.11` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/regex) (The Rust Project Developers, Andrew Gallant <jamslam@gmail.c) |
| `reqwest` | `0.13.5` | `MIT OR Apache-2.0` | [Repo](https://github.com/seanmonstar/reqwest) (Sean McArthur <sean@seanmonstar.com>) |
| `rfc6979` | `0.6.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/signatures) (RustCrypto Developers) |
| `ring` | `0.17.14` | `Apache-2.0 AND ISC` | [Repo](https://github.com/briansmith/ring) |
| `rsa` | `0.10.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/RustCrypto/RSA) (RustCrypto Developers, dignifiedquire <dignifiedquire@gmail.) |
| `rsqlite-vfs` | `0.2.0` | `MIT` | [Repo](https://github.com/Spxg/sqlite-wasm-rs) |
| `rusqlite` | `0.42.0` | `MIT` | [Repo](https://github.com/rusqlite/rusqlite) (The rusqlite developers) |
| `rustc-hash` | `2.1.3` | `Apache-2.0 OR MIT` | [Repo](https://github.com/rust-lang/rustc-hash) (The Rust Project Developers) |
| `rustc_version` | `0.4.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/djc/rustc-version-rs) |
| `rustix` | `1.1.6` | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | [Repo](https://github.com/bytecodealliance/rustix) (Dan Gohman <dev@sunfishcode.online>, Jakub Konka <kubkon@jak) |
| `rustls` | `0.23.46` | `Apache-2.0 OR ISC OR MIT` | [Repo](https://github.com/rustls/rustls) |
| `rustls-native-certs` | `0.8.5` | `Apache-2.0 OR ISC OR MIT` | [Repo](https://github.com/rustls/rustls-native-certs) |
| `rustls-pki-types` | `1.15.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/rustls/pki-types) |
| `rustls-platform-verifier` | `0.7.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/rustls/rustls-platform-verifier) |
| `rustls-platform-verifier-android` | `0.2.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/rustls/rustls-platform-verifier) |
| `rustls-webpki` | `0.103.16` | `ISC` | [Repo](https://github.com/rustls/webpki) |
| `rustversion` | `1.0.23` | `MIT OR Apache-2.0` | [Repo](https://github.com/dtolnay/rustversion) (David Tolnay <dtolnay@gmail.com>) |
| `ryu` | `1.0.23` | `Apache-2.0 OR BSL-1.0` | [Repo](https://github.com/dtolnay/ryu) (David Tolnay <dtolnay@gmail.com>) |
| `same-file` | `1.0.6` | `Unlicense/MIT` | [Repo](https://github.com/BurntSushi/same-file) (Andrew Gallant <jamslam@gmail.com>) |
| `schannel` | `0.1.30` | `MIT` | [Repo](https://github.com/steffengy/schannel-rs) (Steven Fackler <sfackler@gmail.com>, Steffen Butzer <steffen) |
| `scroll` | `0.12.0` | `MIT` | [Repo](https://github.com/m4b/scroll) (m4b <m4b.github.io@gmail.com>, Ted Mielczarek <ted@mielczare) |
| `scroll_derive` | `0.12.1` | `MIT` | [Repo](https://github.com/m4b/scroll) (m4b <m4b.github.io@gmail.com>, Ted Mielczarek <ted@mielczare) |
| `sec1` | `0.8.1` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/formats) (RustCrypto Developers) |
| `security-framework` | `3.7.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/kornelski/rust-security-framework) (Steven Fackler <sfackler@gmail.com>, Kornel <kornel@geekhood) |
| `security-framework-sys` | `2.17.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/kornelski/rust-security-framework) (Steven Fackler <sfackler@gmail.com>, Kornel <kornel@geekhood) |
| `semver` | `1.0.28` | `MIT OR Apache-2.0` | [Repo](https://github.com/dtolnay/semver) (David Tolnay <dtolnay@gmail.com>) |
| `serde` | `1.0.229` | `MIT OR Apache-2.0` | [Repo](https://github.com/serde-rs/serde) (Erick Tryzelaar <erick.tryzelaar@gmail.com>, David Tolnay <d) |
| `serde_core` | `1.0.229` | `MIT OR Apache-2.0` | [Repo](https://github.com/serde-rs/serde) (Erick Tryzelaar <erick.tryzelaar@gmail.com>, David Tolnay <d) |
| `serde_derive` | `1.0.229` | `MIT OR Apache-2.0` | [Repo](https://github.com/serde-rs/serde) (Erick Tryzelaar <erick.tryzelaar@gmail.com>, David Tolnay <d) |
| `serde_json` | `1.0.154` | `MIT OR Apache-2.0` | [Repo](https://github.com/serde-rs/json) |
| `serde_spanned` | `1.1.3` | `MIT OR Apache-2.0` | [Repo](https://github.com/toml-rs/toml) |
| `serde_urlencoded` | `0.7.1` | `MIT/Apache-2.0` | [Repo](https://github.com/nox/serde_urlencoded) (Anthony Ramine <n.oxyde@gmail.com>) |
| `serdect` | `0.4.3` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/formats) (RustCrypto Developers) |
| `sha1` | `0.11.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/RustCrypto/hashes) (RustCrypto Developers) |
| `sha1_smol` | `1.0.1` | `BSD-3-Clause` | [Repo](https://github.com/mitsuhiko/sha1-smol) (Armin Ronacher <armin.ronacher@active-4.com>) |
| `sha2` | `0.11.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/RustCrypto/hashes) (RustCrypto Developers) |
| `shlex` | `2.0.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/comex/rust-shlex) (comex <comexk@gmail.com>, Fenhl <fenhl@fenhl.net>, Adrian Ta) |
| `signature` | `3.0.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/traits) (RustCrypto Developers) |
| `simd_cesu8` | `1.2.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/seancroach/simd_cesu8) (Sean C. Roach <me@seancroach.dev>) |
| `simdutf8` | `0.1.5` | `MIT OR Apache-2.0` | [Repo](https://github.com/rusticstuff/simdutf8) (Hans Kratz <hans@appfour.com>) |
| `simple_asn1` | `0.6.4` | `ISC` | [Repo](https://github.com/acw/simple_asn1) (Adam Wick <awick@uhsure.com>) |
| `simpleicons-rs` | `16.37.0` | `MIT AND CC0-1.0` | [Repo](https://github.com/cscnk52/simpleicons-rs) |
| `siphasher` | `1.0.4` | `MIT OR Apache-2.0` | [Repo](https://github.com/jedisct1/rust-siphash) (Frank Denis <github@pureftpd.org>) |
| `slab` | `0.4.12` | `MIT` | [Repo](https://github.com/tokio-rs/slab) (Carl Lerche <me@carllerche.com>) |
| `smallvec` | `1.16.5` | `MIT OR Apache-2.0` | [Repo](https://github.com/servo/rust-smallvec) (The Servo Project Developers) |
| `smawk` | `0.3.3` | `MIT` | [Repo](https://github.com/mgeisler/smawk) (Martin Geisler <martin@geisler.net>) |
| `socket2` | `0.6.5` | `MIT OR Apache-2.0` | [Repo](https://github.com/rust-lang/socket2) (Alex Crichton <alex@alexcrichton.com>, Thomas de Zeeuw <thom) |
| `spki` | `0.8.1` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/formats) (RustCrypto Developers) |
| `sqlcipher-src` | `420.0.0+sqlcipher-4.20.0-sqlite-3.54.1-libtomcrypt-1.18.2` | `MIT AND BSD-3-Clause AND blessing AND WTFPL` | [Repo](https://github.com/LucaCappelletti94/sqlcipher-src) |
| `sqlite-wasm-rs` | `0.6.3` | `MIT` | [Repo](https://github.com/Spxg/sqlite-wasm-rs) (Spxg <noz@live.com>) |
| `stable_deref_trait` | `1.2.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/storyyeller/stable_deref_trait) (Robert Grosse <n210241048576@gmail.com>) |
| `static_assertions` | `1.1.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/nvzqz/static-assertions-rs) (Nikolai Vazquez) |
| `strsim` | `0.11.1` | `MIT` | [Repo](https://github.com/rapidfuzz/strsim-rs) (Danny Guo <danny@dannyguo.com>, maxbachmann <oss@maxbachmann) |
| `subtle` | `2.6.1` | `BSD-3-Clause` | [Repo](https://github.com/dalek-cryptography/subtle) (Isis Lovecruft <isis@patternsinthevoid.net>, Henry de Valenc) |
| `syn` | `2.0.119` | `MIT OR Apache-2.0` | [Repo](https://github.com/dtolnay/syn) (David Tolnay <dtolnay@gmail.com>) |
| `syn` | `3.0.10` | `MIT OR Apache-2.0` | [Repo](https://github.com/dtolnay/syn) (David Tolnay <dtolnay@gmail.com>) |
| `sync_wrapper` | `1.0.2` | `Apache-2.0` | [Repo](https://github.com/Actyx/sync_wrapper) (Actyx AG <developer@actyx.io>) |
| `synstructure` | `0.14.0` | `MIT` | [Repo](https://github.com/mystor/synstructure) (Nika Layzell <nika@thelayzells.com>) |
| `tempfile` | `3.27.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/Stebalien/tempfile) (Steven Allen <steven@stebalien.com>, The Rust Project Develo) |
| `textwrap` | `0.16.4` | `MIT` | [Repo](https://github.com/mgeisler/textwrap) |
| `thiserror` | `2.0.21` | `MIT OR Apache-2.0` | [Repo](https://github.com/dtolnay/thiserror) (David Tolnay <dtolnay@gmail.com>) |
| `thiserror-impl` | `2.0.21` | `MIT OR Apache-2.0` | [Repo](https://github.com/dtolnay/thiserror) (David Tolnay <dtolnay@gmail.com>) |
| `time` | `0.3.55` | `MIT OR Apache-2.0` | [Repo](https://github.com/time-rs/time) (Jacob Pratt <open-source@jhpratt.dev>, Time contributors) |
| `time-core` | `0.1.9` | `MIT OR Apache-2.0` | [Repo](https://github.com/time-rs/time) (Jacob Pratt <open-source@jhpratt.dev>, Time contributors) |
| `time-macros` | `0.2.32` | `MIT OR Apache-2.0` | [Repo](https://github.com/time-rs/time) (Jacob Pratt <open-source@jhpratt.dev>, Time contributors) |
| `tinystr` | `0.8.5` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `tinyvec` | `1.13.3` | `Zlib OR Apache-2.0 OR MIT` | [Repo](https://github.com/Lokathor/tinyvec) (Lokathor <zefria@gmail.com>) |
| `tokio` | `1.54.1` | `MIT` | [Repo](https://github.com/tokio-rs/tokio) (Tokio Contributors <team@tokio.rs>) |
| `tokio-macros` | `2.7.2` | `MIT` | [Repo](https://github.com/tokio-rs/tokio) (Tokio Contributors <team@tokio.rs>) |
| `tokio-rustls` | `0.26.6` | `MIT OR Apache-2.0` | [Repo](https://github.com/rustls/tokio-rustls) |
| `tokio-tungstenite` | `0.30.0` | `MIT` | [Repo](https://github.com/snapview/tokio-tungstenite) (Daniel Abramov <dabramov@snapview.de>, Alexey Galakhov <agal) |
| `toml` | `1.2.1+spec-1.1.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/toml-rs/toml) |
| `toml_datetime` | `1.1.3+spec-1.1.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/toml-rs/toml) |
| `toml_parser` | `1.1.5+spec-1.1.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/toml-rs/toml) |
| `toml_writer` | `1.2.0+spec-1.1.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/toml-rs/toml) |
| `tower` | `0.5.3` | `MIT` | [Repo](https://github.com/tower-rs/tower) (Tower Maintainers <team@tower-rs.com>) |
| `tower-http` | `0.6.11` | `MIT` | [Repo](https://github.com/tower-rs/tower-http) (Tower Maintainers <team@tower-rs.com>) |
| `tower-layer` | `0.3.3` | `MIT` | [Repo](https://github.com/tower-rs/tower) (Tower Maintainers <team@tower-rs.com>) |
| `tower-service` | `0.3.3` | `MIT` | [Repo](https://github.com/tower-rs/tower) (Tower Maintainers <team@tower-rs.com>) |
| `tracing` | `0.1.44` | `MIT` | [Repo](https://github.com/tokio-rs/tracing) (Eliza Weisman <eliza@buoyant.io>, Tokio Contributors <team@t) |
| `tracing-core` | `0.1.36` | `MIT` | [Repo](https://github.com/tokio-rs/tracing) (Tokio Contributors <team@tokio.rs>) |
| `try-lock` | `0.2.5` | `MIT` | [Repo](https://github.com/seanmonstar/try-lock) (Sean McArthur <sean@seanmonstar.com>) |
| `tungstenite` | `0.30.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/snapview/tungstenite-rs) (Alexey Galakhov, Daniel Abramov) |
| `typenum` | `1.20.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/paholg/typenum) |
| `uncased` | `0.9.10` | `MIT OR Apache-2.0` | [Repo](https://github.com/SergioBenitez/uncased) (Sergio Benitez <sb@sergio.bz>) |
| `unicode-ident` | `1.0.27` | `(MIT OR Apache-2.0) AND Unicode-3.0` | [Repo](https://github.com/dtolnay/unicode-ident) (David Tolnay <dtolnay@gmail.com>) |
| `unicode-width` | `0.2.3` | `MIT OR Apache-2.0` | [Repo](https://github.com/unicode-rs/unicode-width) (kwantam <kwantam@gmail.com>, Manish Goregaokar <manishsmail@) |
| `uniffi` | `0.32.2` | `MPL-2.0` | [Repo](https://github.com/mozilla/uniffi-rs) |
| `uniffi_bindgen` | `0.32.2` | `MPL-2.0` | [Repo](https://github.com/mozilla/uniffi-rs) |
| `uniffi_core` | `0.32.2` | `MPL-2.0` | [Repo](https://github.com/mozilla/uniffi-rs) |
| `uniffi_internal_macros` | `0.32.2` | `MPL-2.0` | [Repo](https://github.com/mozilla/uniffi-rs) |
| `uniffi_macros` | `0.32.2` | `MPL-2.0` | [Repo](https://github.com/mozilla/uniffi-rs) |
| `uniffi_meta` | `0.32.2` | `MPL-2.0` | [Repo](https://github.com/mozilla/uniffi-rs) |
| `uniffi_pipeline` | `0.32.2` | `MPL-2.0` | [Repo](https://github.com/mozilla/uniffi-rs) |
| `uniffi_udl` | `0.32.2` | `MPL-2.0` | [Repo](https://github.com/mozilla/uniffi-rs) |
| `untrusted` | `0.9.0` | `ISC` | [Repo](https://github.com/briansmith/untrusted) (Brian Smith <brian@briansmith.org>) |
| `url` | `2.5.8` | `MIT OR Apache-2.0` | [Repo](https://github.com/servo/rust-url) (The rust-url developers) |
| `utf8_iter` | `1.1.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/hsivonen/utf8_iter) (Henri Sivonen <hsivonen@hsivonen.fi>) |
| `utf_types` | `1.0.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/hsivonen/utf_types) (Henri Sivonen <hsivonen@hsivonen.fi>) |
| `uuid` | `1.29.2` | `Apache-2.0 OR MIT` | [Repo](https://github.com/uuid-rs/uuid) |
| `vcpkg` | `0.2.15` | `MIT/Apache-2.0` | [Repo](https://github.com/mcgoo/vcpkg-rs) (Jim McGrath <jimmc2@gmail.com>) |
| `version_check` | `0.9.5` | `MIT/Apache-2.0` | [Repo](https://github.com/SergioBenitez/version_check) (Sergio Benitez <sb@sergio.bz>) |
| `walkdir` | `2.5.0` | `Unlicense/MIT` | [Repo](https://github.com/BurntSushi/walkdir) (Andrew Gallant <jamslam@gmail.com>) |
| `want` | `0.3.2` | `MIT` | [Repo](https://github.com/seanmonstar/want) (Sean McArthur <sean@seanmonstar.com>) |
| `wasi` | `0.11.1+wasi-snapshot-preview1` | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | [Repo](https://github.com/bytecodealliance/wasi) (The Cranelift Project Developers) |
| `wasip2` | `1.0.4+wasi-0.2.12` | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | [Repo](https://github.com/bytecodealliance/wasi-rs) |
| `wasm-bindgen` | `0.2.130` | `MIT OR Apache-2.0` | [Repo](https://github.com/wasm-bindgen/wasm-bindgen) (The wasm-bindgen Developers) |
| `wasm-bindgen-futures` | `0.4.80` | `MIT OR Apache-2.0` | [Repo](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/futures) (The wasm-bindgen Developers) |
| `wasm-bindgen-macro` | `0.2.130` | `MIT OR Apache-2.0` | [Repo](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro) (The wasm-bindgen Developers) |
| `wasm-bindgen-macro-support` | `0.2.130` | `MIT OR Apache-2.0` | [Repo](https://github.com/wasm-bindgen/wasm-bindgen/tree/main/crates/macro-support) (The wasm-bindgen Developers) |
| `wasm-bindgen-shared` | `0.2.130` | `MIT OR Apache-2.0` | [Repo](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/shared) (The wasm-bindgen Developers) |
| `web-sys` | `0.3.107` | `MIT OR Apache-2.0` | [Repo](https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/web-sys) (The wasm-bindgen Developers) |
| `web-time` | `1.1.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/daxpedda/web-time) |
| `webpki-root-certs` | `1.0.11` | `CDLA-Permissive-2.0` | [Repo](https://github.com/rustls/webpki-roots) |
| `webpki-roots` | `0.26.11` | `CDLA-Permissive-2.0` | [Repo](https://github.com/rustls/webpki-roots) |
| `webpki-roots` | `1.0.11` | `CDLA-Permissive-2.0` | [Repo](https://github.com/rustls/webpki-roots) |
| `weedle2` | `5.0.0` | `MIT` | [Repo](https://github.com/mozilla/uniffi-rs) (Sharad Chand <sharad.d.chand@gmail.com>, Jan-Erik Rediger <j) |
| `winapi-util` | `0.1.11` | `Unlicense OR MIT` | [Repo](https://github.com/BurntSushi/winapi-util) (Andrew Gallant <jamslam@gmail.com>) |
| `windows-core` | `0.62.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) |
| `windows-implement` | `0.60.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) |
| `windows-interface` | `0.59.3` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) |
| `windows-link` | `0.2.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) |
| `windows-result` | `0.4.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) |
| `windows-strings` | `0.5.1` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) |
| `windows-sys` | `0.52.0` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) (Microsoft) |
| `windows-sys` | `0.61.2` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) |
| `windows-targets` | `0.52.6` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) (Microsoft) |
| `windows_aarch64_gnullvm` | `0.52.6` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) (Microsoft) |
| `windows_aarch64_msvc` | `0.52.6` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) (Microsoft) |
| `windows_i686_gnu` | `0.52.6` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) (Microsoft) |
| `windows_i686_gnullvm` | `0.52.6` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) (Microsoft) |
| `windows_i686_msvc` | `0.52.6` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) (Microsoft) |
| `windows_x86_64_gnu` | `0.52.6` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) (Microsoft) |
| `windows_x86_64_gnullvm` | `0.52.6` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) (Microsoft) |
| `windows_x86_64_msvc` | `0.52.6` | `MIT OR Apache-2.0` | [Repo](https://github.com/microsoft/windows-rs) (Microsoft) |
| `winnow` | `1.0.5` | `MIT` | [Repo](https://github.com/winnow-rs/winnow) |
| `wit-bindgen` | `0.57.1` | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | [Repo](https://github.com/bytecodealliance/wit-bindgen) (Alex Crichton <alex@alexcrichton.com>) |
| `wnaf` | `0.14.1` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/elliptic-curves) (RustCrypto Developers) |
| `writeable` | `0.6.4` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `yoke` | `0.8.4` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (Manish Goregaokar <manishsmail@gmail.com>) |
| `yoke-derive` | `0.8.4` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (Manish Goregaokar <manishsmail@gmail.com>) |
| `zerocopy` | `0.8.63` | `BSD-2-Clause OR Apache-2.0 OR MIT` | [Repo](https://github.com/google/zerocopy) |
| `zerocopy-derive` | `0.8.63` | `BSD-2-Clause OR Apache-2.0 OR MIT` | [Repo](https://github.com/google/zerocopy) |
| `zerofrom` | `0.1.8` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `zerofrom-derive` | `0.1.8` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (Manish Goregaokar <manishsmail@gmail.com>) |
| `zeroize` | `1.9.1` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/utils) (The RustCrypto Project Developers) |
| `zeroize_derive` | `1.5.0` | `Apache-2.0 OR MIT` | [Repo](https://github.com/RustCrypto/utils) (The RustCrypto Project Developers) |
| `zerotrie` | `0.2.5` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `zerovec` | `0.11.9` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (The ICU4X Project Developers) |
| `zerovec-derive` | `0.11.7` | `Unicode-3.0` | [Repo](https://github.com/unicode-org/icu4x) (Manish Goregaokar <manishsmail@gmail.com>) |
| `zmij` | `1.0.23` | `MIT` | [Repo](https://github.com/dtolnay/zmij) (David Tolnay <dtolnay@gmail.com>) |

</details>
