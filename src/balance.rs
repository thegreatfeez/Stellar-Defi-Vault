use crate::storage::{ClaimWindow, DataKey};
use soroban_sdk::{symbol_short, Address, Env, Symbol, Vec};

pub fn get_shares(env: &Env, user: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::ShareBalance(user.clone()))
        .unwrap_or(0)
}

pub fn set_shares(env: &Env, user: &Address, amount: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::ShareBalance(user.clone()), &amount);
}

pub fn get_total_shares(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::TotalShares)
        .unwrap_or(0)
}

pub fn set_total_shares(env: &Env, total: i128) {
    env.storage().instance().set(&DataKey::TotalShares, &total);
}

pub fn get_total_deposited(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::TotalDeposited)
        .unwrap_or(0)
}

pub fn set_total_deposited(env: &Env, total: i128) {
    env.storage()
        .instance()
        .set(&DataKey::TotalDeposited, &total);
}

pub fn get_min_stake(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::MinStake)
        .unwrap_or(0)
}

pub fn set_min_stake(env: &Env, amount: i128) {
    env.storage().instance().set(&DataKey::MinStake, &amount);
}

pub fn get_reward_rate_bps(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::RewardRateBps)
        .unwrap_or(0)
}

pub fn set_reward_rate_bps(env: &Env, rate_bps: u32) {
    env.storage()
        .instance()
        .set(&DataKey::RewardRateBps, &rate_bps);
}

pub fn get_rate_history(env: &Env) -> Vec<(u32, u32)> {
    env.storage()
        .instance()
        .get(&DataKey::RateHistory)
        .unwrap_or(Vec::new(env))
}

pub fn set_rate_history(env: &Env, history: &Vec<(u32, u32)>) {
    env.storage()
        .instance()
        .set(&DataKey::RateHistory, history);
}

pub const MAX_RATE_HISTORY_ENTRIES: u32 = 50;

/// Maximum allowed reward rate in basis points (500% APR). Issue #72.
pub const MAX_RATE_BPS: u32 = 50_000;

pub fn get_reward_pool_balance(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::RewardPoolBalance)
        .unwrap_or(0)
}

pub fn set_reward_pool_balance(env: &Env, balance: i128) {
    env.storage()
        .instance()
        .set(&DataKey::RewardPoolBalance, &balance);
}

pub fn get_withdrawal_limit(env: &Env) -> Option<i128> {
    env.storage().instance().get(&DataKey::WithdrawalLimit)
}

pub fn set_withdrawal_limit(env: &Env, limit: i128) {
    env.storage()
        .instance()
        .set(&DataKey::WithdrawalLimit, &limit);
}

pub fn get_pool_cap(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::PoolCap)
        .unwrap_or(0)
}

pub fn set_pool_cap(env: &Env, cap: i128) {
    env.storage().instance().set(&DataKey::PoolCap, &cap);
}

pub fn get_unstake_fee_bps(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::UnstakeFeeBps)
        .unwrap_or(0)
}

pub fn set_unstake_fee_bps(env: &Env, bps: u32) {
    env.storage().instance().set(&DataKey::UnstakeFeeBps, &bps);
}

pub fn get_reward_checkpoint_ledger(env: &Env, user: &Address) -> Option<u32> {
    env.storage()
        .persistent()
        .get(&DataKey::RewardCheckpointLedger(user.clone()))
}

pub fn set_reward_checkpoint_ledger(env: &Env, user: &Address, ledger: u32) {
    env.storage()
        .persistent()
        .set(&DataKey::RewardCheckpointLedger(user.clone()), &ledger);
}

pub fn set_last_claim_ledger(env: &Env, user: &Address, ledger: u32) {
    env.storage()
        .persistent()
        .set(&DataKey::LastClaimLedger(user.clone()), &ledger);
}

pub fn get_accrued_reward(env: &Env, user: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::AccruedReward(user.clone()))
        .unwrap_or(0)
}

pub fn set_accrued_reward(env: &Env, user: &Address, amount: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::AccruedReward(user.clone()), &amount);
}

pub fn get_stake_history(env: &Env, user: &Address) -> Option<Vec<(u32, i128)>> {
    env.storage()
        .persistent()
        .get(&DataKey::StakeHistory(user.clone()))
}

pub fn set_stake_history(env: &Env, user: &Address, history: &Vec<(u32, i128)>) {
    env.storage()
        .persistent()
        .set(&DataKey::StakeHistory(user.clone()), history);
}

pub fn get_boost_schedule(env: &Env) -> Option<Vec<(u32, u32)>> {
    env.storage().instance().get(&DataKey::BoostSchedule)
}

pub fn set_boost_schedule(env: &Env, tiers: &Vec<(u32, u32)>) {
    env.storage().instance().set(&DataKey::BoostSchedule, tiers);
}

pub fn get_total_stakers(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::TotalStakers)
        .unwrap_or(0)
}

pub fn set_total_stakers(env: &Env, count: u32) {
    env.storage()
        .instance()
        .set(&DataKey::TotalStakers, &count);
}

pub fn get_total_rewards_paid(env: &Env) -> i128 {
    env.storage()
        .instance()
        .get(&DataKey::TotalRewardsPaid)
        .unwrap_or(0)
}

pub fn set_total_rewards_paid(env: &Env, amount: i128) {
    env.storage()
        .instance()
        .set(&DataKey::TotalRewardsPaid, &amount);
}

pub fn get_last_claim_ledger(env: &Env, user: &Address) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::LastClaimLedger(user.clone()))
        .unwrap_or(0)
}

pub fn get_delegate(env: &Env, user: &Address) -> Option<Address> {
    env.storage()
        .persistent()
        .get(&DataKey::Delegate(user.clone()))
}

pub fn set_delegate(env: &Env, user: &Address, delegate: &Address) {
    env.storage()
        .persistent()
        .set(&DataKey::Delegate(user.clone()), delegate);
}

pub fn remove_delegate(env: &Env, user: &Address) {
    env.storage()
        .persistent()
        .remove(&DataKey::Delegate(user.clone()));
}

// ── Issue #39: reward token ───────────────────────────────────────────────────

pub fn get_reward_token(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::RewardToken)
}

pub fn set_reward_token(env: &Env, token: &Address) {
    env.storage().instance().set(&DataKey::RewardToken, token);
}

// ── Issue #40: NFT contract ───────────────────────────────────────────────────

pub fn get_nft_contract(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::NftContract)
}

pub fn set_nft_contract(env: &Env, nft: &Address) {
    env.storage().instance().set(&DataKey::NftContract, nft);
}

// ── Issue #41: restake grace window ──────────────────────────────────────────

pub fn get_restake_window(env: &Env) -> u32 {
    env.storage().instance().get(&DataKey::RestakeWindow).unwrap_or(0)
}

pub fn set_restake_window(env: &Env, ledgers: u32) {
    env.storage().instance().set(&DataKey::RestakeWindow, &ledgers);
}

pub fn get_last_unstake_ledger(env: &Env, user: &Address) -> Option<u32> {
    env.storage().persistent().get(&DataKey::LastUnstakeLedger(user.clone()))
}

pub fn set_last_unstake_ledger(env: &Env, user: &Address, ledger: u32) {
    env.storage().persistent().set(&DataKey::LastUnstakeLedger(user.clone()), &ledger);
}

pub fn is_restaked(env: &Env, user: &Address) -> bool {
    env.storage().persistent().get(&DataKey::Restaked(user.clone())).unwrap_or(false)
}

pub fn set_restaked(env: &Env, user: &Address, value: bool) {
    env.storage().persistent().set(&DataKey::Restaked(user.clone()), &value);
}

pub fn remove_restaked(env: &Env, user: &Address) {
    let key = DataKey::Restaked(user.clone());
    if env.storage().persistent().has(&key) {
        env.storage().persistent().remove(&key);
    }
}

// ── Issue #42: admin action count ────────────────────────────────────────────

pub fn get_admin_action_count(env: &Env) -> u32 {
    env.storage().instance().get(&DataKey::AdminActionCount).unwrap_or(0)
}

pub fn increment_admin_action_count(env: &Env) {
    let count = get_admin_action_count(env);
    env.storage().instance().set(&DataKey::AdminActionCount, &(count + 1));
}

// ── Claim cap (issue #78) ─────────────────────────────────────────────────────

pub fn get_claim_cap(env: &Env) -> i128 {
    env.storage().instance().get(&DataKey::ClaimCap).unwrap_or(0)
}

pub fn set_claim_cap(env: &Env, cap: i128) {
    env.storage().instance().set(&DataKey::ClaimCap, &cap);
}

pub fn get_claim_cap_window(env: &Env) -> u32 {
    env.storage().instance().get(&DataKey::ClaimCapWindow).unwrap_or(0)
}

pub fn set_claim_cap_window(env: &Env, window_ledgers: u32) {
    env.storage()
        .instance()
        .set(&DataKey::ClaimCapWindow, &window_ledgers);
}

pub fn get_user_claim_window(env: &Env, user: &Address) -> Option<ClaimWindow> {
    env.storage()
        .persistent()
        .get(&DataKey::UserClaimWindow(user.clone()))
}

pub fn set_user_claim_window(env: &Env, user: &Address, window: &ClaimWindow) {
    env.storage()
        .persistent()
        .set(&DataKey::UserClaimWindow(user.clone()), window);
}

// ── Token decimals (reward normalization) ─────────────────────────────────────

/// Default decimal precision for Stellar tokens. Most tokens use 7 places,
/// but this is only a fallback for pools initialized without explicit values.
pub const DEFAULT_TOKEN_DECIMALS: u32 = 7;

pub fn get_stake_decimals(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::StakeDecimals)
        .unwrap_or(DEFAULT_TOKEN_DECIMALS)
}

pub fn set_stake_decimals(env: &Env, decimals: u32) {
    env.storage()
        .instance()
        .set(&DataKey::StakeDecimals, &decimals);
}

pub fn get_reward_decimals(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::RewardDecimals)
        .unwrap_or(DEFAULT_TOKEN_DECIMALS)
}

pub fn set_reward_decimals(env: &Env, decimals: u32) {
    env.storage()
        .instance()
        .set(&DataKey::RewardDecimals, &decimals);
}

// ── All-stakers list (issue #95) ──────────────────────────────────────────────

pub fn get_all_stakers(env: &Env) -> Vec<Address> {
    env.storage()
        .instance()
        .get(&DataKey::AllStakers)
        .unwrap_or(Vec::new(env))
}

pub fn set_all_stakers(env: &Env, stakers: &Vec<Address>) {
    env.storage().instance().set(&DataKey::AllStakers, stakers);
}

// ── Share math ────────────────────────────────────────────────────────────────

/// Convert a deposit amount to shares using current vault ratio.
/// First deposit: 1:1. Subsequent: proportional to existing pool.
pub fn amount_to_shares(total_shares: i128, total_deposited: i128, amount: i128) -> Option<i128> {
    if total_shares == 0 || total_deposited == 0 {
        Some(amount)
    } else {
        amount
            .checked_mul(total_shares)?
            .checked_div(total_deposited)
    }
}

/// Convert shares to the underlying token amount.
pub fn shares_to_amount(total_shares: i128, total_deposited: i128, shares: i128) -> Option<i128> {
    if total_shares == 0 {
        Some(0)
    } else {
        shares
            .checked_mul(total_deposited)?
            .checked_div(total_shares)
    }
}

pub fn get_reward_remainder(env: &Env, user: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::RewardRemainder(user.clone()))
        .unwrap_or(0)
}

pub fn set_reward_remainder(env: &Env, user: &Address, amount: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::RewardRemainder(user.clone()), &amount);
}

// ── Issue #69: last updated ledger ───────────────────────────────────────────
// Uses symbol_short! to avoid pushing DataKey over the contracttype variant limit.

pub fn get_last_updated_ledger(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&symbol_short!("lst_upd"))
        .unwrap_or(0)
}

pub fn set_last_updated_ledger(env: &Env, ledger: u32) {
    env.storage()
        .instance()
        .set(&symbol_short!("lst_upd"), &ledger);
}

// ── Issue #97: pool description ──────────────────────────────────────────────

pub fn get_pool_description(env: &Env) -> Option<soroban_sdk::String> {
    env.storage().instance().get(&symbol_short!("pool_desc"))
}

pub fn set_pool_description(env: &Env, description: &soroban_sdk::String) {
    env.storage().instance().set(&symbol_short!("pool_desc"), description);
}

// ── Issue #99: staking streak ────────────────────────────────────────────────

pub fn get_last_recorded_wave(env: &Env) -> Option<u32> {
    env.storage().instance().get(&symbol_short!("last_wave"))
}

pub fn set_last_recorded_wave(env: &Env, wave_id: u32) {
    env.storage().instance().set(&symbol_short!("last_wave"), &wave_id);
}

pub fn get_user_streak(env: &Env, user: &Address) -> Option<crate::storage::StakeStreak> {
    let key = (Symbol::new(env, "strk"), user.clone());
    env.storage().persistent().get(&key)
}

pub fn set_user_streak(env: &Env, user: &Address, streak: &crate::storage::StakeStreak) {
    let key = (Symbol::new(env, "strk"), user.clone());
    env.storage().persistent().set(&key, streak);
}

