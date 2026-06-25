#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Events, Ledger as _},
    token, Address, Env, Symbol, TryFromVal, Vec,
};

use crate::{
    errors::VaultError,
    storage::LeaderboardEntry,
    vault::{
        VaultContract, VaultContractClient, BOOST_BPS_BASE, CONTRACT_VERSION,
        STELLAR_LEDGERS_PER_YEAR,
    },
};

// ── helpers ──────────────────────────────────────────────────────────────────

fn create_token<'a>(
    env: &Env,
    admin: &Address,
) -> (Address, token::Client<'a>, token::StellarAssetClient<'a>) {
    let address = env.register_stellar_asset_contract(admin.clone());
    let client = token::Client::new(env, &address);
    let admin_client = token::StellarAssetClient::new(env, &address);
    (address, client, admin_client)
}

fn set_ledger(env: &Env, sequence: u32) {
    env.ledger().with_mut(|li| {
        li.sequence_number = sequence;
    });
}

fn boost_schedule(env: &Env, tiers: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut schedule = Vec::new(env);
    for tier in tiers {
        schedule.push_back(*tier);
    }
    schedule
}

fn topic_matches(env: &Env, topics: &Vec<soroban_sdk::Val>, name: &str) -> bool {
    match topics.get(0) {
        Some(val) => Symbol::try_from_val(env, &val)
            .map(|topic| topic == Symbol::new(env, name))
            .unwrap_or(false),
        None => false,
    }
}

struct VaultFixture<'a> {
    env: Env,
    vault: VaultContractClient<'a>,
    token: token::Client<'a>,
    token_admin: token::StellarAssetClient<'a>,
    admin: Address,
    alice: Address,
    bob: Address,
}

impl<'a> VaultFixture<'a> {
    fn new() -> Self {
        Self::with_mock_auths(true)
    }

    fn with_mock_auths(mock_auths: bool) -> Self {
        Self::build(mock_auths, None, None)
    }

    /// Build a fixture with explicit stake/reward token decimals.
    fn with_decimals(stake_decimals: u32, reward_decimals: u32) -> Self {
        Self::build(true, Some(stake_decimals), Some(reward_decimals))
    }

    fn build(
        mock_auths: bool,
        stake_decimals: Option<u32>,
        reward_decimals: Option<u32>,
    ) -> Self {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|li| {
            li.min_temp_entry_ttl = 10_000_000;
            li.min_persistent_entry_ttl = 10_000_000;
            li.max_entry_ttl = 10_000_000;
        });

        let admin = Address::generate(&env);
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);

        let (token_addr, token, token_admin) = create_token(&env, &admin);

        let vault_id = env.register_contract(None, VaultContract);
        let vault = VaultContractClient::new(&env, &vault_id);

        vault.initialize(&admin, &token_addr, &stake_decimals, &reward_decimals);

        // Mint starting balances
        token_admin.mint(&alice, &20_000_000);
        token_admin.mint(&bob, &20_000_000);

        if !mock_auths {
            env.set_auths(&[]);
        }

        VaultFixture {
            env,
            vault,
            token,
            token_admin,
            admin,
            alice,
            bob,
        }
    }
}

// ── initialization ────────────────────────────────────────────────────────────

#[test]
fn test_initialize_sets_state() {
    let f = VaultFixture::new();
    let (total_shares, total_deposited) = f.vault.vault_state();
    assert_eq!(total_shares, 0);
    assert_eq!(total_deposited, 0);
}

#[test]
fn test_double_initialize_fails() {
    let f = VaultFixture::new();
    let token_addr: soroban_sdk::Address = f
        .env
        .register_stellar_asset_contract(Address::generate(&f.env));
    let result = f.vault.try_initialize(&f.admin, &token_addr, &None, &None);
    assert_eq!(result, Err(Ok(VaultError::AlreadyInitialized)));
}

#[test]
fn test_get_admin_returns_initialized_admin() {
    let f = VaultFixture::new();
    assert_eq!(f.vault.get_admin(), f.admin);
}

#[test]
fn test_get_version_returns_contract_version() {
    let f = VaultFixture::new();
    assert_eq!(f.vault.get_version(), soroban_sdk::String::from_str(&f.env, CONTRACT_VERSION));
}

// ── deposit ───────────────────────────────────────────────────────────────────

#[test]
fn test_first_deposit_mints_1to1_shares() {
    let f = VaultFixture::new();
    let shares = f.vault.deposit(&f.alice, &500_000);
    assert_eq!(shares, 500_000);
    assert_eq!(f.vault.shares_of(&f.alice), 500_000);

    let (total_shares, total_deposited) = f.vault.vault_state();
    assert_eq!(total_shares, 500_000);
    assert_eq!(total_deposited, 500_000);
}

#[test]
fn test_deposit_zero_fails() {
    let f = VaultFixture::new();
    let result = f.vault.try_deposit(&f.alice, &0);
    assert_eq!(result, Err(Ok(VaultError::ZeroAmount)));
}

#[test]
fn test_deposit_negative_fails() {
    let f = VaultFixture::new();
    let result = f.vault.try_deposit(&f.alice, &-100);
    assert_eq!(result, Err(Ok(VaultError::ZeroAmount)));
}

#[test]
fn test_two_depositors_get_proportional_shares() {
    let f = VaultFixture::new();

    let alice_shares = f.vault.deposit(&f.alice, &400_000);
    let bob_shares = f.vault.deposit(&f.bob, &100_000);

    assert_eq!(alice_shares, 400_000);
    assert_eq!(bob_shares, 100_000);

    let (total_shares, _) = f.vault.vault_state();
    assert_eq!(total_shares, 500_000);
}

// ── withdraw ──────────────────────────────────────────────────────────────────

#[test]
fn test_withdraw_returns_correct_amount() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &600_000);

    let token_before = f.token.balance(&f.alice);
    let amount_back = f.vault.withdraw(&f.alice, &300_000);

    assert_eq!(amount_back, 300_000);
    assert_eq!(f.vault.shares_of(&f.alice), 300_000);
    assert_eq!(f.token.balance(&f.alice), token_before + 300_000);
}

#[test]
fn test_withdraw_more_than_owned_fails() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &100_000);

    let result = f.vault.try_withdraw(&f.alice, &200_000);
    assert_eq!(result, Err(Ok(VaultError::InsufficientShares)));
}

#[test]
fn test_withdraw_zero_fails() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &100_000);

    let result = f.vault.try_withdraw(&f.alice, &0);
    assert_eq!(result, Err(Ok(VaultError::ZeroAmount)));
}

#[test]
fn test_full_withdraw_clears_shares() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &400_000);
    f.vault.withdraw(&f.alice, &400_000);

    assert_eq!(f.vault.shares_of(&f.alice), 0);
    let (total_shares, total_deposited) = f.vault.vault_state();
    assert_eq!(total_shares, 0);
    assert_eq!(total_deposited, 0);
}

// ── preview_redeem ────────────────────────────────────────────────────────────

#[test]
fn test_preview_redeem_matches_actual_withdraw() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &500_000);

    let preview = f.vault.preview_redeem(&250_000);
    let actual = f.vault.withdraw(&f.alice, &250_000);

    assert_eq!(preview, actual);
}

// ── pause / unpause ───────────────────────────────────────────────────────────

#[test]
fn test_pause_blocks_deposit() {
    let f = VaultFixture::new();
    f.vault.pause();

    let result = f.vault.try_deposit(&f.alice, &100_000);
    assert_eq!(result, Err(Ok(VaultError::VaultPaused)));
}

#[test]
fn test_pause_blocks_withdraw() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &100_000);
    f.vault.pause();

    let result = f.vault.try_withdraw(&f.alice, &100_000);
    assert_eq!(result, Err(Ok(VaultError::VaultPaused)));
}

#[test]
fn test_unpause_restores_operations() {
    let f = VaultFixture::new();
    f.vault.pause();
    f.vault.unpause();

    let shares = f.vault.deposit(&f.alice, &100_000);
    assert_eq!(shares, 100_000);
}

#[test]
fn test_is_paused_defaults_to_false() {
    let f = VaultFixture::new();
    assert!(!f.vault.is_paused());
}

#[test]
fn test_is_paused_returns_true_after_pause() {
    let f = VaultFixture::new();
    f.vault.pause();
    assert!(f.vault.is_paused());
}

// ── admin transfer ────────────────────────────────────────────────────────────

#[test]
fn test_transfer_admin() {
    let f = VaultFixture::new();
    f.vault.transfer_admin(&f.bob);
    // Bob is now admin — he should be able to pause
    f.vault.pause();
}

// ── yield accrual ────────────────────────────────────────────────────────────

#[test]
fn test_add_yield_increases_share_price() {
    let f = VaultFixture::new();

    // Alice deposits 500k -> 500k shares
    f.vault.deposit(&f.alice, &500_000);

    // Mint tokens to admin so they can add yield
    f.token_admin.mint(&f.admin, &100_000);

    // Preview before yield: 250k shares -> 250k tokens
    let preview_before = f.vault.preview_redeem(&250_000);
    assert_eq!(preview_before, 250_000);

    // Admin adds 100k yield
    f.vault.add_yield(&f.admin, &100_000);

    // Vault total_deposited should increase
    let (_total_shares, total_deposited) = f.vault.vault_state();
    assert_eq!(total_deposited, 600_000);

    // Preview after yield: 250k shares -> 300k tokens
    let preview_after = f.vault.preview_redeem(&250_000);
    assert_eq!(preview_after, 300_000);
}

#[test]
fn test_add_yield_requires_admin_auth() {
    let f = VaultFixture::new();
    f.token_admin.mint(&f.admin, &10_000);

    f.vault.add_yield(&f.admin, &10_000);
    assert_eq!(f.env.auths()[0].0, f.admin);
}

#[test]
fn test_add_yield_paused_blocks() {
    let f = VaultFixture::new();
    f.token_admin.mint(&f.admin, &50_000);
    f.vault.pause();

    let result = f.vault.try_add_yield(&f.admin, &50_000);
    assert_eq!(result, Err(Ok(VaultError::VaultPaused)));
}

#[test]
fn test_add_yield_zero_fails() {
    let f = VaultFixture::new();
    f.token_admin.mint(&f.admin, &10_000);

    let result = f.vault.try_add_yield(&f.admin, &0);
    assert_eq!(result, Err(Ok(VaultError::ZeroAmount)));
}

// ── withdrawal limit (Issue #8) ──────────────────────────────────────────────

#[test]
fn test_set_withdrawal_limit() {
    let f = VaultFixture::new();
    f.vault.set_withdrawal_limit(&100_000);
    assert_eq!(f.vault.get_withdrawal_limit(), 100_000);
}

#[test]
fn test_withdrawal_limit_blocks_large_withdrawal() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &500_000);
    f.vault.set_withdrawal_limit(&100_000);

    let result = f.vault.try_withdraw(&f.alice, &200_000);
    assert_eq!(result, Err(Ok(VaultError::WithdrawalLimitExceeded)));
}

#[test]
fn test_withdrawal_limit_allows_within_limit() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &500_000);
    f.vault.set_withdrawal_limit(&100_000);

    let amount = f.vault.withdraw(&f.alice, &100_000);
    assert_eq!(amount, 100_000);
    assert_eq!(f.vault.shares_of(&f.alice), 400_000);
}

#[test]
fn test_withdrawal_limit_exact_boundary() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &500_000);
    f.vault.set_withdrawal_limit(&100_000);

    // Exactly at limit should work
    let amount = f.vault.withdraw(&f.alice, &100_000);
    assert_eq!(amount, 100_000);
}

#[test]
fn test_withdrawal_limit_one_over_fails() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &500_000);
    f.vault.set_withdrawal_limit(&100_000);

    // One over limit should fail
    let result = f.vault.try_withdraw(&f.alice, &100_001);
    assert_eq!(result, Err(Ok(VaultError::WithdrawalLimitExceeded)));
}

#[test]
fn test_admin_updates_withdrawal_limit() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &500_000);

    // Set initial limit
    f.vault.set_withdrawal_limit(&50_000);
    assert_eq!(f.vault.get_withdrawal_limit(), 50_000);

    // 60k fails with old limit
    let result = f.vault.try_withdraw(&f.alice, &60_000);
    assert_eq!(result, Err(Ok(VaultError::WithdrawalLimitExceeded)));

    // Admin raises limit
    f.vault.set_withdrawal_limit(&100_000);
    assert_eq!(f.vault.get_withdrawal_limit(), 100_000);

    // 60k now passes
    let amount = f.vault.withdraw(&f.alice, &60_000);
    assert_eq!(amount, 60_000);
}

#[test]
fn test_set_withdrawal_limit_zero_fails() {
    let f = VaultFixture::new();
    let result = f.vault.try_set_withdrawal_limit(&0);
    assert_eq!(result, Err(Ok(VaultError::ZeroAmount)));
}

#[test]
fn test_set_withdrawal_limit_negative_fails() {
    let f = VaultFixture::new();
    let result = f.vault.try_set_withdrawal_limit(&-100);
    assert_eq!(result, Err(Ok(VaultError::ZeroAmount)));
}

#[test]
fn test_set_withdrawal_limit_requires_admin_auth() {
    let f = VaultFixture::new();
    f.vault.set_withdrawal_limit(&100_000);
    assert_eq!(f.env.auths()[0].0, f.admin);
}

#[test]
fn test_no_withdrawal_limit_by_default() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &500_000);

    // No limit set, should be 0 (no restriction)
    assert_eq!(f.vault.get_withdrawal_limit(), 0);

    // Should be able to withdraw everything
    let amount = f.vault.withdraw(&f.alice, &500_000);
    assert_eq!(amount, 500_000);
}

// ── event emission (Issue #7) ─────────────────────────────────────────────────

#[test]
fn test_deposit_emits_event() {
    let f = VaultFixture::new();

    f.vault.deposit(&f.alice, &100_000);

    let events = f.env.events().all();
    let deposit_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "deposit"))
        .collect();

    assert_eq!(deposit_events.len(), 1);
    let event = &deposit_events[0];
    assert_eq!(
        Address::try_from_val(&f.env, &event.1.get(1).unwrap()).unwrap(),
        f.alice
    );
}

#[test]
fn test_withdraw_emits_event() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &100_000);

    f.vault.withdraw(&f.alice, &50_000);

    let events = f.env.events().all();
    let withdraw_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "withdraw"))
        .collect();

    assert_eq!(withdraw_events.len(), 1);
    let event = &withdraw_events[0];
    assert_eq!(
        Address::try_from_val(&f.env, &event.1.get(1).unwrap()).unwrap(),
        f.alice
    );
}

#[test]
fn test_pause_emits_event() {
    let f = VaultFixture::new();

    f.vault.pause();

    let events = f.env.events().all();
    let paused_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "paused"))
        .collect();

    assert_eq!(paused_events.len(), 1);
}

#[test]
fn test_unpause_emits_event() {
    let f = VaultFixture::new();
    f.vault.pause();

    f.vault.unpause();

    let events = f.env.events().all();
    let unpaused_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "unpaused"))
        .collect();

    assert_eq!(unpaused_events.len(), 1);
}

#[test]
fn test_transfer_admin_emits_event() {
    let f = VaultFixture::new();

    f.vault.transfer_admin(&f.bob);

    let events = f.env.events().all();
    let admin_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "admin_set"))
        .collect();

    assert_eq!(admin_events.len(), 1);
    let event = &admin_events[0];
    assert_eq!(
        Address::try_from_val(&f.env, &event.1.get(1).unwrap()).unwrap(),
        f.admin
    );
}

#[test]
fn test_withdrawal_limit_update_emits_event() {
    let f = VaultFixture::new();

    f.vault.set_withdrawal_limit(&100_000);

    let events = f.env.events().all();
    let limit_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "wd_limit"))
        .collect();

    assert_eq!(limit_events.len(), 1);
}

#[test]
fn test_yield_added_emits_event() {
    let f = VaultFixture::new();
    f.token_admin.mint(&f.admin, &50_000);

    f.vault.add_yield(&f.admin, &50_000);

    let events = f.env.events().all();
    let yield_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "yield_add"))
        .collect();

    assert_eq!(yield_events.len(), 1);
}

// ── error handling edge cases (Issue #9) ─────────────────────────────────────

#[test]
fn test_deposit_negative_amount_fails() {
    let f = VaultFixture::new();
    let result = f.vault.try_deposit(&f.alice, &-500);
    assert_eq!(result, Err(Ok(VaultError::ZeroAmount)));
}

#[test]
fn test_withdraw_negative_shares_fails() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &100_000);

    let result = f.vault.try_withdraw(&f.alice, &-500);
    assert_eq!(result, Err(Ok(VaultError::ZeroAmount)));
}

#[test]
fn test_transfer_admin_requires_admin_auth() {
    let f = VaultFixture::new();
    f.vault.transfer_admin(&f.bob);
    assert_eq!(f.env.auths()[0].0, f.admin);
}

#[test]
fn test_pause_requires_admin_auth() {
    let f = VaultFixture::new();
    f.vault.pause();
    assert_eq!(f.env.auths()[0].0, f.admin);
}

#[test]
fn test_unpause_requires_admin_auth() {
    let f = VaultFixture::new();
    f.vault.unpause();
    assert_eq!(f.env.auths()[0].0, f.admin);
}

#[test]
fn test_get_withdrawal_limit_before_init_fails() {
    let env = Env::default();
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    let result = vault.try_get_withdrawal_limit();
    assert_eq!(result, Err(Ok(VaultError::NotInitialized)));
}

// ── lock-up period and early-unstake penalty tests ───────────────────────────

#[test]
fn test_set_lock_period_requires_admin_auth() {
    let f = VaultFixture::new();
    f.vault.set_lock_period(&100);
    assert_eq!(f.env.auths()[0].0, f.admin);
}

#[test]
fn test_set_early_exit_penalty_bps_requires_admin_auth() {
    let f = VaultFixture::new();
    f.vault.set_early_exit_penalty_bps(&500);
    assert_eq!(f.env.auths()[0].0, f.admin);
}

#[test]
fn test_set_early_exit_penalty_bps_exceeds_max_fails() {
    let f = VaultFixture::new();
    // 2001 BPS should fail
    let result = f.vault.try_set_early_exit_penalty_bps(&2001);
    assert_eq!(result, Err(Ok(VaultError::InvalidPenaltyBps)));
}

#[test]
fn test_lock_config_query() {
    let f = VaultFixture::new();
    // Default config
    let (lock_period, penalty_bps) = f.vault.get_lock_config();
    assert_eq!(lock_period, 0);
    assert_eq!(penalty_bps, 0);

    // Set new config
    f.vault.set_lock_period(&100);
    f.vault.set_early_exit_penalty_bps(&1500);

    let (lock_period, penalty_bps) = f.vault.get_lock_config();
    assert_eq!(lock_period, 100);
    assert_eq!(penalty_bps, 1500);
}

// ── unstake fee (separate from withdrawal fee) ────────────────────────────────

#[test]
fn test_set_and_get_unstake_fee_bps() {
    let f = VaultFixture::new();
    assert_eq!(f.vault.get_unstake_fee_bps(), 0);

    f.vault.set_unstake_fee_bps(&f.admin, &250);
    assert_eq!(f.vault.get_unstake_fee_bps(), 250);
}

#[test]
fn test_set_unstake_fee_bps_requires_admin_auth() {
    let f = VaultFixture::new();
    f.vault.set_unstake_fee_bps(&f.admin, &100);
    assert_eq!(f.env.auths()[0].0, f.admin);
}

#[test]
fn test_set_unstake_fee_bps_allows_max() {
    let f = VaultFixture::new();
    f.vault.set_unstake_fee_bps(&f.admin, &500);
    assert_eq!(f.vault.get_unstake_fee_bps(), 500);
}

#[test]
fn test_set_unstake_fee_bps_too_high_rejected() {
    let f = VaultFixture::new();
    let result = f.vault.try_set_unstake_fee_bps(&f.admin, &501);
    assert_eq!(result, Err(Ok(VaultError::UnstakeFeeTooHigh)));
}

#[test]
fn test_unstake_with_zero_fee_returns_full_principal() {
    let f = VaultFixture::new();
    f.vault.deposit(&f.alice, &600_000);

    let token_before = f.token.balance(&f.alice);
    let amount_back = f.vault.withdraw(&f.alice, &300_000);

    assert_eq!(amount_back, 300_000);
    assert_eq!(f.token.balance(&f.alice), token_before + 300_000);
    // No fee configured, so nothing is routed to the treasury.
    assert_eq!(f.vault.get_reward_pool_balance(), 0);
}

#[test]
fn test_unstake_deducts_fee_and_credits_treasury() {
    let f = VaultFixture::new();
    f.vault.set_unstake_fee_bps(&f.admin, &500); // 5%
    f.vault.deposit(&f.alice, &600_000);

    let token_before = f.token.balance(&f.alice);
    let amount_back = f.vault.withdraw(&f.alice, &300_000);

    // 5% of 300_000 = 15_000 fee; 285_000 returned to the user.
    assert_eq!(amount_back, 285_000);
    assert_eq!(f.token.balance(&f.alice), token_before + 285_000);
    // Fee is routed to the reward pool treasury, not burned.
    assert_eq!(f.vault.get_reward_pool_balance(), 15_000);
}

#[test]
fn test_unstake_fee_applies_after_lock_penalty() {
    let f = VaultFixture::new();
    f.vault.set_lock_period(&100);
    f.vault.set_early_exit_penalty_bps(&1000); // 10%
    f.vault.set_unstake_fee_bps(&f.admin, &500); // 5%

    set_ledger(&f.env, 1);
    f.vault.deposit(&f.alice, &1_000_000);

    let token_before = f.token.balance(&f.alice);
    set_ledger(&f.env, 50); // still within the lock-up window
    let amount_back = f.vault.withdraw(&f.alice, &1_000_000);

    // Penalty first: 10% of 1_000_000 = 100_000 -> 900_000 after penalty.
    // Fee on the remainder: 5% of 900_000 = 45_000 -> 855_000 returned.
    assert_eq!(amount_back, 855_000);
    assert_eq!(f.token.balance(&f.alice), token_before + 855_000);
    assert_eq!(f.vault.get_reward_pool_balance(), 45_000);
}

// ── governance vote weight snapshots (Issue #31) ─────────────────────────────

#[test]
fn test_vote_weight_tracks_stake_history() {
    let f = VaultFixture::new();

    assert_eq!(f.vault.vote_weight_at(&f.alice, &0), 0);

    set_ledger(&f.env, 1);
    f.vault.stake(&f.alice, &500_000);
    assert_eq!(f.vault.current_vote_weight(&f.alice), 500_000);
    assert_eq!(f.vault.total_vote_weight(), 500_000);
    assert_eq!(f.vault.vote_weight_at(&f.alice, &1), 500_000);

    set_ledger(&f.env, 2);
    f.vault.unstake(&f.alice, &200_000);

    assert_eq!(f.vault.current_vote_weight(&f.alice), 300_000);
    assert_eq!(f.vault.total_vote_weight(), 300_000);
    assert_eq!(f.vault.vote_weight_at(&f.alice, &1), 500_000);
    assert_eq!(f.vault.vote_weight_at(&f.alice, &2), 300_000);
}

#[test]
fn test_vote_weight_history_is_capped_at_100_snapshots() {
    let f = VaultFixture::new();

    for ledger in 1..=105 {
        set_ledger(&f.env, ledger);
        f.vault.stake(&f.alice, &1);
    }

    assert_eq!(f.vault.current_vote_weight(&f.alice), 105);
    assert_eq!(f.vault.vote_weight_at(&f.alice, &1), 0);
    assert_eq!(f.vault.vote_weight_at(&f.alice, &5), 0);
    assert_eq!(f.vault.vote_weight_at(&f.alice, &6), 6);
    assert_eq!(f.vault.vote_weight_at(&f.alice, &105), 105);
}

// ── minimum stake (Issue #35) ─────────────────────────────────────────────────

#[test]
fn test_stake_exactly_at_minimum_succeeds() {
    let f = VaultFixture::new();
    f.vault.set_min_stake(&100_000);

    assert_eq!(f.vault.get_min_stake(), 100_000);
    assert_eq!(f.vault.stake(&f.alice, &100_000), 100_000);
}

#[test]
fn test_stake_below_minimum_fails() {
    let f = VaultFixture::new();
    f.vault.set_min_stake(&100_000);

    let result = f.vault.try_stake(&f.alice, &99_999);
    assert_eq!(result, Err(Ok(VaultError::BelowMinimumStake)));
}

#[test]
fn test_minimum_stake_can_be_disabled() {
    let f = VaultFixture::new();
    f.vault.set_min_stake(&100_000);
    f.vault.set_min_stake(&0);

    assert_eq!(f.vault.get_min_stake(), 0);
    assert_eq!(f.vault.stake(&f.alice, &1), 1);
}

#[test]
fn test_top_up_below_minimum_must_reach_threshold() {
    let f = VaultFixture::new();

    f.vault.set_min_stake(&0);
    f.vault.stake(&f.alice, &40_000);

    f.vault.set_min_stake(&100_000);
    let result = f.vault.try_stake(&f.alice, &50_000);
    assert_eq!(result, Err(Ok(VaultError::BelowMinimumStake)));

    assert_eq!(f.vault.stake(&f.alice, &60_000), 60_000);
    assert_eq!(f.vault.current_vote_weight(&f.alice), 100_000);
}

#[test]
fn test_admin_can_update_minimum_stake() {
    let f = VaultFixture::new();

    f.vault.set_min_stake(&100_000);
    assert_eq!(f.vault.get_min_stake(), 100_000);

    f.vault.set_min_stake(&50_000);
    assert_eq!(f.vault.get_min_stake(), 50_000);
}

// ── reward boost schedule (Issue #36) ─────────────────────────────────────────

#[test]
fn test_no_boost_schedule_means_base_multiplier_only() {
    let f = VaultFixture::new();
    let annual_stake = STELLAR_LEDGERS_PER_YEAR as i128;

    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.stake(&f.alice, &annual_stake);

    set_ledger(&f.env, 20);
    assert_eq!(f.vault.get_boost_multiplier(&f.alice), BOOST_BPS_BASE);
    assert_eq!(f.vault.calc_pending_reward(&f.alice), 20);
}

#[test]
fn test_boost_schedule_round_trips_and_applies_by_tier() {
    let f = VaultFixture::new();
    let annual_stake = STELLAR_LEDGERS_PER_YEAR as i128;
    let schedule = boost_schedule(&f.env, &[(10, 11_000), (20, 12_500)]);

    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.set_boost_schedule(&schedule);
    f.vault.stake(&f.alice, &annual_stake);

    let configured = f.vault.get_boost_schedule();
    assert_eq!(configured.len(), 2);
    assert_eq!(configured.get(0), Some((10, 11_000)));
    assert_eq!(configured.get(1), Some((20, 12_500)));

    set_ledger(&f.env, 9);
    assert_eq!(f.vault.get_boost_multiplier(&f.alice), BOOST_BPS_BASE);

    set_ledger(&f.env, 10);
    assert_eq!(f.vault.get_boost_multiplier(&f.alice), 11_000);

    set_ledger(&f.env, 20);
    assert_eq!(f.vault.get_boost_multiplier(&f.alice), 12_500);

    set_ledger(&f.env, 28);
    assert_eq!(f.vault.calc_pending_reward(&f.alice), 31);
}

#[test]
fn test_claim_does_not_reset_boost_tier() {
    let f = VaultFixture::new();
    let annual_stake = STELLAR_LEDGERS_PER_YEAR as i128;
    let schedule = boost_schedule(&f.env, &[(10, 11_000)]);

    f.token_admin.mint(&f.admin, &(annual_stake * 2));
    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.set_boost_schedule(&schedule);
    f.vault.fund_reward_pool(&f.admin, &(annual_stake * 2));
    f.vault.stake(&f.alice, &annual_stake);

    set_ledger(&f.env, 20);
    assert_eq!(f.vault.claim(&f.alice), 21);
    assert_eq!(f.vault.get_boost_multiplier(&f.alice), 11_000);

    set_ledger(&f.env, 30);
    assert_eq!(f.vault.calc_pending_reward(&f.alice), 11);
}

#[test]
fn test_reward_checkpoint_on_top_up_avoids_overpaying() {
    let f = VaultFixture::new();
    let annual_stake = STELLAR_LEDGERS_PER_YEAR as i128;

    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.stake(&f.alice, &annual_stake);

    set_ledger(&f.env, 100);
    f.vault.stake(&f.alice, &annual_stake);

    set_ledger(&f.env, 200);
    assert_eq!(f.vault.calc_pending_reward(&f.alice), 300);
}

// ── reward token decimal normalization ────────────────────────────────────────

#[test]
fn test_initialize_defaults_decimals_to_seven() {
    // Pools initialized without explicit decimals fall back to 7/7.
    let f = VaultFixture::new();
    assert_eq!(f.vault.stake_decimals(), 7);
    assert_eq!(f.vault.reward_decimals(), 7);
}

#[test]
fn test_initialize_stores_custom_decimals() {
    let f = VaultFixture::with_decimals(7, 6);
    assert_eq!(f.vault.stake_decimals(), 7);
    assert_eq!(f.vault.reward_decimals(), 6);
}

#[test]
fn test_pending_reward_same_decimals_unchanged() {
    // With matching decimals the normalized reward equals the raw reward,
    // preserving the existing behaviour. Raw reward over `n` ledgers at a
    // 100% APR on a one-year stake is exactly `n`.
    let f = VaultFixture::with_decimals(7, 7);
    let annual_stake = STELLAR_LEDGERS_PER_YEAR as i128;

    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.stake(&f.alice, &annual_stake);

    set_ledger(&f.env, 100);
    assert_eq!(f.vault.calc_pending_reward(&f.alice), 100);
}

#[test]
fn test_pending_reward_scaled_down_when_reward_decimals_smaller() {
    // Reward token has fewer decimals than the stake token (6 vs 7), so the
    // raw reward of 100 is divided by 10^(7-6) = 10.
    let f = VaultFixture::with_decimals(7, 6);
    let annual_stake = STELLAR_LEDGERS_PER_YEAR as i128;

    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.stake(&f.alice, &annual_stake);

    set_ledger(&f.env, 100);
    assert_eq!(f.vault.calc_pending_reward(&f.alice), 10);
}

#[test]
fn test_pending_reward_scaled_up_when_reward_decimals_larger() {
    // Reward token has more decimals than the stake token (9 vs 7), so the
    // raw reward of 100 is multiplied by 10^(9-7) = 100.
    let f = VaultFixture::with_decimals(7, 9);
    let annual_stake = STELLAR_LEDGERS_PER_YEAR as i128;

    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.stake(&f.alice, &annual_stake);

    set_ledger(&f.env, 100);
    assert_eq!(f.vault.calc_pending_reward(&f.alice), 10_000);
}

// ── pool cap (TVL limit) ──────────────────────────────────────────────────────

#[test]
fn test_stake_within_cap_succeeds() {
    let f = VaultFixture::new();
    f.vault.set_pool_cap(&1_000_000);

    let shares = f.vault.stake(&f.alice, &500_000);
    assert_eq!(shares, 500_000);
    assert_eq!(f.vault.shares_of(&f.alice), 500_000);

    let cap = f.vault.get_pool_cap();
    assert_eq!(cap, 1_000_000);
}

#[test]
fn test_stake_exceeding_cap_fails() {
    let f = VaultFixture::new();
    f.vault.set_pool_cap(&1_000_000);

    f.vault.stake(&f.alice, &800_000);

    let result = f.vault.try_stake(&f.alice, &300_000);
    assert_eq!(result, Err(Ok(VaultError::PoolCapReached)));
}

#[test]
fn test_stake_at_exact_cap_boundary_succeeds() {
    let f = VaultFixture::new();
    f.vault.set_pool_cap(&1_000_000);

    f.vault.stake(&f.alice, &900_000);

    let shares = f.vault.stake(&f.bob, &100_000);
    assert_eq!(shares, 100_000);

    let (_shares, total_deposited) = f.vault.vault_state();
    assert_eq!(total_deposited, 1_000_000);
}

#[test]
fn test_stake_one_over_cap_fails() {
    let f = VaultFixture::new();
    f.vault.set_pool_cap(&1_000_000);

    f.vault.stake(&f.alice, &900_000);

    let result = f.vault.try_stake(&f.bob, &100_001);
    assert_eq!(result, Err(Ok(VaultError::PoolCapReached)));
}

#[test]
fn test_cap_disabled_allows_unlimited_staking() {
    let f = VaultFixture::new();
    f.vault.set_pool_cap(&0);

    f.vault.stake(&f.alice, &10_000_000);
    f.vault.stake(&f.bob, &20_000_000);

    let (_shares, total_deposited) = f.vault.vault_state();
    assert_eq!(total_deposited, 30_000_000);
}

#[test]
fn test_admin_can_raise_and_lower_cap() {
    let f = VaultFixture::new();
    f.vault.set_pool_cap(&500_000);
    assert_eq!(f.vault.get_pool_cap(), 500_000);

    f.vault.set_pool_cap(&2_000_000);
    assert_eq!(f.vault.get_pool_cap(), 2_000_000);

    f.vault.set_pool_cap(&1_000_000);
    assert_eq!(f.vault.get_pool_cap(), 1_000_000);
}

#[test]
fn test_set_pool_cap_requires_admin_auth() {
    // With mock_all_auths, env.auths() records every require_auth() call.
    // Verify that set_pool_cap requires the stored admin to authorise the call.
    let f = VaultFixture::new();
    f.vault.set_pool_cap(&1_000_000);
    let auths = f.env.auths();
    let admin_auth_required = auths.iter().any(|(addr, _)| *addr == f.admin);
    assert!(admin_auth_required, "set_pool_cap must require admin auth");
}

#[test]
fn test_lowering_cap_below_current_tvl_blocks_new_stakes() {
    let f = VaultFixture::new();
    f.vault.set_pool_cap(&1_000_000);

    f.vault.stake(&f.alice, &1_000_000);

    f.vault.set_pool_cap(&500_000);
    assert_eq!(f.vault.get_pool_cap(), 500_000);

    let (_shares, total_deposited) = f.vault.vault_state();
    assert_eq!(total_deposited, 1_000_000);

    let result = f.vault.try_stake(&f.bob, &1);
    assert_eq!(result, Err(Ok(VaultError::PoolCapReached)));
}

#[test]
fn test_existing_stakers_unaffected_when_cap_lowered() {
    let f = VaultFixture::new();
    f.vault.set_pool_cap(&1_000_000);

    f.vault.stake(&f.alice, &1_000_000);

    f.vault.set_pool_cap(&500_000);

    let shares = f.vault.shares_of(&f.alice);
    assert_eq!(shares, 1_000_000);

    let preview = f.vault.preview_redeem(&shares);
    assert_eq!(preview, 1_000_000);

    let withdrawn = f.vault.withdraw(&f.alice, &shares);
    assert_eq!(withdrawn, 1_000_000);
}

#[test]
fn test_pool_cap_updated_emits_event() {
    let f = VaultFixture::new();

    f.vault.set_pool_cap(&1_000_000);

    let events = f.env.events().all();
    let cap_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| {
            let symbol = Symbol::try_from_val(&f.env, &topics.get(0).unwrap()).unwrap();
            symbol == Symbol::new(&f.env, "cap_upd")
        })
        .collect();

    assert_eq!(cap_events.len(), 1);
    let event = &cap_events[0];
    assert_eq!(
        Address::try_from_val(&f.env, &event.1.get(1).unwrap()).unwrap(),
        f.admin
    );
    let data = soroban_sdk::Vec::<soroban_sdk::Val>::try_from_val(&f.env, &event.2).unwrap();
    assert_eq!(i128::try_from_val(&f.env, &data.get(0).unwrap()).unwrap(), 1_000_000);
    assert_eq!(f.vault.get_pool_cap(), 1_000_000);
}

#[test]
fn test_pool_cap_defaults_to_zero() {
    let f = VaultFixture::new();

    assert_eq!(f.vault.get_pool_cap(), 0);
}

#[test]
fn test_stake_for_respects_pool_cap() {
    let f = VaultFixture::new();
    f.vault.set_pool_cap(&1_000_000);

    f.vault.approve_delegate(&f.alice, &f.bob);

    f.vault.stake(&f.alice, &800_000);

    let result = f.vault.try_stake_for(&f.bob, &f.alice, &300_000);
    assert_eq!(result, Err(Ok(VaultError::PoolCapReached)));

    let shares = f.vault.stake_for(&f.bob, &f.alice, &100_000);
    assert_eq!(shares, 100_000);
}

#[test]
fn test_set_pool_cap_negative_fails() {
    let f = VaultFixture::new();

    let result = f.vault.try_set_pool_cap(&-1);
    assert_eq!(result, Err(Ok(VaultError::ZeroAmount)));
}

// ── total_rewards_paid (Issue #71) ──────────────────────────────────────────

#[test]
fn test_total_rewards_paid_starts_at_zero() {
    let f = VaultFixture::new();
    assert_eq!(f.vault.total_rewards_paid(), 0);
}

#[test]
fn test_total_rewards_paid_increments_after_claim() {
    let f = VaultFixture::new();
    let annual_stake = STELLAR_LEDGERS_PER_YEAR as i128;

    f.token_admin.mint(&f.admin, &(annual_stake * 2));
    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.fund_reward_pool(&f.admin, &(annual_stake * 2));
    f.vault.stake(&f.alice, &annual_stake);

    set_ledger(&f.env, 100);
    let claim_amount = f.vault.claim(&f.alice);
    assert!(claim_amount > 0);
    assert_eq!(f.vault.total_rewards_paid(), claim_amount);
}

#[test]
fn test_total_rewards_paid_accumulates_across_claims() {
    let f = VaultFixture::new();
    let annual_stake = STELLAR_LEDGERS_PER_YEAR as i128;

    f.token_admin.mint(&f.admin, &(annual_stake * 2));
    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.fund_reward_pool(&f.admin, &(annual_stake * 2));
    f.vault.stake(&f.alice, &annual_stake);

    set_ledger(&f.env, 100);
    let claim1 = f.vault.claim(&f.alice);
    assert_eq!(f.vault.total_rewards_paid(), claim1);

    set_ledger(&f.env, 200);
    let claim2 = f.vault.claim(&f.alice);
    assert_eq!(f.vault.total_rewards_paid(), claim1 + claim2);
}

#[test]
fn test_total_rewards_paid_increments_after_unstake_then_claim() {
    let f = VaultFixture::new();
    let annual_stake = STELLAR_LEDGERS_PER_YEAR as i128;

    f.token_admin.mint(&f.admin, &(annual_stake * 2));
    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.fund_reward_pool(&f.admin, &(annual_stake * 2));
    f.vault.stake(&f.alice, &annual_stake);

    set_ledger(&f.env, 100);
    f.vault.unstake(&f.alice, &annual_stake);

    let claim_amount = f.vault.claim(&f.alice);
    assert!(claim_amount > 0);
    assert_eq!(f.vault.total_rewards_paid(), claim_amount);
}

// ── get_stake_token (Issue #64) ─────────────────────────────────────────────

#[test]
fn test_get_stake_token_returns_initialized_token() {
    let f = VaultFixture::new();
    // get_stake_token() and get_pool_config().stake_token must agree since both
    // read the same underlying value set during initialize.
    let stake_token = f.vault.get_stake_token();
    let config = f.vault.get_pool_config();
    assert_eq!(stake_token, config.stake_token);
}

#[test]
fn test_get_stake_token_before_init_fails() {
    let env = Env::default();
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    let result = vault.try_get_stake_token();
    assert_eq!(result, Err(Ok(VaultError::NotInitialized)));
}

// ── simulation functions (Issue #54) ────────────────────────────────────────

#[test]
fn test_simulate_stake_zero_rate() {
    let f = VaultFixture::new();
    assert_eq!(f.vault.simulate_stake(&1_000_000, &1000), 0);
}

#[test]
fn test_simulate_stake_known_output() {
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);

    let result = f.vault.simulate_stake(&1_000_000, &STELLAR_LEDGERS_PER_YEAR);
    assert_eq!(result, 1_000_000);
}

#[test]
fn test_simulate_compound_zero_rate() {
    let f = VaultFixture::new();
    assert_eq!(f.vault.simulate_compound(&1_000_000, &1000, &100), 0);
}

#[test]
fn test_simulate_compound_zero_interval() {
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    assert_eq!(f.vault.simulate_compound(&1_000_000, &1000, &0), 0);
}

#[test]
fn test_simulate_compound_matches_single_stake_for_one_interval() {
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);

    let ledgers = 1000;
    let compound = f
        .vault
        .simulate_compound(&1_000_000, &ledgers, &ledgers);
    let simple = f.vault.simulate_stake(&1_000_000, &ledgers);
    assert_eq!(compound, simple);
}

#[test]
fn test_simulate_compound_yields_more_than_simple() {
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);

    // 1_000_000_000 principal ensures each compounding interval produces enough
    // reward that the cumulative boost is visible above integer-division truncation.
    let compound = f.vault.simulate_compound(&1_000_000_000, &10_000, &1_000);
    let simple = f.vault.simulate_stake(&1_000_000_000, &10_000);
    assert!(compound > simple);
}

#[test]
fn test_simulate_boost_impact_no_schedule() {
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);

    let (base, boosted) = f.vault.simulate_boost_impact(&1_000_000, &1000);
    assert_eq!(base, boosted);
}

#[test]
fn test_simulate_boost_impact_with_schedule() {
    let f = VaultFixture::new();
    let schedule = boost_schedule(&f.env, &[(500, 15_000)]);
    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.set_boost_schedule(&schedule);

    // 6_308 ledgers at 100% APR on 1M principal produces exactly 1_000 base reward
    // (1_000_000 * 10_000 * 6_308 / 10_000 / 6_307_200 = 1_000).
    let (base, boosted) = f.vault.simulate_boost_impact(&1_000_000, &6_308);
    assert_eq!(base, 1_000);
    assert!(boosted > base);
}

// ── get_pool_config (#76) ─────────────────────────────────────────────────────

#[test]
fn test_get_pool_config_returns_all_fields() {
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&500_u32);

    let config = f.vault.get_pool_config();

    assert_eq!(config.admin, f.admin);
    // stake_token and reward_token are the same single-token vault token
    assert_eq!(config.stake_token, config.reward_token);
    assert_eq!(config.reward_rate_bps, 500_u32);
    assert!(!config.paused);
}

#[test]
fn test_get_pool_config_reflects_paused_state() {
    let f = VaultFixture::new();
    f.vault.pause();

    let config = f.vault.get_pool_config();
    assert!(config.paused);

    f.vault.unpause();
    let config2 = f.vault.get_pool_config();
    assert!(!config2.paused);
}

// ── stake_and_claim (#77) ─────────────────────────────────────────────────────

fn setup_reward_pool(f: &VaultFixture) {
    f.token_admin.mint(&f.admin, &5_000_000);
    f.vault.fund_reward_pool(&f.admin, &5_000_000);
    f.vault.set_reward_rate_bps(&1000_u32); // 10% APR
}

#[test]
fn test_stake_and_claim_with_pending_reward_settles_correctly() {
    let f = VaultFixture::new();
    setup_reward_pool(&f);

    // Alice stakes at ledger 0
    f.vault.stake(&f.alice, &1_000_000);

    // Advance ledger so rewards accrue
    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR);

    let balance_before = f.token.balance(&f.alice);

    // Alice stakes more and claims simultaneously
    let claimed = f.vault.stake_and_claim(&f.alice, &500_000);

    // Reward should be positive (10% APR × 1 year ≈ 100_000)
    assert!(claimed > 0, "claimed reward must be positive");
    // Alice's token balance should have decreased by 500_000 (new stake) minus the claimed reward
    let balance_after = f.token.balance(&f.alice);
    assert_eq!(balance_after, balance_before - 500_000 + claimed);
}

#[test]
fn test_stake_and_claim_no_pending_reward_still_stakes() {
    let f = VaultFixture::new();
    setup_reward_pool(&f);

    // Alice stakes at ledger 0 — no time elapses so no reward yet
    f.vault.stake(&f.alice, &500_000);

    let claimed = f.vault.stake_and_claim(&f.alice, &200_000);

    assert_eq!(claimed, 0, "no reward should accrue within the same ledger");
    // New stake should have been added: 500_000 + 200_000 = 700_000 shares
    assert_eq!(f.vault.shares_of(&f.alice), 700_000);
}

#[test]
fn test_stake_and_claim_emits_claimed_then_deposit_events() {
    let f = VaultFixture::new();
    setup_reward_pool(&f);

    f.vault.stake(&f.alice, &1_000_000);
    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR);

    f.vault.stake_and_claim(&f.alice, &500_000);

    let events = f.env.events().all();
    let mut found_claimed = false;
    let mut found_deposit = false;
    let mut claimed_index = usize::MAX;
    let mut deposit_index = usize::MAX;

    for (i, (_contract_id, topics, _data)) in events.iter().enumerate() {
        if topic_matches(&f.env, &topics, "claimed") {
            found_claimed = true;
            claimed_index = i;
        }
        if topic_matches(&f.env, &topics, "deposit") {
            found_deposit = true;
            deposit_index = i;
        }
    }

    assert!(found_claimed, "claimed event must be emitted");
    assert!(found_deposit, "deposit (staked) event must be emitted");
    assert!(
        claimed_index < deposit_index,
        "claimed event must precede deposit event"
    );
}

#[test]
fn test_stake_and_claim_new_stake_amount_added_correctly() {
    let f = VaultFixture::new();
    setup_reward_pool(&f);

    // Alice opens a position first
    f.vault.stake(&f.alice, &1_000_000);
    set_ledger(&f.env, 100);

    let shares_before = f.vault.shares_of(&f.alice);

    f.vault.stake_and_claim(&f.alice, &300_000);

    let shares_after = f.vault.shares_of(&f.alice);
    assert!(
        shares_after > shares_before,
        "share count must increase after stake_and_claim"
    );
}

// ── set_claim_cap / get_claim_window (#78) ────────────────────────────────────

fn setup_with_cap(cap: i128, window: u32) -> VaultFixture<'static> {
    let f = VaultFixture::new();
    setup_reward_pool(&f);
    f.vault.set_claim_cap(&f.admin, &cap, &window);
    f
}

#[test]
fn test_claim_within_cap_succeeds_fully() {
    let f = setup_with_cap(500_000, 100_000);

    f.vault.stake(&f.alice, &1_000_000);
    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR);

    // Expected reward ≈ 100_000 (10% APR × 1M for 1 year), which is well under the 500_000 cap
    let claimed = f.vault.claim(&f.alice);
    assert!(claimed > 0, "claim within cap must return the full reward");

    // Claimed amount must be fully reflected in claim window
    let window = f.vault.get_claim_window(&f.alice).unwrap();
    assert_eq!(window.claimed_in_window, claimed);
}

#[test]
fn test_claim_exceeding_cap_is_truncated() {
    // Cap at 50_000 — less than the expected ~100_000 reward for 1 year at 10% APR on 1M stake
    let f = setup_with_cap(50_000, 100_000);

    f.vault.stake(&f.alice, &1_000_000);
    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR);

    let claimed = f.vault.claim(&f.alice);
    assert_eq!(claimed, 50_000, "claim must be truncated to the cap");

    // Remainder should still be accrued — a second claim (same window) pays 0
    let claimed2 = f.vault.claim(&f.alice);
    assert_eq!(claimed2, 0, "no further claim allowed until window resets");
}

#[test]
fn test_claim_window_resets_after_expiry() {
    // Cap at 50_000, window = 100 ledgers
    let f = setup_with_cap(50_000, 100);

    f.vault.stake(&f.alice, &1_000_000);
    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR);

    // First claim exhausts the window
    let first = f.vault.claim(&f.alice);
    assert_eq!(first, 50_000);

    // Jump past the window boundary
    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR + 200);

    // Second claim should be allowed again (window reset)
    let second = f.vault.claim(&f.alice);
    assert!(second > 0, "claim after window reset must succeed");
}

#[test]
fn test_cap_zero_disables_limit() {
    // Cap 0 = no limit
    let f = setup_with_cap(0, 100_000);

    f.vault.stake(&f.alice, &1_000_000);
    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR);

    let claimed = f.vault.claim(&f.alice);
    assert!(claimed > 0, "unlimited claim (cap=0) must return full reward");

    // No ClaimWindow record should be written when cap is disabled
    let window_opt = f.vault.get_claim_window(&f.alice);
    assert!(window_opt.is_none(), "no window stored when cap is disabled");
}

// ── TWAP / rate history tests ─────────────────────────────────────────────────

#[test]
fn test_twap_single_rate_equals_current_rate() {
    let f = VaultFixture::new();

    f.vault.set_reward_rate_bps(&1500);
    set_ledger(&f.env, 100);

    let twap = f.vault.twap_apr_bps(&50);
    assert_eq!(twap, 1500);

    let twap = f.vault.twap_apr_bps(&100);
    assert_eq!(twap, 1500);
}

#[test]
fn test_twap_two_rates_calculated_correctly() {
    let f = VaultFixture::new();

    f.vault.set_reward_rate_bps(&1000);
    set_ledger(&f.env, 100);

    f.vault.set_reward_rate_bps(&2000);
    set_ledger(&f.env, 200);

    let twap = f.vault.twap_apr_bps(&100);
    assert_eq!(twap, 2000);

    f.vault.set_reward_rate_bps(&3000);
    set_ledger(&f.env, 300);

    let twap = f.vault.twap_apr_bps(&200);
    assert_eq!(twap, 2500);
}

#[test]
fn test_twap_with_window_starting_before_first_change() {
    let f = VaultFixture::new();

    set_ledger(&f.env, 50);
    f.vault.set_reward_rate_bps(&1000);
    set_ledger(&f.env, 100);
    f.vault.set_reward_rate_bps(&2000);
    set_ledger(&f.env, 200);

    let twap = f.vault.twap_apr_bps(&150);
    assert_eq!(twap, 1666);
}

#[test]
fn test_rate_history_capped_at_50_entries() {
    let f = VaultFixture::new();

    f.vault.set_reward_rate_bps(&1000);

    for i in 1..=60 {
        set_ledger(&f.env, i * 10);
        f.vault.set_reward_rate_bps(&(1000 + i as u32));
    }

    set_ledger(&f.env, 650);

    let history = f.vault.get_rate_history();
    assert_eq!(history.len(), 50);

    let first_entry = history.get(0).unwrap();
    assert!(first_entry.0 > 10);
}

#[test]
fn test_get_rate_history_returns_full_history() {
    let f = VaultFixture::new();

    f.vault.set_reward_rate_bps(&1000);
    set_ledger(&f.env, 100);
    f.vault.set_reward_rate_bps(&2000);
    set_ledger(&f.env, 200);
    f.vault.set_reward_rate_bps(&3000);

    let history = f.vault.get_rate_history();
    assert_eq!(history.len(), 3);

    let entry1 = history.get(0).unwrap();
    assert_eq!(entry1.0, 0);
    assert_eq!(entry1.1, 0);

    let entry2 = history.get(1).unwrap();
    assert_eq!(entry2.0, 100);
    assert_eq!(entry2.1, 1000);

    let entry3 = history.get(2).unwrap();
    assert_eq!(entry3.0, 200);
    assert_eq!(entry3.1, 2000);
}

#[test]
fn test_twap_zero_window_returns_current_rate() {
    let f = VaultFixture::new();

    f.vault.set_reward_rate_bps(&1500);

    let twap = f.vault.twap_apr_bps(&0);
    assert_eq!(twap, 1500);
}

// ── Issue #49: ledgers_to_target / days_to_target ─────────────────────────────

#[test]
fn test_ledgers_to_target_target_already_met_returns_zero() {
    let f = VaultFixture::new();
    let annual = STELLAR_LEDGERS_PER_YEAR as i128;

    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.stake(&f.alice, &annual);

    // Advance so Alice has 100 pending rewards
    set_ledger(&f.env, 100);
    // Target is less than or equal to pending → should return 0
    assert_eq!(f.vault.ledgers_to_target(&f.alice, &1), 0);
    assert_eq!(f.vault.ledgers_to_target(&f.alice, &99), 0);
    assert_eq!(f.vault.ledgers_to_target(&f.alice, &100), 0);
}

#[test]
fn test_ledgers_to_target_no_position_returns_max() {
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    assert_eq!(f.vault.ledgers_to_target(&f.alice, &1000), u32::MAX);
}

#[test]
fn test_ledgers_to_target_zero_rate_returns_max() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &1_000_000);
    assert_eq!(f.vault.ledgers_to_target(&f.alice, &500), u32::MAX);
}

#[test]
fn test_ledgers_to_target_known_input() {
    let f = VaultFixture::new();
    let annual = STELLAR_LEDGERS_PER_YEAR as i128;

    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.stake(&f.alice, &annual);

    let ledgers = f.vault.ledgers_to_target(&f.alice, &100);
    assert_eq!(ledgers, 100);
}

#[test]
fn test_days_to_target_sentinel_for_max_ledgers() {
    let f = VaultFixture::new();
    assert_eq!(f.vault.days_to_target(&f.alice, &1000), u32::MAX);
}

#[test]
fn test_days_to_target_converts_ledgers_correctly() {
    let f = VaultFixture::new();
    let annual = STELLAR_LEDGERS_PER_YEAR as i128;

    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.stake(&f.alice, &annual);

    let days = f.vault.days_to_target(&f.alice, &100);
    assert_eq!(days, 1);

    let days = f.vault.days_to_target(&f.alice, &17280);
    assert_eq!(days, 1);

    let days = f.vault.days_to_target(&f.alice, &17281);
    assert_eq!(days, 2);
}

// ── Issue #48: Boost Campaign ─────────────────────────────────────────────────

#[test]
fn test_start_boost_campaign_activates() {
    let f = VaultFixture::new();
    set_ledger(&f.env, 10);

    f.vault.start_boost_campaign(&20_000, &100);

    let campaign = f.vault.active_campaign();
    assert!(campaign.is_some());
    let (mult, ends) = campaign.unwrap();
    assert_eq!(mult, 20_000);
    assert_eq!(ends, 110); // 10 + 100
}

#[test]
fn test_active_campaign_returns_none_when_none() {
    let f = VaultFixture::new();
    let campaign = f.vault.active_campaign();
    assert!(campaign.is_none());
}

#[test]
fn test_active_campaign_returns_none_after_expiry() {
    let f = VaultFixture::new();
    set_ledger(&f.env, 0);
    f.vault.start_boost_campaign(&20_000, &50);

    set_ledger(&f.env, 51);
    let campaign = f.vault.active_campaign();
    assert!(campaign.is_none());
}

#[test]
fn test_second_campaign_rejected_while_first_active() {
    let f = VaultFixture::new();
    f.vault.start_boost_campaign(&15_000, &200);

    let result = f.vault.try_start_boost_campaign(&20_000, &100);
    assert_eq!(result, Err(Ok(VaultError::CampaignAlreadyActive)));
}

#[test]
fn test_end_boost_campaign_cancels_early() {
    let f = VaultFixture::new();
    f.vault.start_boost_campaign(&20_000, &500);

    f.vault.end_boost_campaign();

    let campaign = f.vault.active_campaign();
    assert!(campaign.is_none());
}

#[test]
fn test_end_boost_campaign_when_none_active_fails() {
    let f = VaultFixture::new();
    let result = f.vault.try_end_boost_campaign();
    assert_eq!(result, Err(Ok(VaultError::NoCampaignActive)));
}

#[test]
fn test_campaign_boosts_rewards_during_window() {
    let f = VaultFixture::new();
    let annual = STELLAR_LEDGERS_PER_YEAR as i128;

    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE); // 100% APR → 1 token/ledger baseline
    f.vault.stake(&f.alice, &annual);

    set_ledger(&f.env, 1);
    // Start a 2x campaign for 10 ledgers
    f.vault.start_boost_campaign(&20_000, &10);

    // At ledger 11: 1 base ledger (0→1) + 10 campaign 2x ledgers (1→11) = 1 + 20 = 21
    set_ledger(&f.env, 11);
    let pending = f.vault.calc_pending_reward(&f.alice);
    assert_eq!(pending, 21);
}

#[test]
fn test_campaign_reverts_to_base_rate_after_expiry() {
    let f = VaultFixture::new();
    let annual = STELLAR_LEDGERS_PER_YEAR as i128;

    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.stake(&f.alice, &annual);

    // Campaign: 2x for 10 ledgers (ledgers 0..10)
    f.vault.start_boost_campaign(&20_000, &10);

    // At ledger 20: 10 campaign ledgers (2 each) + 10 base ledgers (1 each) = 30
    set_ledger(&f.env, 20);
    let pending = f.vault.calc_pending_reward(&f.alice);
    assert_eq!(pending, 30);
}

#[test]
fn test_campaign_time_weighted_claim_across_boundary() {
    let f = VaultFixture::new();
    let annual = STELLAR_LEDGERS_PER_YEAR as i128;

    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.stake(&f.alice, &annual);

    // Campaign starts at ledger 5, runs for 10 ledgers (ends at 15)
    set_ledger(&f.env, 5);
    f.vault.start_boost_campaign(&20_000, &10);

    // At ledger 20:
    // ledgers 0-5 (before campaign): 5 * 1 = 5
    // ledgers 5-15 (campaign, 2x): 10 * 2 = 20
    // ledgers 15-20 (after campaign): 5 * 1 = 5
    // total = 30
    set_ledger(&f.env, 20);
    let pending = f.vault.calc_pending_reward(&f.alice);
    assert_eq!(pending, 30);
}

#[test]
fn test_campaign_emits_started_event() {
    let f = VaultFixture::new();
    f.vault.start_boost_campaign(&15_000, &100);

    let events = f.env.events().all();
    let camp_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "camp_on"))
        .collect();
    assert_eq!(camp_events.len(), 1);
}

#[test]
fn test_campaign_emits_ended_event() {
    let f = VaultFixture::new();
    f.vault.start_boost_campaign(&15_000, &100);
    f.vault.end_boost_campaign();

    let events = f.env.events().all();
    let camp_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "camp_off"))
        .collect();
    assert_eq!(camp_events.len(), 1);
}

#[test]
fn test_start_campaign_after_expiry_succeeds() {
    let f = VaultFixture::new();
    f.vault.start_boost_campaign(&15_000, &10);

    // Advance past expiry, then start a new one
    set_ledger(&f.env, 11);
    f.vault.start_boost_campaign(&20_000, &50);

    let campaign = f.vault.active_campaign();
    assert!(campaign.is_some());
    assert_eq!(campaign.unwrap().0, 20_000);
}

// ── Issue #43: transfer_position ─────────────────────────────────────────────

#[test]
fn test_transfer_position_success() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &500_000);

    f.vault.transfer_position(&f.alice, &f.bob);

    assert_eq!(f.vault.shares_of(&f.alice), 0);
    assert_eq!(f.vault.shares_of(&f.bob), 500_000);
}

#[test]
fn test_transfer_position_no_position_fails() {
    let f = VaultFixture::new();
    // Alice has nothing staked
    let result = f.vault.try_transfer_position(&f.alice, &f.bob);
    assert_eq!(result, Err(Ok(VaultError::PositionNotFound)));
}

#[test]
fn test_transfer_position_recipient_already_staking_fails() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &500_000);
    f.vault.stake(&f.bob, &100_000);

    let result = f.vault.try_transfer_position(&f.alice, &f.bob);
    assert_eq!(result, Err(Ok(VaultError::RecipientAlreadyStaking)));
}

#[test]
fn test_transfer_position_settles_pending_rewards() {
    let f = VaultFixture::new();
    let annual = STELLAR_LEDGERS_PER_YEAR as i128;

    f.token_admin.mint(&f.admin, &(annual * 2));
    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.fund_reward_pool(&f.admin, &(annual * 2));
    f.vault.stake(&f.alice, &annual);

    // Advance 100 ledgers so Alice accumulates rewards
    set_ledger(&f.env, 100);

    // Transfer position — pending rewards should be settled to Alice's accrued balance
    f.vault.transfer_position(&f.alice, &f.bob);

    // Alice should still be able to claim her accrued rewards (shares = 0 but accrued > 0)
    let claimed = f.vault.claim(&f.alice);
    assert_eq!(claimed, 100);
}

#[test]
fn test_transfer_position_lock_carries_over() {
    let f = VaultFixture::new();
    set_ledger(&f.env, 50);
    f.vault.stake(&f.alice, &500_000);

    // Record Alice's staked_at
    let alice_pos = f.vault.position_of(&f.alice).unwrap();
    assert_eq!(alice_pos.staked_at_ledger, 50);

    set_ledger(&f.env, 60);
    f.vault.transfer_position(&f.alice, &f.bob);

    let bob_pos = f.vault.position_of(&f.bob).unwrap();
    assert_eq!(bob_pos.staked_at_ledger, 50);
}

#[test]
fn test_transfer_position_recipient_starts_fresh_accrual() {
    let f = VaultFixture::new();
    let annual = STELLAR_LEDGERS_PER_YEAR as i128;

    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE);
    f.vault.stake(&f.alice, &annual);

    set_ledger(&f.env, 100);
    f.vault.transfer_position(&f.alice, &f.bob);

    // Bob just received the position — pending should be 0 (fresh start)
    assert_eq!(f.vault.calc_pending_reward(&f.bob), 0);

    // After 50 more ledgers, Bob should have accrued 50 tokens
    set_ledger(&f.env, 150);
    assert_eq!(f.vault.calc_pending_reward(&f.bob), 50);
}

#[test]
fn test_transfer_position_emits_event() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &500_000);
    f.vault.transfer_position(&f.alice, &f.bob);

    let events = f.env.events().all();
    let xfer_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "pos_xfer"))
        .collect();
    assert_eq!(xfer_events.len(), 1);
}

#[test]
fn test_transfer_position_total_shares_unchanged() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &500_000);

    let (total_before, deposited_before) = f.vault.vault_state();
    f.vault.transfer_position(&f.alice, &f.bob);
    let (total_after, deposited_after) = f.vault.vault_state();

    assert_eq!(total_before, total_after);
    assert_eq!(deposited_before, deposited_after);
}

#[test]
fn test_transfer_position_paused_fails() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &500_000);
    f.vault.pause();

    let result = f.vault.try_transfer_position(&f.alice, &f.bob);
    assert_eq!(result, Err(Ok(VaultError::VaultPaused)));
}

// ── Issue #46: Leaderboard ────────────────────────────────────────────────────

#[test]
fn test_leaderboard_empty_by_default() {
    let f = VaultFixture::new();
    f.vault.set_leaderboard_size(&10);
    let board = f.vault.get_leaderboard();
    assert_eq!(board.len(), 0);
}

#[test]
fn test_leaderboard_size_too_large_fails() {
    let f = VaultFixture::new();
    let result = f.vault.try_set_leaderboard_size(&21);
    assert_eq!(result, Err(Ok(VaultError::LeaderboardSizeTooLarge)));
}

#[test]
fn test_leaderboard_top_staker_appears_first() {
    let f = VaultFixture::new();
    f.vault.set_leaderboard_size(&10);

    f.vault.stake(&f.alice, &100_000);
    f.vault.stake(&f.bob, &500_000);

    let board = f.vault.get_leaderboard();
    assert_eq!(board.len(), 2);
    // Bob staked more, should appear first
    assert_eq!(board.get(0).unwrap().staker, f.bob);
    assert_eq!(board.get(1).unwrap().staker, f.alice);
}

#[test]
fn test_leaderboard_unstaking_drops_user() {
    let f = VaultFixture::new();
    f.vault.set_leaderboard_size(&10);

    f.vault.stake(&f.alice, &300_000);
    f.vault.stake(&f.bob, &200_000);

    // Alice fully unstakes
    let alice_shares = f.vault.shares_of(&f.alice);
    f.vault.unstake(&f.alice, &alice_shares);

    let board = f.vault.get_leaderboard();
    assert_eq!(board.len(), 1);
    assert_eq!(board.get(0).unwrap().staker, f.bob);
}

#[test]
fn test_leaderboard_respects_size_limit() {
    let f = VaultFixture::new();
    f.vault.set_leaderboard_size(&1);

    f.vault.stake(&f.alice, &100_000);
    f.vault.stake(&f.bob, &500_000); // larger — displaces Alice

    let board = f.vault.get_leaderboard();
    assert_eq!(board.len(), 1);
    assert_eq!(board.get(0).unwrap().staker, f.bob);
}

#[test]
fn test_leaderboard_small_staker_not_added_when_full() {
    let f = VaultFixture::new();
    f.vault.set_leaderboard_size(&1);

    f.vault.stake(&f.bob, &500_000);
    f.vault.stake(&f.alice, &100_000); // smaller, should not appear

    let board = f.vault.get_leaderboard();
    assert_eq!(board.len(), 1);
    assert_eq!(board.get(0).unwrap().staker, f.bob);
}

#[test]
fn test_leaderboard_updates_on_additional_stake() {
    let f = VaultFixture::new();
    f.vault.set_leaderboard_size(&10);

    f.vault.stake(&f.bob, &500_000);
    f.vault.stake(&f.alice, &100_000);

    // Alice stakes more to surpass Bob
    f.vault.stake(&f.alice, &1_000_000);

    let board = f.vault.get_leaderboard();
    assert_eq!(board.len(), 2);
    assert_eq!(board.get(0).unwrap().staker, f.alice);
}

#[test]
fn test_leaderboard_disabled_when_size_zero() {
    let f = VaultFixture::new();
    // No set_leaderboard_size call → size defaults to 0 → tracking disabled

    f.vault.stake(&f.alice, &500_000);
    f.vault.stake(&f.bob, &100_000);

    // get_leaderboard always returns the stored vec (empty when disabled)
    let board = f.vault.get_leaderboard();
    assert_eq!(board.len(), 0);
}

#[test]
fn test_leaderboard_trim_on_size_reduction() {
    let f = VaultFixture::new();
    f.vault.set_leaderboard_size(&5);

    f.vault.stake(&f.alice, &500_000);
    f.vault.stake(&f.bob, &300_000);

    let board = f.vault.get_leaderboard();
    assert_eq!(board.len(), 2);

    // Reduce size to 1 — should trim to top staker only
    f.vault.set_leaderboard_size(&1);

    let board = f.vault.get_leaderboard();
    assert_eq!(board.len(), 1);
    assert_eq!(board.get(0).unwrap().staker, f.alice);
}

#[test]
fn test_leaderboard_amounts_are_correct() {
    let f = VaultFixture::new();
    f.vault.set_leaderboard_size(&10);

    f.vault.stake(&f.alice, &400_000);
    f.vault.stake(&f.bob, &200_000);

    let board = f.vault.get_leaderboard();
    assert_eq!(board.get(0).unwrap().amount, 400_000);
    assert_eq!(board.get(1).unwrap().amount, 200_000);
}

// ── Issue #83: pool_initialized event ────────────────────────────────────────

#[test]
fn test_initialize_emits_pool_initialized_event() {
    // Fresh env so there are no spurious events from the fixture
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.min_temp_entry_ttl = 10_000_000;
        li.min_persistent_entry_ttl = 10_000_000;
        li.max_entry_ttl = 10_000_000;
    });

    let admin = Address::generate(&env);
    let (token_addr, _token, _token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);

    vault.initialize(&admin, &token_addr, &None, &None);

    let events = env.events().all();
    let init_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&env, topics, "init"))
        .collect();

    assert_eq!(init_events.len(), 1, "exactly one pool_initialized event expected after initialize");
}

#[test]
fn test_initialize_pool_initialized_event_not_emitted_on_reinit() {
    // On a second (rejected) initialize call, no new pool_initialized event should appear
    let f = VaultFixture::new();
    let events_before = f.env.events().all().len() as usize;

    let other_token = f.env.register_stellar_asset_contract(Address::generate(&f.env));
    let _ = f.vault.try_initialize(&f.admin, &other_token, &None, &None);

    // AlreadyInitialized — event list should not have grown by an "init" event
    let new_init: std::vec::Vec<_> = f
        .env
        .events()
        .all()
        .into_iter()
        .skip(events_before)
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "init"))
        .collect();

    assert!(new_init.is_empty(), "failed reinit must not emit pool_initialized");
}

// ── Issue #84: claimable_since ────────────────────────────────────────────────

#[test]
fn test_claimable_since_returns_last_claim_ledger_after_stake() {
    let f = VaultFixture::new();
    set_ledger(&f.env, 42);
    f.vault.stake(&f.alice, &500_000);

    // Immediately after staking, last_claim_ledger is the stake ledger (42)
    let since = f.vault.claimable_since(&f.alice);
    assert_eq!(since, 42);
}

#[test]
fn test_claimable_since_updates_after_claim() {
    let f = VaultFixture::new();
    // 10% APR on 10_000_000 for one year ≈ 1_000_000 reward — fund exactly that
    f.vault.set_reward_rate_bps(&1_000_u32);
    f.token_admin.mint(&f.admin, &1_000_000);
    f.vault.fund_reward_pool(&f.admin, &1_000_000_i128);
    f.vault.stake(&f.alice, &10_000_000);

    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR);
    // Claim resets last_claim_ledger to the current ledger
    f.vault.claim(&f.alice);

    let since = f.vault.claimable_since(&f.alice);
    assert_eq!(since, STELLAR_LEDGERS_PER_YEAR);
}

#[test]
fn test_claimable_since_no_position_fails() {
    let f = VaultFixture::new();
    let result = f.vault.try_claimable_since(&f.alice);
    assert_eq!(result, Err(Ok(VaultError::PositionNotFound)));
}

// ── Issue #85: batch_position_query ──────────────────────────────────────────

#[test]
fn test_batch_position_query_empty_input_returns_empty_vec() {
    let f = VaultFixture::new();
    let users: Vec<Address> = Vec::new(&f.env);
    let result = f.vault.batch_position_query(&users);
    assert_eq!(result.len(), 0);
}

#[test]
fn test_batch_position_query_mixed_stakers_and_non_stakers() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &500_000);
    // bob has no position

    let mut users = Vec::new(&f.env);
    users.push_back(f.alice.clone());
    users.push_back(f.bob.clone());

    let result = f.vault.batch_position_query(&users);
    assert_eq!(result.len(), 2);
    assert!(result.get(0).unwrap().is_some(), "alice should have a position");
    assert!(result.get(1).unwrap().is_none(), "bob has no position");
}

#[test]
fn test_batch_position_query_order_preserved() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &500_000);
    f.vault.stake(&f.bob, &300_000);

    let mut users = Vec::new(&f.env);
    users.push_back(f.bob.clone());   // bob first
    users.push_back(f.alice.clone()); // alice second

    let result = f.vault.batch_position_query(&users);
    assert_eq!(result.len(), 2);
    let bob_pos = result.get(0).unwrap().unwrap();
    let alice_pos = result.get(1).unwrap().unwrap();
    assert_eq!(bob_pos.amount, 300_000);
    assert_eq!(alice_pos.amount, 500_000);
}

#[test]
fn test_batch_position_query_exceeds_max_fails() {
    let f = VaultFixture::new();
    let mut users = Vec::new(&f.env);
    // 21 addresses — one over the 20-address limit
    for _ in 0..21 {
        users.push_back(Address::generate(&f.env));
    }
    let result = f.vault.try_batch_position_query(&users);
    assert_eq!(result, Err(Ok(VaultError::BatchTooLarge)));
}

#[test]
fn test_batch_position_query_exactly_20_succeeds() {
    let f = VaultFixture::new();
    let mut users = Vec::new(&f.env);
    for _ in 0..20 {
        users.push_back(Address::generate(&f.env));
    }
    let result = f.vault.try_batch_position_query(&users);
    assert!(result.is_ok());
}

// ── Issue #95: get_total_claimable ────────────────────────────────────────────

#[test]
fn test_get_total_claimable_single_staker_matches_calc_pending_reward() {
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&10_000_u32);
    f.vault.stake(&f.alice, &10_000_000);
    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR);

    let pending = f.vault.calc_pending_reward(&f.alice);
    let total = f.vault.get_total_claimable();
    assert_eq!(total, pending);
}

#[test]
fn test_get_total_claimable_two_stakers_sum_correctly() {
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&10_000_u32);
    f.vault.stake(&f.alice, &10_000_000);
    f.vault.stake(&f.bob, &5_000_000);
    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR);

    let alice_pending = f.vault.calc_pending_reward(&f.alice);
    let bob_pending = f.vault.calc_pending_reward(&f.bob);
    let total = f.vault.get_total_claimable();
    assert_eq!(total, alice_pending + bob_pending);
}

#[test]
fn test_get_total_claimable_fully_unstaked_user_excluded() {
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&10_000_u32);
    f.vault.stake(&f.alice, &10_000_000);
    f.vault.stake(&f.bob, &5_000_000);
    set_ledger(&f.env, 1000);

    // Fully unstake alice
    let alice_shares = f.vault.shares_of(&f.alice);
    f.vault.withdraw(&f.alice, &alice_shares);

    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR);

    // Only bob should be included
    let bob_pending = f.vault.calc_pending_reward(&f.bob);
    let total = f.vault.get_total_claimable();
    assert_eq!(total, bob_pending, "fully unstaked alice must not contribute to total_claimable");
}

#[test]
fn test_get_total_claimable_zero_stakers_returns_zero() {
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&10_000_u32);
    let total = f.vault.get_total_claimable();
    assert_eq!(total, 0);
}








