# mini-rust

A tiny Rust crate used as a fixture for quorum's context feature tests.
It provides a couple of small, self-contained modules so the extractor has
something realistic to chew on without pulling in real dependencies.

## Usage

Add the crate to your workspace and call the helpers directly:

```rust
use mini_rust::token::{verify_token, VerifyOpts};

let claims = verify_token("abc.def.ghi", VerifyOpts { allow_expired: false });
```

The `token` module handles JWT verification and the `util` module exposes
small generic helpers such as `clamp`.

## Design

The crate is intentionally minimal. Each module is a single file with a
single public entry point so tests can grep for documentation and signatures
without worrying about re-exports. The public surface is stable: changing
names here will break fixture-dependent tests in the parent repository.
