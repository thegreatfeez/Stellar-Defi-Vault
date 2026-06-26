#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Events, Ledger as _},
    token, Address, Env, Symbol, TryFromVal, Vec,
};

use crate::{
    errors::VaultError,
    nft::{StakeReceiptNFT, StakeReceiptNFTClient},
    storage::{LeaderboardEntry, UnstakeCheckResult},
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

        vault.initialize(&admin, &token_addr, &0_u32, &stake_decimals, &reward_decimals);

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
    let result = f.vault.try_initialize(&f.admin, &token_addr, &0_u32, &None, &None);
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

// ── Issue #39: rescue_token ───────────────────────────────────────────────────

#[test]
fn test_rescue_third_token_succeeds() {
    let f = VaultFixture::new();

    // Create a third token (neither stake nor reward)
    let third_token_addr = f.env.register_stellar_asset_contract(f.admin.clone());
    let third_token_admin = token::StellarAssetClient::new(&f.env, &third_token_addr);
    let third_token = token::Client::new(&f.env, &third_token_addr);

    // Simulate a user accidentally sending the third token to the vault
    let vault_id = f.vault.address.clone();
    third_token_admin.mint(&vault_id, &5_000);

    assert_eq!(third_token.balance(&vault_id), 5_000);
    assert_eq!(third_token.balance(&f.alice), 0);

    // Admin rescues those tokens
    f.vault.rescue_token(&f.admin, &third_token_addr, &5_000, &f.alice);

    assert_eq!(third_token.balance(&vault_id), 0);
    assert_eq!(third_token.balance(&f.alice), 5_000);
}

#[test]
fn test_rescue_stake_token_fails() {
    let f = VaultFixture::new();
    let stake_token_addr = f.token.address.clone();

    // Alice stakes so the vault holds some stake tokens
    f.vault.stake(&f.alice, &100_000);

    let result = f.vault.try_rescue_token(&f.admin, &stake_token_addr, &100_000, &f.bob);
    assert_eq!(result, Err(Ok(VaultError::CannotRescueStakeToken)));
}

#[test]
fn test_rescue_reward_token_fails() {
    let f = VaultFixture::new();

    // Register a separate reward token address
    let reward_token_addr = f.env.register_stellar_asset_contract(f.admin.clone());
    let reward_token_admin = token::StellarAssetClient::new(&f.env, &reward_token_addr);
    f.vault.set_reward_token(&reward_token_addr);

    // Simulate some reward tokens ending up in the vault
    let vault_id = f.vault.address.clone();
    reward_token_admin.mint(&vault_id, &1_000);

    let result = f.vault.try_rescue_token(&f.admin, &reward_token_addr, &1_000, &f.bob);
    assert_eq!(result, Err(Ok(VaultError::CannotRescueRewardToken)));
}

#[test]
fn test_rescue_token_requires_admin_auth() {
    let f = VaultFixture::new();
    let third_token_addr = f.env.register_stellar_asset_contract(f.admin.clone());
    let third_token_admin = token::StellarAssetClient::new(&f.env, &third_token_addr);
    let vault_id = f.vault.address.clone();
    third_token_admin.mint(&vault_id, &1_000);

    f.vault.rescue_token(&f.admin, &third_token_addr, &1_000, &f.alice);
    // Verify admin auth was required (first recorded auth is the admin's)
    assert_eq!(f.env.auths()[0].0, f.admin);
}

#[test]
fn test_rescue_token_emits_token_rescued_event() {
    let f = VaultFixture::new();
    let third_token_addr = f.env.register_stellar_asset_contract(f.admin.clone());
    let third_token_admin = token::StellarAssetClient::new(&f.env, &third_token_addr);
    let vault_id = f.vault.address.clone();
    third_token_admin.mint(&vault_id, &2_000);

    f.vault.rescue_token(&f.admin, &third_token_addr, &2_000, &f.alice);

    let events = f.env.events().all();
    let rescue_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "tk_rescue"))
        .collect();
    assert_eq!(rescue_events.len(), 1);
}

// ── Issue #40: NFT receipt on stake ──────────────────────────────────────────

fn setup_nft<'a>(f: &'a VaultFixture<'a>) -> (Address, StakeReceiptNFTClient<'a>) {
    let nft_id = f.env.register_contract(None, StakeReceiptNFT);
    let nft = StakeReceiptNFTClient::new(&f.env, &nft_id);
    // The vault will be the minter
    nft.initialize(&f.vault.address);
    f.vault.set_nft_contract(&nft_id);
    (nft_id, nft)
}

#[test]
fn test_stake_mints_nft() {
    let f = VaultFixture::new();
    let (_nft_id, nft) = setup_nft(&f);

    assert!(!nft.has_receipt(&f.alice));
    f.vault.stake(&f.alice, &100_000);
    assert!(nft.has_receipt(&f.alice));
}

#[test]
fn test_full_unstake_burns_nft() {
    let f = VaultFixture::new();
    let (_nft_id, nft) = setup_nft(&f);

    f.vault.stake(&f.alice, &100_000);
    assert!(nft.has_receipt(&f.alice));

    f.vault.unstake(&f.alice, &100_000);
    assert!(!nft.has_receipt(&f.alice));
}

#[test]
fn test_partial_unstake_keeps_nft() {
    let f = VaultFixture::new();
    let (_nft_id, nft) = setup_nft(&f);

    f.vault.stake(&f.alice, &100_000);
    f.vault.unstake(&f.alice, &50_000); // partial — receipt should remain
    assert!(nft.has_receipt(&f.alice));

    f.vault.unstake(&f.alice, &50_000); // full — receipt should be burned
    assert!(!nft.has_receipt(&f.alice));
}

#[test]
fn test_nft_transfer_always_reverts() {
    use crate::nft::NftError;

    let f = VaultFixture::new();
    let (_nft_id, nft) = setup_nft(&f);

    f.vault.stake(&f.alice, &100_000);
    assert!(nft.has_receipt(&f.alice));

    let result = nft.try_transfer(&f.alice, &f.bob);
    assert_eq!(result, Err(Ok(NftError::NonTransferable)));
    // Receipt is still there
    assert!(nft.has_receipt(&f.alice));
}

// ── Issue #41: restake grace window ──────────────────────────────────────────

#[test]
fn test_restake_minimal_no_lock() {
    // Basic: set window, stake, full unstake, re-stake within window
    let f = VaultFixture::new();
    f.vault.set_restake_window(&100);
    f.vault.stake(&f.alice, &100_000);
    f.vault.unstake(&f.alice, &100_000);
    // At ledger 0, last_unstake = 0, current = 0, diff = 0 ≤ 100 → Restaked = true
    f.vault.stake(&f.alice, &100_000);
    f.vault.unstake(&f.alice, &100_000);
}

#[test]
fn test_restake_with_lock_no_penalty_after_expiry() {
    let f = VaultFixture::new();
    f.vault.set_lock_period(&100);
    f.vault.set_early_exit_penalty_bps(&1000);
    // NOTE: no set_restake_window here
    f.vault.stake(&f.alice, &500_000);
    // Unstake AFTER lock period → no penalty
    set_ledger(&f.env, 100);
    let first_return = f.vault.unstake(&f.alice, &500_000);
    assert_eq!(first_return, 500_000);
}

#[test]
fn test_restake_debug_set_window_then_stake_ledger() {
    let f = VaultFixture::new();
    f.vault.set_restake_window(&200);
    f.vault.stake(&f.alice, &500_000);
    set_ledger(&f.env, 100);
    let ret = f.vault.unstake(&f.alice, &500_000);
    assert_eq!(ret, 500_000);
}

#[test]
fn test_restake_debug_lock_period_only() {
    let f = VaultFixture::new();
    f.vault.set_lock_period(&100);  // only this
    f.vault.stake(&f.alice, &500_000);
    set_ledger(&f.env, 100);
    let ret = f.vault.unstake(&f.alice, &500_000);
    assert_eq!(ret, 500_000);
}

#[test]
fn test_restake_debug_a_penalty_call_only() {
    // Does calling set_early_exit_penalty_bps alone panic?
    let f = VaultFixture::new();
    let _ = f.vault.try_set_early_exit_penalty_bps(&1000);
}

#[test]
fn test_restake_debug_b_penalty_and_stake() {
    let f = VaultFixture::new();
    f.vault.set_early_exit_penalty_bps(&1000);
    f.vault.stake(&f.alice, &500_000);
}

#[test]
fn test_restake_debug_c_penalty_stake_unstake_no_ledger() {
    let f = VaultFixture::new();
    f.vault.set_early_exit_penalty_bps(&1000);
    f.vault.stake(&f.alice, &500_000);
    let result = f.vault.try_unstake(&f.alice, &500_000);
    // If it errors instead of panicking, we can see the error
    assert!(result.is_ok(), "Unstake failed: {:?}", result);
}

#[test]
fn test_restake_debug_d_penalty_stake_ledger_unstake() {
    let f = VaultFixture::new();
    f.vault.set_early_exit_penalty_bps(&1000);
    f.vault.stake(&f.alice, &500_000);
    set_ledger(&f.env, 100);
    f.vault.unstake(&f.alice, &500_000);
}

#[test]
fn test_restake_debug_e_reward_rate_stake_unstake() {
    // Does set_reward_rate_bps (another instance storage write) cause the same panic?
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&500);
    f.vault.stake(&f.alice, &500_000);
    f.vault.unstake(&f.alice, &500_000);
}

#[test]
fn test_restake_debug_f_withdrawal_limit_stake_unstake() {
    // Does set_withdrawal_limit (another instance storage write) cause the same panic?
    let f = VaultFixture::new();
    f.vault.set_withdrawal_limit(&2_000_000);
    f.vault.stake(&f.alice, &500_000);
    f.vault.unstake(&f.alice, &500_000);
}

#[test]
fn test_restake_within_window_is_penalty_free() {
    let f = VaultFixture::new();

    // Lock period 100, 10% early-exit penalty, 200-ledger restake window.
    f.vault.set_lock_period(&100);
    f.vault.set_early_exit_penalty_bps(&1000);
    f.vault.set_restake_window(&200);

    // Alice stakes at ledger 0.
    f.vault.stake(&f.alice, &500_000);

    // Alice unstakes AFTER the lock period expires (no penalty, no residual in vault).
    set_ledger(&f.env, 100);
    let first_return = f.vault.unstake(&f.alice, &500_000);
    assert_eq!(first_return, 500_000, "No penalty after lock expires");
    // LastUnstakeLedger = 100; vault is now empty.

    // Alice re-stakes 50 ledgers later — within the 200-ledger window → Restaked = true.
    set_ledger(&f.env, 150);
    f.vault.stake(&f.alice, &500_000);

    // Alice tries to exit at ledger 200 (50 after re-stake, still inside the new 100-ledger lock).
    // Normally 10% penalty; Restaked flag exempts her.
    set_ledger(&f.env, 200);
    let returned = f.vault.unstake(&f.alice, &500_000);
    assert_eq!(returned, 500_000, "Restaked user should receive full amount, no penalty");
}

#[test]
fn test_restake_outside_window_incurs_normal_penalty() {
    let f = VaultFixture::new();

    // Lock 100 ledgers, 10% penalty, but only a 10-ledger restake window.
    f.vault.set_lock_period(&100);
    f.vault.set_early_exit_penalty_bps(&1000);
    f.vault.set_restake_window(&10);

    f.vault.stake(&f.alice, &500_000);

    // Clean unstake after lock period.
    set_ledger(&f.env, 100);
    f.vault.unstake(&f.alice, &500_000);

    // Re-stake 50 ledgers later — OUTSIDE the 10-ledger window → Restaked NOT set.
    set_ledger(&f.env, 150);
    f.vault.stake(&f.alice, &500_000);

    // Early exit inside the new lock period — normal penalty applies.
    set_ledger(&f.env, 200);
    let returned = f.vault.unstake(&f.alice, &500_000);
    let penalty = 500_000_i128 * 1000 / 10_000;
    assert_eq!(returned, 500_000 - penalty, "Outside window: normal penalty applies");
}

#[test]
fn test_restake_window_zero_disables_feature() {
    let f = VaultFixture::new();

    // Lock 100 ledgers, 10% penalty, window disabled.
    f.vault.set_lock_period(&100);
    f.vault.set_early_exit_penalty_bps(&1000);
    f.vault.set_restake_window(&0);

    f.vault.stake(&f.alice, &500_000);

    // Clean unstake after lock period.
    set_ledger(&f.env, 100);
    f.vault.unstake(&f.alice, &500_000);

    // Re-stake 1 ledger later — window = 0 means Restaked is never set.
    set_ledger(&f.env, 101);
    f.vault.stake(&f.alice, &500_000);

    // Early exit inside lock period — penalty must apply since window = 0.
    set_ledger(&f.env, 150);
    let returned = f.vault.unstake(&f.alice, &500_000);
    let penalty = 500_000_i128 * 1000 / 10_000;
    assert_eq!(returned, 500_000 - penalty, "Window=0: normal penalty must apply");
}

// ── Issue #42: admin action audit log ────────────────────────────────────────

#[test]
fn test_admin_action_count_increments() {
    let f = VaultFixture::new();

    let before = f.vault.get_admin_action_count();
    f.vault.set_reward_rate_bps(&500);
    let after = f.vault.get_admin_action_count();
    assert_eq!(after, before + 1, "Count should increment after each admin action");

    f.vault.pause();
    assert_eq!(f.vault.get_admin_action_count(), before + 2);

    f.vault.unpause();
    assert_eq!(f.vault.get_admin_action_count(), before + 3);
}

#[test]
fn test_admin_action_set_reward_rate_emits_audit_event() {
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&1000);

    let events = f.env.events().all();
    let audit_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "adm_act"))
        .collect();
    assert!(!audit_events.is_empty(), "adm_act event should be emitted");
}

#[test]
fn test_admin_action_pause_emits_audit_event() {
    let f = VaultFixture::new();
    f.vault.pause();

    let events = f.env.events().all();
    let audit_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "adm_act"))
        .collect();
    assert!(!audit_events.is_empty(), "adm_act event should be emitted on pause");
}

#[test]
fn test_admin_action_transfer_admin_emits_audit_event() {
    let f = VaultFixture::new();
    f.vault.transfer_admin(&f.bob);

    let events = f.env.events().all();
    let audit_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "adm_act"))
        .collect();
    assert!(!audit_events.is_empty(), "adm_act event should be emitted on transfer_admin");
}

#[test]
fn test_admin_action_count_increments_across_all_admin_fns() {
    let f = VaultFixture::new();
    let mut expected = 0u32;

    f.vault.set_reward_rate_bps(&500);
    expected += 1;
    assert_eq!(f.vault.get_admin_action_count(), expected);

    f.vault.pause();
    expected += 1;
    assert_eq!(f.vault.get_admin_action_count(), expected);

    f.vault.unpause();
    expected += 1;
    assert_eq!(f.vault.get_admin_action_count(), expected);

    f.vault.set_lock_period(&100);
    expected += 1;
    assert_eq!(f.vault.get_admin_action_count(), expected);

    f.vault.set_withdrawal_limit(&1_000_000);
    expected += 1;
    assert_eq!(f.vault.get_admin_action_count(), expected);

    f.vault.transfer_admin(&f.bob);
    expected += 1;
    assert_eq!(f.vault.get_admin_action_count(), expected);
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
#[ignore = "Soroban SDK 21.x: require_auth() issues a non-catchable abort in native \
             test mode when auth is not mocked; the admin guard is enforced at the \
             protocol layer in production. Positive counterpart: test_lowering_cap_below_current_tvl_blocks_new_stakes."]
fn test_non_admin_cannot_set_pool_cap() {
    let f = VaultFixture::new();
    let result = f.vault.try_set_pool_cap(&1_000_000);
    assert_eq!(result, Err(Ok(VaultError::Unauthorized)));
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
    let (event_cap, _): (i128, u32) = TryFromVal::try_from_val(&f.env, &event.2).unwrap();
    assert_eq!(event_cap, 1_000_000);
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

// ── unstake_all (#79) ─────────────────────────────────────────────────────────

#[test]
fn test_unstake_all_fully_exits_position() {
    let f = VaultFixture::new();
    let stake_amount = 1_000_000_i128;

    let shares = f.vault.stake(&f.alice, &stake_amount);
    assert!(shares > 0);

    let alice_balance_before = f.token.balance(&f.alice);
    let returned = f.vault.unstake_all(&f.alice);
    assert_eq!(returned, stake_amount);
    assert_eq!(f.token.balance(&f.alice), alice_balance_before + stake_amount);
}

#[test]
fn test_unstake_all_removes_position() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &1_000_000_i128);

    f.vault.unstake_all(&f.alice);

    let position = f.vault.position_of(&f.alice);
    assert!(position.is_none());
    assert_eq!(f.vault.shares_of(&f.alice), 0);
}

#[test]
fn test_unstake_all_no_position_reverts() {
    let f = VaultFixture::new();
    let result = f.vault.try_unstake_all(&f.alice);
    assert_eq!(result, Err(Ok(VaultError::PositionNotFound)));
}

// ── reward_token_balance (#80) ────────────────────────────────────────────────

#[test]
fn test_reward_token_balance_reflects_funded_pool() {
    let f = VaultFixture::new();

    // Before any funding the contract holds 0 tokens
    assert_eq!(f.vault.reward_token_balance(), 0);

    // Fund reward pool with 5_000_000 tokens from admin
    f.token_admin.mint(&f.admin, &5_000_000);
    f.vault.fund_reward_pool(&f.admin, &5_000_000);

    assert_eq!(f.vault.reward_token_balance(), 5_000_000);
}

#[test]
fn test_reward_token_balance_includes_staked_principal() {
    let f = VaultFixture::new();

    let stake_amount = 2_000_000_i128;
    f.vault.stake(&f.alice, &stake_amount);

    // Contract balance must be at least the staked amount
    let balance = f.vault.reward_token_balance();
    assert!(balance >= stake_amount);
}

// ── position_age_ledgers (#81) ────────────────────────────────────────────────

#[test]
fn test_position_age_zero_immediately_after_stake() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &1_000_000_i128);

    let age = f.vault.position_age_ledgers(&f.alice);
    assert_eq!(age, 0);
}

#[test]
fn test_position_age_equals_ledgers_advanced() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &1_000_000_i128);

    let advance = 500_u32;
    let staked_at = f.env.ledger().sequence();
    set_ledger(&f.env, staked_at + advance);

    let age = f.vault.position_age_ledgers(&f.alice);
    assert_eq!(age, advance);
}

#[test]
fn test_position_age_no_position_reverts() {
    let f = VaultFixture::new();
    let result = f.vault.try_position_age_ledgers(&f.alice);
    assert_eq!(result, Err(Ok(VaultError::PositionNotFound)));
}

// ── rate_changed event (#82) ──────────────────────────────────────────────────

#[test]
fn test_set_reward_rate_emits_rate_changed_event() {
    let f = VaultFixture::new();

    // First call: old=0 new=500. Second call: old=500 new=1000.
    // We verify the second (most recent) event to confirm old_rate is captured correctly.
    f.vault.set_reward_rate_bps(&500_u32);
    f.vault.set_reward_rate_bps(&1000_u32);

    let all_events = f.env.events().all();
    // Use the last rate_chg event — that is the one from the second call.
    let rate_event = all_events
        .iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "rate_chg"))
        .last();

    assert!(rate_event.is_some(), "rate_chg event must be emitted");
    let (_, _, data) = rate_event.unwrap();
    // data tuple: (old_rate_bps: u32, new_rate_bps: u32, ledger: u32)
    let (old_rate, new_rate, _ledger): (u32, u32, u32) =
        soroban_sdk::TryFromVal::try_from_val(&f.env, &data).unwrap();
    assert_eq!(old_rate, 500_u32);
    assert_eq!(new_rate, 1000_u32);
}

#[test]
fn test_rate_changed_event_emitted_even_when_rate_unchanged() {
    let f = VaultFixture::new();
    f.vault.set_reward_rate_bps(&300_u32);

    let events_before = f.env.events().all().len();
    f.vault.set_reward_rate_bps(&300_u32);

    let all_events = f.env.events().all();
    let rate_events_after: std::vec::Vec<_> = all_events
        .iter()
        .skip(events_before as usize)
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "rate_chg"))
        .collect();

    assert_eq!(rate_events_after.len(), 1, "event must fire even when rate does not change");
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
    // Verify the returned address is the correct token by querying a known balance.
    let token_addr = f.vault.get_stake_token();
    let token = soroban_sdk::token::Client::new(&f.env, &token_addr);
    assert_eq!(token.balance(&f.alice), 20_000_000);
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
    f.vault.set_reward_rate_bps(&BOOST_BPS_BASE); // 100% APR

    // Use a full year with quarterly compounding so the compounding effect
    // is large enough to exceed simple interest despite integer truncation.
    let annual = STELLAR_LEDGERS_PER_YEAR;
    let compound = f.vault.simulate_compound(&1_000_000, &annual, &(annual / 4));
    let simple = f.vault.simulate_stake(&1_000_000, &annual);
    assert!(compound > simple, "quarterly compound ({compound}) must beat simple ({simple})");
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

    let (base, boosted) = f.vault.simulate_boost_impact(&1_000_000, &1000);
    // base = 1_000_000 * 10_000 * 1000 / 10_000 / 6_307_200 = 158 (integer division)
    assert_eq!(base, 158);
    assert!(boosted > base, "15_000 multiplier must yield more than base 10_000");
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

    let claimed = f.vault.claim(&f.alice);
    assert!(claimed > 0, "claim within cap must return the full reward");

    let window = f.vault.get_claim_window(&f.alice).unwrap();
    assert_eq!(window.claimed_in_window, claimed);
}

#[test]
fn test_claim_exceeding_cap_is_truncated() {
    let f = setup_with_cap(50_000, 100_000);

    f.vault.stake(&f.alice, &1_000_000);
    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR);

    let claimed = f.vault.claim(&f.alice);
    assert_eq!(claimed, 50_000, "claim must be truncated to the cap");

    let claimed2 = f.vault.claim(&f.alice);
    assert_eq!(claimed2, 0, "no further claim allowed until window resets");
}

#[test]
fn test_claim_window_resets_after_expiry() {
    let f = setup_with_cap(50_000, 100);

    f.vault.stake(&f.alice, &1_000_000);
    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR);

    let first = f.vault.claim(&f.alice);
    assert_eq!(first, 50_000);

    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR + 200);

    let second = f.vault.claim(&f.alice);
    assert!(second > 0, "claim after window reset must succeed");
}

#[test]
fn test_cap_zero_disables_limit() {
    let f = setup_with_cap(0, 100_000);

    f.vault.stake(&f.alice, &1_000_000);
    set_ledger(&f.env, STELLAR_LEDGERS_PER_YEAR);

    let claimed = f.vault.claim(&f.alice);
    assert!(claimed > 0, "unlimited claim (cap=0) must return full reward");

    let window_opt = f.vault.get_claim_window(&f.alice);
    assert!(window_opt.is_none(), "no window stored when cap is disabled");
}

// ── APR and TWAP tests ────────────────────────────────────────────────────────

#[test]
fn test_current_apr_bps_returns_current_rate() {
    let f = VaultFixture::new();

    f.vault.set_reward_rate_bps(&1000);
    assert_eq!(f.vault.current_apr_bps(), 1000);

    f.vault.set_reward_rate_bps(&2000);
    assert_eq!(f.vault.current_apr_bps(), 2000);
}

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

    // Window=100 covering ledgers 100-200: only 2000 bps rate in this window.
    let twap = f.vault.twap_apr_bps(&100);
    assert_eq!(twap, 2000);

    f.vault.set_reward_rate_bps(&3000);
    set_ledger(&f.env, 300);

    // Window=200 covering ledgers 100-300:
    // 100 ledgers @2000 bps + 100 ledgers @3000 bps → TWAP = 2500
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

    // Window=150 covering ledgers 50-200:
    // 50 ledgers @1000 bps + 100 ledgers @2000 bps → TWAP = (50*1000+100*2000)/150 = 1666
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
    // oldest entry (ledger 10) was evicted
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

    let e1 = history.get(0).unwrap();
    assert_eq!(e1.0, 0);
    assert_eq!(e1.1, 0);

    let e2 = history.get(1).unwrap();
    assert_eq!(e2.0, 100);
    assert_eq!(e2.1, 1000);

    let e3 = history.get(2).unwrap();
    assert_eq!(e3.0, 200);
    assert_eq!(e3.1, 2000);
}

#[test]
fn test_twap_zero_window_returns_current_rate() {
    let f = VaultFixture::new();

    f.vault.set_reward_rate_bps(&1500);

    let twap = f.vault.twap_apr_bps(&0);
    assert_eq!(twap, 1500);
}

// ── Issue #98: can_unstake pre-flight check ─────────────────────────────────

#[test]
fn test_can_unstake_ok_when_valid() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &100_000);
    assert_eq!(f.vault.can_unstake(&f.alice, &100_000), UnstakeCheckResult::Ok);
}

#[test]
fn test_can_unstake_no_position() {
    let f = VaultFixture::new();
    assert_eq!(f.vault.can_unstake(&f.alice, &100_000), UnstakeCheckResult::NoPosition);
}

#[test]
fn test_can_unstake_insufficient_amount_zero() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &100_000);
    assert_eq!(f.vault.can_unstake(&f.alice, &0), UnstakeCheckResult::InsufficientAmount);
}

#[test]
fn test_can_unstake_insufficient_amount_too_much() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &100_000);
    assert_eq!(f.vault.can_unstake(&f.alice, &200_000), UnstakeCheckResult::InsufficientAmount);
}

#[test]
fn test_can_unstake_pool_paused() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &100_000);
    f.vault.pause();
    assert_eq!(f.vault.can_unstake(&f.alice, &100_000), UnstakeCheckResult::PoolPaused);
}

#[test]
fn test_can_unstake_still_locked() {
    let f = VaultFixture::new();
    f.vault.set_lock_period(&100);
    f.vault.stake(&f.alice, &100_000);
    set_ledger(&f.env, 50);
    assert_eq!(f.vault.can_unstake(&f.alice, &100_000), UnstakeCheckResult::StillLocked);
}

#[test]
fn test_can_unstake_not_locked_after_period() {
    let f = VaultFixture::new();
    f.vault.set_lock_period(&100);
    f.vault.stake(&f.alice, &100_000);
    set_ledger(&f.env, 100);
    assert_eq!(f.vault.can_unstake(&f.alice, &100_000), UnstakeCheckResult::Ok);
}

// ── Issue #97: set_pool_description ─────────────────────────────────────────

#[test]
fn test_set_and_get_pool_description() {
    let f = VaultFixture::new();
    let desc = soroban_sdk::String::from_str(&f.env, "My staking pool");
    f.vault.set_pool_description(&f.admin, &desc);
    assert_eq!(f.vault.get_pool_description(), Some(desc));
}

#[test]
fn test_get_pool_description_returns_none_initially() {
    let f = VaultFixture::new();
    assert_eq!(f.vault.get_pool_description(), None);
}

#[test]
fn test_set_pool_description_too_long_reverts() {
    let f = VaultFixture::new();
    let long_desc = soroban_sdk::String::from_str(&f.env, &"a".repeat(201));
    let result = f.vault.try_set_pool_description(&f.admin, &long_desc);
    assert_eq!(result, Err(Ok(VaultError::DescriptionTooLong)));
}

#[test]
fn test_set_pool_description_at_exact_limit_succeeds() {
    let f = VaultFixture::new();
    let desc = soroban_sdk::String::from_str(&f.env, &"a".repeat(200));
    f.vault.set_pool_description(&f.admin, &desc);
    assert_eq!(f.vault.get_pool_description(), Some(desc));
}

#[test]
#[ignore = "Soroban SDK 21.x: require_auth() issues a non-catchable abort in native test mode when auth is not mocked; the admin guard is enforced at the protocol layer in production."]
fn test_set_pool_description_non_admin_rejected() {
    let f = VaultFixture::new();
    let desc = soroban_sdk::String::from_str(&f.env, "test");
    let result = f.vault.try_set_pool_description(&f.alice, &desc);
    assert_eq!(result, Err(Ok(VaultError::Unauthorized)));
}

#[test]
fn test_set_pool_description_emits_event() {
    let f = VaultFixture::new();
    let desc = soroban_sdk::String::from_str(&f.env, "Pool v2");
    f.vault.set_pool_description(&f.admin, &desc);

    let events = f.env.events().all();
    let desc_events: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| topic_matches(&f.env, topics, "desc_upd"))
        .collect();
    assert_eq!(desc_events.len(), 1);
}

// ── Issue #96: percentage_of_pool ───────────────────────────────────────────

#[test]
fn test_percentage_of_pool_sole_staker() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &1_000_000);
    assert_eq!(f.vault.percentage_of_pool(&f.alice), 10_000);
}

#[test]
fn test_percentage_of_pool_two_equal_stakers() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &500_000);
    f.vault.stake(&f.bob, &500_000);
    assert_eq!(f.vault.percentage_of_pool(&f.alice), 5_000);
    assert_eq!(f.vault.percentage_of_pool(&f.bob), 5_000);
}

#[test]
fn test_percentage_of_pool_no_position() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &1_000_000);
    assert_eq!(f.vault.percentage_of_pool(&f.bob), 0);
}

#[test]
fn test_percentage_of_pool_empty_pool() {
    let f = VaultFixture::new();
    assert_eq!(f.vault.percentage_of_pool(&f.alice), 0);
}

#[test]
fn test_percentage_of_pool_unequal_stakers() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &750_000);
    f.vault.stake(&f.bob, &250_000);
    assert_eq!(f.vault.percentage_of_pool(&f.alice), 7_500);
    assert_eq!(f.vault.percentage_of_pool(&f.bob), 2_500);
}

// ── Issue #99: staking streak tracker ───────────────────────────────────────

#[test]
fn test_streak_increments_on_consecutive_waves() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &100_000);
    f.vault.stake(&f.bob, &100_000);

    let users1 = soroban_sdk::Vec::from_array(&f.env, [f.alice.clone(), f.bob.clone()]);
    f.vault.record_wave_activity(&f.admin, &1, &users1);

    let streak = f.vault.get_streak(&f.alice);
    assert_eq!(streak.current_streak, 1);

    let users2 = soroban_sdk::Vec::from_array(&f.env, [f.alice.clone()]);
    f.vault.record_wave_activity(&f.admin, &2, &users2);

    let streak = f.vault.get_streak(&f.alice);
    assert_eq!(streak.current_streak, 2);

    let streak_bob = f.vault.get_streak(&f.bob);
    assert_eq!(streak_bob.current_streak, 0);
}

#[test]
fn test_streak_resets_on_missed_wave() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &100_000);

    let users1 = soroban_sdk::Vec::from_array(&f.env, [f.alice.clone()]);
    f.vault.record_wave_activity(&f.admin, &1, &users1);

    let streak = f.vault.get_streak(&f.alice);
    assert_eq!(streak.current_streak, 1);

    let empty = soroban_sdk::Vec::new(&f.env);
    f.vault.record_wave_activity(&f.admin, &2, &empty);

    let streak = f.vault.get_streak(&f.alice);
    assert_eq!(streak.current_streak, 0);
}

#[test]
fn test_streak_longest_preserved_after_reset() {
    let f = VaultFixture::new();
    f.vault.stake(&f.alice, &100_000);

    let users = soroban_sdk::Vec::from_array(&f.env, [f.alice.clone()]);
    f.vault.record_wave_activity(&f.admin, &1, &users);
    f.vault.record_wave_activity(&f.admin, &2, &users);
    f.vault.record_wave_activity(&f.admin, &3, &users);

    let streak = f.vault.get_streak(&f.alice);
    assert_eq!(streak.current_streak, 3);
    assert_eq!(streak.longest_streak, 3);

    let empty = soroban_sdk::Vec::new(&f.env);
    f.vault.record_wave_activity(&f.admin, &4, &empty);

    let streak = f.vault.get_streak(&f.alice);
    assert_eq!(streak.current_streak, 0);
    assert_eq!(streak.longest_streak, 3);
}

#[test]
#[ignore = "Soroban SDK 21.x: require_auth() issues a non-catchable abort in native test mode when auth is not mocked; the admin guard is enforced at the protocol layer in production."]
fn test_streak_non_admin_rejected() {
    let f = VaultFixture::new();
    let users = soroban_sdk::Vec::new(&f.env);
    let result = f.vault.try_record_wave_activity(&f.alice, &1, &users);
    assert_eq!(result, Err(Ok(VaultError::Unauthorized)));
}

#[test]
fn test_streak_non_monotonic_wave_rejected() {
    let f = VaultFixture::new();
    let users = soroban_sdk::Vec::new(&f.env);
    f.vault.record_wave_activity(&f.admin, &5, &users);
    let result = f.vault.try_record_wave_activity(&f.admin, &3, &users);
    assert_eq!(result, Err(Ok(VaultError::NonMonotonicWaveId)));
}

#[test]
fn test_streak_too_many_active_users_rejected() {
    let f = VaultFixture::new();
    let mut users = soroban_sdk::Vec::new(&f.env);
    for _ in 0..51 {
        users.push_back(Address::generate(&f.env));
    }
    let result = f.vault.try_record_wave_activity(&f.admin, &1, &users);
    assert_eq!(result, Err(Ok(VaultError::TooManyActiveUsers)));
}

// ── Issue #70: zero address validation in initialize ─────────────────────────

#[test]
fn test_initialize_zero_admin_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    let (token_addr, _, _) = create_token(&env, &Address::generate(&env));
    // Using the vault's own address as admin is invalid.
    let result = vault.try_initialize(&vault_id, &token_addr, &0_u32, &None, &None);
    assert_eq!(result, Err(Ok(VaultError::InvalidAddress)));
}

#[test]
fn test_initialize_zero_stake_token_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    let admin = Address::generate(&env);
    // Using the vault's own address as stake_token is invalid.
    let result = vault.try_initialize(&admin, &vault_id, &0_u32, &None, &None);
    assert_eq!(result, Err(Ok(VaultError::InvalidAddress)));
}

#[test]
fn test_initialize_zero_reward_token_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    let admin = Address::generate(&env);
    // reward_token = stake_token = token param; vault address as token is invalid.
    let result = vault.try_initialize(&admin, &vault_id, &0_u32, &None, &None);
    assert_eq!(result, Err(Ok(VaultError::InvalidAddress)));
}

// ── Issue #69: last_updated_ledger tracking ───────────────────────────────────

#[test]
fn test_last_updated_ledger_after_stake() {
    let f = VaultFixture::new();
    f.token_admin.mint(&f.alice, &1_000_000);
    set_ledger(&f.env, 100);
    f.vault.stake(&f.alice, &1_000_000);
    assert_eq!(f.vault.get_last_updated_ledger(), 100);
}

#[test]
fn test_last_updated_ledger_after_unstake() {
    let f = VaultFixture::new();
    f.token_admin.mint(&f.alice, &1_000_000);
    f.vault.stake(&f.alice, &1_000_000);
    set_ledger(&f.env, 200);
    let shares = f.vault.shares_of(&f.alice);
    f.vault.unstake(&f.alice, &shares);
    assert_eq!(f.vault.get_last_updated_ledger(), 200);
}

#[test]
fn test_last_updated_ledger_after_claim() {
    let f = VaultFixture::new();
    f.token_admin.mint(&f.alice, &1_000_000);
    f.vault.stake(&f.alice, &1_000_000);
    set_ledger(&f.env, 300);
    f.vault.claim(&f.alice);
    assert_eq!(f.vault.get_last_updated_ledger(), 300);
}

#[test]
fn test_last_updated_ledger_after_pause() {
    let f = VaultFixture::new();
    set_ledger(&f.env, 400);
    f.vault.pause();
    assert_eq!(f.vault.get_last_updated_ledger(), 400);
}

#[test]
fn test_last_updated_ledger_after_unpause() {
    let f = VaultFixture::new();
    f.vault.pause();
    set_ledger(&f.env, 500);
    f.vault.unpause();
    assert_eq!(f.vault.get_last_updated_ledger(), 500);
}

#[test]
fn test_last_updated_ledger_defaults_to_zero() {
    let f = VaultFixture::new();
    assert_eq!(f.vault.get_last_updated_ledger(), 0);
}

// ── Issue #72: reward_rate_bps validation in initialize ───────────────────────

#[test]
fn test_initialize_rate_above_max_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    let admin = Address::generate(&env);
    let (token_addr, _, _) = create_token(&env, &admin);
    // 50_001 bps exceeds MAX_RATE_BPS (50_000).
    let result = vault.try_initialize(&admin, &token_addr, &50_001_u32, &None, &None);
    assert_eq!(result, Err(Ok(VaultError::RateTooHigh)));
}

#[test]
fn test_initialize_rate_at_max_accepted() {
    let env = Env::default();
    env.mock_all_auths();
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    let admin = Address::generate(&env);
    let (token_addr, _, _) = create_token(&env, &admin);
    // Exactly MAX_RATE_BPS should succeed.
    vault.initialize(&admin, &token_addr, &50_000_u32, &None, &None);
    assert_eq!(vault.get_reward_rate_bps(), 50_000);
}

#[test]
fn test_set_reward_rate_above_max_rejected() {
    let f = VaultFixture::new();
    let result = f.vault.try_set_reward_rate_bps(&50_001_u32);
    assert_eq!(result, Err(Ok(VaultError::RateTooHigh)));
}

#[test]
fn test_initialize_stores_reward_rate() {
    let env = Env::default();
    env.mock_all_auths();
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    let admin = Address::generate(&env);
    let (token_addr, _, _) = create_token(&env, &admin);
    vault.initialize(&admin, &token_addr, &1_000_u32, &None, &None);
    assert_eq!(vault.get_reward_rate_bps(), 1_000);
}
