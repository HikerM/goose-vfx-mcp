# lumina-sdk

The bindings layer for Lumina. It houses the shared types used for both ACP and
SDK access, and exposes a cross-language version of the Lumina API.

With `--features uniffi` the crate compiles to native bindings for Python and
Kotlin (namespace `lumina` / `io.github.hikerm.lumina`). The UniFFI surface currently lets
callers construct declarative providers from JSON and stream provider
completions.

```bash
just python   # build bindings + run examples/uniffi/provider.py
just kotlin   # build the Maven artifact + run examples/uniffi/kotlin
```

## Python package

The PyPI package is published as `lumina-sdk` and imports as `lumina`.
Build a local wheel from the repository root with:

```bash
just --justfile crates/lumina-sdk/justfile python-wheel
```

This regenerates the UniFFI Python bindings, copies the release native library
into the package, and writes the wheel to `crates/lumina-sdk/python/dist/`.

## Maven package

The Maven Central artifact is published as `io.github.hikerm:gdk` and uses
the Rust crate version from `crates/lumina-sdk/Cargo.toml`.

```bash
just --justfile crates/lumina-sdk/justfile maven-package
```

This regenerates the UniFFI Kotlin bindings and packages them with the native
library in a JVM jar. CI builds the native libraries for supported platforms and
can optionally publish the combined artifact to Maven Central.
