use crate::storage::DataKey;
use soroban_sdk::{Address, Env, Vec};

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
