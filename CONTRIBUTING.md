# Contributing to STViewer

Thanks for helping! Bug reports, format samples and pull requests are welcome.

**Never attach real mail to an issue or commit it to the repo.** Fixtures must be
synthetic or come from a corpus with a clear open license. Use reserved example domains
(`example.com`, `*.example`, `*.test`) for every name and address.

## Building

You need Rust 1.92+, plus Node.js 24 and the Tauri system dependencies for the desktop
app. The [README](README.md#build-from-source) lists them for each OS.

```sh
cargo build -p stv                    # CLI
cd app && npm ci && npm run tauri dev # desktop app with hot reload
```

`app/src-tauri` is a workspace member, and `tauri::generate_context!` needs the frontend
output directory `app/dist` to exist at compile time. Run `npm run build` in `app/` once
before a workspace-wide `cargo build` / `cargo test` / `cargo clippy`. An empty
`app/dist/index.html` also works if you only touch Rust code.

For quicker local optimised builds, use `--profile release-fast` (no LTO, incremental).
Keep `--release` for shipped binaries.

## Checks (what CI runs)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd app && npm ci && npm run format:check && npm run build   # Prettier, svelte-check, vite build
cargo deny check                       # licenses, advisories, sources (cargo install cargo-deny)
```

CI runs these on Linux, Windows and macOS. Formatting follows `rustfmt.toml`
(`max_width = 120`). Fix clippy findings instead of silencing them. If an `#[allow]` is
really needed, add a comment that says why.

The macOS-only preview code (ImageIO, Quick Look) can be type-checked elsewhere with
`rustup target add aarch64-apple-darwin` and
`cargo check -p stviewer-app --target aarch64-apple-darwin`; the build stops at
`objc2-exception-helper`, which needs a C compiler for that target. The desktop exe also
runs as its own PDF render worker when started with the internal `--stv-pdf-worker`
argument; don't use it directly.

## Tests and fixtures

- Committed fixtures live in `testdata/`. [testdata/README.md](testdata/README.md) lists
  where each directory comes from and how it is regenerated:
  - the PST writer in `crates/pst/tests/common` (`cargo test -p stv-pst --test synthetic
    -- --ignored regenerate_committed_fixtures`);
  - the mbox generator in `crates/mbox/tests/common` (runs with the tests);
  - `node app/scripts/make-fixtures.mjs` for the app's `testdata/app/demo.mbox`.
- `testdata/large/` is optional and git-ignored. It holds multi-GB public archives (Enron
  EDRM, Aspose and java-libpst samples) used for real-file tests and benchmarks.
  Download instructions are in [testdata/pst/README.md](testdata/pst/README.md). Tests
  that need these files print a note and pass when the files are missing, so a fresh
  clone and CI run the whole suite.
- Tests must pass on every OS: build paths with `Path::join`, with no drive letters or
  backslashes.
- Prefer tests of observable behaviour: parsing results, boundaries, malformed input
  that must not panic. Avoid tests that pin wording or internals. Add a regression test
  with each bug fix.
- Performance changes need numbers. Use `stv bench <archive>` or
  `cargo test --profile release-fast -p stv-index --test perf` and give before/after
  figures in the PR.

## Debugging format issues

Each reader has a dump example:

```sh
cargo run -p stv-pst  --example pst-dump  -- <file.pst>
cargo run -p stv-msg  --example msg-dump  -- [--props] <file-or-dir>
cargo run -p stv-mbox --example mbox-dump -- <path>
```

If a file from a real mailbox doesn't parse, please don't share it. Open an issue with
the dump output (with personal data redacted) or a synthetic file that reproduces the
problem.

## Pull requests

- Keep each PR focused, and add an entry under *Unreleased* in
  [CHANGELOG.md](CHANGELOG.md) for user-visible changes.
- Contributions are dual-licensed `MIT OR Apache-2.0` as described in the
  [README](README.md#license).
