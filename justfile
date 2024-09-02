default := "validate"

install-tools:
    cargo install cargo-bundle

bundle:
    cargo bundle --release

validate:
    cargo build
    cargo test
    cargo fmt --all -- --check
    cargo clippy -- -D warnings