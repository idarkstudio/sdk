#!/usr/bin/env bats

load ../utils/_

setup() {
  standard_setup

  dfx_new
}

teardown() {
  dfx_stop

  standard_teardown
}

@test "build without cargo-audit installed cannot check for vulnerabilities" {
  assert_command rustup default stable
  assert_command rustup target add wasm32-unknown-unknown
  install_asset vulnerable_rust_deps
  dfx_start
  dfx canister create --all
  assert_command dfx build
  assert_match "Cannot check for vulnerabilities in rust canisters because cargo-audit is not installed."
}

@test "build with vulnerabilities in rust dependencies emits a warning" {
  assert_command rustup default stable
  assert_command rustup target add wasm32-unknown-unknown
  assert_command cargo install cargo-audit --locked
  assert_command cargo audit --version
  install_asset vulnerable_rust_deps
  dfx_start
  dfx canister create --all
  assert_command dfx build
  assert_match "Audit found vulnerabilities"
  jq '.canisters.hello.skip_cargo_audit=true' dfx.json | sponge dfx.json
  assert_command dfx build
  assert_not_match "Audit found vulnerabilities"
}
