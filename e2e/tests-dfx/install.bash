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

@test "canister install --upgrade-unchanged upgrades even if the .wasm did not change" {
    dfx_start
    dfx canister create --all
    dfx build

    assert_command dfx canister install --all

    assert_command dfx canister install --all --mode upgrade
    assert_match "Module hash.*is already installed"

    assert_command dfx canister install --all --mode upgrade --upgrade-unchanged
    assert_not_match "Module hash.*is already installed"
}

@test "install fails if no argument is provided" {
    [ "$USE_IC_REF" ] && skip "skipped for ic-ref"

    dfx_start
    assert_command_fail dfx canister install
    assert_match "required arguments were not provided"
    assert_match "--all"
}

@test "install succeeds when --all is provided" {
    dfx_start
    dfx canister create --all
    dfx build

    assert_command dfx canister install --all

    assert_match "Installing code for canister e2e_project"
}

@test "install succeeds with network name" {
    dfx_start
    dfx canister create --all
    dfx build

    assert_command dfx canister --network local install --all

    assert_match "Installing code for canister e2e_project"
}

@test "install fails with network name that is not in dfx.json" {
    dfx_start
    dfx canister create --all
    dfx build

    assert_command_fail dfx canister --network nosuch install --all

    assert_match "ComputeNetworkNotFound.*nosuch"
}
