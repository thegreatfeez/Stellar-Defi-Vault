use soroban_sdk::{contract, contractimpl, token, Address, Env, String, Vec};

use crate::{
    admin, balance, errors::VaultError, events,
    storage::{DataKey, PoolStats, StakePosition, UnbondingPosition, UserStats},
};

pub(crate) const CONTRACT_VERSION: &str = "0.1.0";
pub(crate) const BOOST_BPS_BASE: u32 = 10_000;
pub(crate) const MAX_BOOST_TIERS: u32 = 5;
pub(crate) const MAX_HISTORY_SNAPSHOTS: u32 = 100;
pub(crate) const STELLAR_LEDGERS_PER_YEAR: u32 = 6_307_200;

#[contract]
pub struct VaultContract;

#[contractimpl]
impl VaultContract {
    /// Initialize the vault with an admin and the token it accepts.
    pub fn initialize(env: Env, admin: Address, token: Address) -> Result<(), VaultError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(VaultError::AlreadyInitialized);
        }

        admin::set_admin(&env, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::Paused, &false);
        // By default, set the slash treasury to the admin address. Can be updated by admin later.
        env.storage().instance().set(&DataKey::SlashTreasury, &admin);
        Ok(())
    }

    /// Deposit `amount` of the vault token. Returns shares minted to caller.
    pub fn deposit(env: Env, depositor: Address, amount: i128) -> Result<i128, VaultError> {
        Self::do_stake(&env, &depositor, amount)
    }

    /// Stake `amount` of the vault token. This is an alias for `deposit`.
    pub fn stake(env: Env, staker: Address, amount: i128) -> Result<i128, VaultError> {
        Self::do_stake(&env, &staker, amount)
    }

    /// Withdraw by burning `shares`. Returns underlying token amount returned.
    pub fn withdraw(env: Env, withdrawer: Address, shares: i128) -> Result<i128, VaultError> {
        Self::do_unstake(&env, &withdrawer, shares)
    }

    /// Unstake by burning `shares`. This is an alias for `withdraw`.
    pub fn unstake(env: Env, staker: Address, shares: i128) -> Result<i128, VaultError> {
        Self::do_unstake(&env, &staker, shares)
    }

    /// Convenience function to fully exit a staking position in one call.
    ///
    /// Reads the caller's entire share balance and unstakes it, auto-claiming
    /// any pending rewards first (same behaviour as `unstake`).
    /// Returns the total token amount returned to the user.
    /// Reverts with `PositionNotFound` when the user has no active position.
    pub fn unstake_all(env: Env, user: Address) -> Result<i128, VaultError> {
        let shares = balance::get_shares(&env, &user);
        if shares == 0 {
            return Err(VaultError::PositionNotFound);
        }
        Self::do_unstake(&env, &user, shares)
    }

    /// Claim accumulated staking rewards without changing the staked position.
    pub fn claim(env: Env, staker: Address) -> Result<i128, VaultError> {
        staker.require_auth();
        let current_shares = balance::get_shares(&env, &staker);
        Self::accrue_rewards(&env, &staker, current_shares)?;

        let reward = balance::get_accrued_reward(&env, &staker);
        if reward == 0 {
            balance::set_last_claim_ledger(&env, &staker, env.ledger().sequence());
            return Ok(0);
        }

        let reward_pool = balance::get_reward_pool_balance(&env);
        if reward_pool < reward {
            return Err(VaultError::InsufficientRewardPool);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;

        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&env.current_contract_address(), &staker, &reward);

        balance::set_reward_pool_balance(&env, reward_pool - reward);
        balance::set_accrued_reward(&env, &staker, 0);
        balance::set_last_claim_ledger(&env, &staker, env.ledger().sequence());

        let paid = balance::get_total_rewards_paid(&env);
        balance::set_total_rewards_paid(&env, paid + reward);

        Ok(reward)
    }

    /// Query share balance of a user.
    pub fn shares_of(env: Env, user: Address) -> i128 {
        balance::get_shares(&env, &user)
    }

    /// Read-only query for the current admin address.
    pub fn get_admin(env: Env) -> Result<Address, VaultError> {
        admin::get_admin(&env)
    }

    /// Read-only query for the deployed contract version.
    pub fn get_version(env: Env) -> String {
        String::from_str(&env, CONTRACT_VERSION)
    }

    /// Returns true when the pool is paused, false otherwise.
    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    /// Read-only query for the caller's active stake position.
    ///
    /// Returns the current `StakePosition` for an active account, including the
    /// position amount, `staked_at_ledger`, and `last_claim_ledger`.
    /// Returns `None` when the user has no active position.
    pub fn position_of(env: Env, user: Address) -> Result<Option<StakePosition>, VaultError> {
        Self::build_position(&env, &user)
    }

    /// Read-only governance weight using the user's current staked shares.
    pub fn current_vote_weight(env: Env, user: Address) -> Result<i128, VaultError> {
        let _ = admin::get_admin(&env)?;
        Ok(balance::get_shares(&env, &user))
    }

    /// Total staked shares across all users.
    pub fn total_staked(env: Env) -> Result<i128, VaultError> {
        let _ = admin::get_admin(&env)?;
        Ok(balance::get_total_shares(&env))
    }

    /// Pool-wide governance vote weight.
    pub fn total_vote_weight(env: Env) -> Result<i128, VaultError> {
        Self::total_staked(env)
    }

    /// Historical governance vote weight at a specific ledger.
    pub fn vote_weight_at(env: Env, user: Address, ledger: u32) -> Result<i128, VaultError> {
        let _ = admin::get_admin(&env)?;
        let history = balance::get_stake_history(&env, &user).unwrap_or(Vec::new(&env));
        let mut weight = 0;
        let mut index = 0;

        while index < history.len() {
            let (snapshot_ledger, snapshot_amount) = history.get(index).unwrap();
            if snapshot_ledger > ledger {
                break;
            }
            weight = snapshot_amount;
            index += 1;
        }

        Ok(weight)
    }

    /// Query how many tokens a given share count is worth right now.
    pub fn preview_redeem(env: Env, shares: i128) -> Result<i128, VaultError> {
        let total_shares = balance::get_total_shares(&env);
        let total_deposited = balance::get_total_deposited(&env);
        balance::shares_to_amount(total_shares, total_deposited, shares)
            .ok_or(VaultError::ArithmeticError)
    }

    /// Read-only query for pending staking rewards.
    pub fn calc_pending_reward(env: Env, user: Address) -> Result<i128, VaultError> {
        let _ = admin::get_admin(&env)?;
        Self::pending_reward(&env, &user)
    }

    /// Query total shares and deposited amounts.
    pub fn vault_state(env: Env) -> Result<(i128, i128), VaultError> {
        let _ = admin::get_admin(&env)?;
        Ok((
            balance::get_total_shares(&env),
            balance::get_total_deposited(&env),
        ))
    }

    /// Pause all deposits and withdrawals (admin only).
    pub fn pause(env: Env) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &true);
        let admin = admin::get_admin(&env)?;
        events::paused(&env, &admin);
        Ok(())
    }

    /// Resume deposits and withdrawals after a pause (admin only).
    pub fn unpause(env: Env) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &false);
        let admin = admin::get_admin(&env)?;
        events::unpaused(&env, &admin);
        Ok(())
    }

    /// Inject yield into the vault by transferring tokens from the admin wallet (admin only).
    pub fn add_yield(env: Env, admin_addr: Address, amount: i128) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        Self::require_not_paused(&env)?;

        if amount <= 0 {
            return Err(VaultError::ZeroAmount);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;

        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&admin_addr, &env.current_contract_address(), &amount);

        let total_deposited = balance::get_total_deposited(&env);
        balance::set_total_deposited(&env, total_deposited + amount);

        let admin_actual = admin::get_admin(&env)?;
        events::yield_added(&env, &admin_actual, amount);

        Ok(())
    }

    /// Transfer the admin role to a new address (admin only).
    pub fn transfer_admin(env: Env, new_admin: Address) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        let old_admin = admin::get_admin(&env)?;
        admin::set_admin(&env, &new_admin);
        events::admin_changed(&env, &old_admin, &new_admin);
        Ok(())
    }

    /// Admin: set the address that receives slashed tokens. Defaults to admin at initialize.
    pub fn set_slash_treasury(env: Env, treasury: Address) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        env.storage().instance().set(&DataKey::SlashTreasury, &treasury);
        Ok(())
    }

    /// Admin: enable or disable staking whitelist. When enabled, only whitelisted addresses may call stake/stake_for.
    pub fn set_whitelist_enabled(env: Env, enabled: bool) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        env.storage().instance().set(&DataKey::WhitelistEnabled, &enabled);
        Ok(())
    }

    /// Admin: add address to whitelist
    pub fn add_to_whitelist(env: Env, user: Address) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        env.storage()
            .persistent()
            .set(&DataKey::Whitelisted(user), &true);
        Ok(())
    }

    /// Admin: remove address from whitelist
    pub fn remove_from_whitelist(env: Env, user: Address) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        env.storage().persistent().remove(&DataKey::Whitelisted(user));
        Ok(())
    }

    /// Read-only: check whether a user is whitelisted
    pub fn is_whitelisted(env: Env, user: Address) -> bool {
        env.storage()
            .persistent()
            .get::<_, bool>(&DataKey::Whitelisted(user))
            .unwrap_or(false)
    }

    /// Admin: set the maximum withdrawal limit per transaction (in shares).
    pub fn set_withdrawal_limit(env: Env, limit: i128) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        if limit <= 0 {
            return Err(VaultError::ZeroAmount);
        }
        balance::set_withdrawal_limit(&env, limit);
        let admin = admin::get_admin(&env)?;
        events::withdrawal_limit_updated(&env, &admin, limit);
        Ok(())
    }

    /// Admin: set the unbonding cooldown period in ledgers. 0 disables cooldown (instant unstake allowed).
    pub fn set_cooldown_period(env: Env, ledgers: u32) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        env.storage().instance().set(&DataKey::CooldownPeriod, &ledgers);
        Ok(())
    }

    /// User-visible: request an unstake which starts the cooldown. The requested amount is removed from active stake and placed into an unbonding position.
    pub fn request_unstake(env: Env, user: Address, amount: i128) -> Result<(), VaultError> {
        user.require_auth();
        if amount <= 0 {
            return Err(VaultError::ZeroAmount);
        }

        let cooldown: u32 = env.storage().instance().get(&DataKey::CooldownPeriod).unwrap_or(0);
        // If cooldown is zero, user can call instant unstake directly — we still allow request_unstake to perform instant withdrawal for convenience

        let total_shares = balance::get_total_shares(&env);
        let total_deposited = balance::get_total_deposited(&env);
        let user_shares = balance::get_shares(&env, &user);
        if user_shares == 0 {
            return Err(VaultError::PositionNotFound);
        }

        // compute user's current token-equivalent position
        let position_amount = balance::shares_to_amount(total_shares, total_deposited, user_shares)
            .ok_or(VaultError::ArithmeticError)?;
        if position_amount <= 0 {
            return Err(VaultError::PositionNotFound);
        }

        // ensure requested amount <= position_amount
        let actual_amount = if amount > position_amount { position_amount } else { amount };

        // Crucial: finalize reward accrual up to now so that rewards on the to-be-unbonded principal stop accruing afterwards
        Self::accrue_rewards(&env, &user, user_shares)?;

        // compute shares to remove corresponding to actual_amount
        let mut shares_to_remove = balance::amount_to_shares(total_shares, total_deposited, actual_amount)
            .unwrap_or(user_shares);
        if shares_to_remove > user_shares {
            shares_to_remove = user_shares;
        }

        // compute concrete amount removed based on shares_to_remove (rounding-safe)
        let amount_removed = balance::shares_to_amount(total_shares, total_deposited, shares_to_remove)
            .ok_or(VaultError::ArithmeticError)?;

        // update user shares and totals immediately; funds remain in contract until execute_unstake
        let new_user_shares = user_shares - shares_to_remove;
        balance::set_shares(&env, &user, new_user_shares);
        balance::set_total_shares(&env, total_shares - shares_to_remove);

        let new_total_deposited = total_deposited
            .checked_sub(amount_removed)
            .ok_or(VaultError::ArithmeticError)?;
        balance::set_total_deposited(&env, new_total_deposited);

        if new_user_shares == 0 {
            env.storage()
                .persistent()
                .remove(&DataKey::StakedAtLedger(user.clone()));
            let total_stakers = balance::get_total_stakers(&env);
            if total_stakers > 0 {
                balance::set_total_stakers(&env, total_stakers - 1);
            }
            events::position_closed(&env, &user);
        }
        Self::record_stake_snapshot(&env, &user, new_user_shares);

        // store or merge unbonding position; restart cooldown from now
        let current_ledger = env.ledger().sequence();
        let mut existing: UnbondingPosition = env
            .storage()
            .persistent()
            .get(&DataKey::UnbondingPosition(user.clone()))
            .unwrap_or(UnbondingPosition { amount: 0, unbonding_since: 0 });
        let new_amount = existing.amount + amount_removed;
        let new_pos = UnbondingPosition { amount: new_amount, unbonding_since: current_ledger };
        env.storage()
            .persistent()
            .set(&DataKey::UnbondingPosition(user.clone()), &new_pos);

        // advance reward checkpoint so no further rewards accrue to the removed shares
        balance::set_reward_checkpoint_ledger(&env, &user, current_ledger);

        // If cooldown == 0, optionally auto-execute withdrawal immediately
        if cooldown == 0 {
            // transfer tokens immediately
            let token_addr: Address = env
                .storage()
                .instance()
                .get(&DataKey::Token)
                .ok_or(VaultError::NotInitialized)?;
            let token_client = token::Client::new(&env, &token_addr);
            token_client.transfer(&env.current_contract_address(), &user, &amount_removed);
            // remove unbonding position since executed
            env.storage().persistent().remove(&DataKey::UnbondingPosition(user.clone()));
        }

        Ok(())
    }

    /// Execute unstake after cooldown has passed. Transfers the pending unbonded amount to the user.
    pub fn execute_unstake(env: Env, user: Address) -> Result<i128, VaultError> {
        user.require_auth();
        let cooldown: u32 = env.storage().instance().get(&DataKey::CooldownPeriod).unwrap_or(0);
        let pos_opt: Option<UnbondingPosition> = env
            .storage()
            .persistent()
            .get(&DataKey::UnbondingPosition(user.clone()));
        let pos = match pos_opt {
            Some(p) => p,
            None => return Err(VaultError::PositionNotFound),
        };
        let current_ledger = env.ledger().sequence();
        if cooldown > 0 {
            let ready_ledger = pos.unbonding_since.saturating_add(cooldown);
            if current_ledger < ready_ledger {
                return Err(VaultError::UseCooldownFlow);
            }
        }

        // transfer tokens to user and remove unbonding record
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&env.current_contract_address(), &user, &pos.amount);

        env.storage().persistent().remove(&DataKey::UnbondingPosition(user.clone()));

        Ok(pos.amount)
    }

    /// Read-only: get pending unbonding position for a user
    pub fn pending_unbonding(env: Env, user: Address) -> Result<Option<UnbondingPosition>, VaultError> {
        let pos_opt: Option<UnbondingPosition> = env
            .storage()
            .persistent()
            .get(&DataKey::UnbondingPosition(user.clone()));
        Ok(pos_opt)
    }

    /// Query the current withdrawal limit per transaction.
    pub fn get_withdrawal_limit(env: Env) -> Result<i128, VaultError> {
        let _ = admin::get_admin(&env)?;
        Ok(balance::get_withdrawal_limit(&env).unwrap_or(0))
    }

    /// Admin: set the lock-up period in ledgers.
    pub fn set_lock_period(env: Env, ledgers: u32) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        env.storage().instance().set(&DataKey::LockPeriod, &ledgers);
        Ok(())
    }

    /// Admin: set the early exit penalty in basis points (max 2000 bps).
    pub fn set_early_exit_penalty_bps(env: Env, bps: u32) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        if bps > 2000 {
            return Err(VaultError::InvalidPenaltyBps);
        }
        env.storage()
            .instance()
            .set(&DataKey::EarlyExitPenaltyBps, &bps);
        Ok(())
    }

    /// Query the current lock-up configuration: (lock_period, early_exit_penalty_bps).
    pub fn get_lock_config(env: Env) -> Result<(u32, u32), VaultError> {
        let _ = admin::get_admin(&env)?;
        let lock_period = env
            .storage()
            .instance()
            .get(&DataKey::LockPeriod)
            .unwrap_or(0);
        let penalty_bps = env
            .storage()
            .instance()
            .get(&DataKey::EarlyExitPenaltyBps)
            .unwrap_or(0);
        Ok((lock_period, penalty_bps))
    }

    /// Admin: set the minimum stake. Zero disables the minimum.
    pub fn set_min_stake(env: Env, amount: i128) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        if amount < 0 {
            return Err(VaultError::ZeroAmount);
        }
        balance::set_min_stake(&env, amount);
        Ok(())
    }

    /// Read-only minimum stake value.
    pub fn get_min_stake(env: Env) -> Result<i128, VaultError> {
        let _ = admin::get_admin(&env)?;
        Ok(balance::get_min_stake(&env))
    }

    /// Admin: set the maximum TVL cap (in token units).
    /// A cap of 0 means no limit.
    pub fn set_pool_cap(env: Env, cap: i128) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        if cap < 0 {
            return Err(VaultError::ZeroAmount);
        }
        balance::set_pool_cap(&env, cap);
        let admin = admin::get_admin(&env)?;
        events::pool_cap_updated(&env, &admin, cap);
        Ok(())
    }

    /// Read-only pool cap value.
    /// Returns 0 if no cap is set (unlimited).
    pub fn get_pool_cap(env: Env) -> Result<i128, VaultError> {
        let _ = admin::get_admin(&env)?;
        Ok(balance::get_pool_cap(&env))
    }

    /// Admin: set the base reward APR in basis points.
    pub fn set_reward_rate_bps(env: Env, rate_bps: u32) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        let old_rate = balance::get_reward_rate_bps(&env);
        
        // Append to rate history before changing rate
        let current_ledger = env.ledger().sequence();
        let mut history = balance::get_rate_history(&env);
        history.push_back((current_ledger, old_rate));
        
        // Cap history at 50 entries
        while history.len() > balance::MAX_RATE_HISTORY_ENTRIES {
            history.pop_front();
        }
        
        balance::set_rate_history(&env, &history);
        balance::set_reward_rate_bps(&env, rate_bps);
        events::rate_changed(&env, old_rate, rate_bps);
        Ok(())
    }

    /// Read-only reward rate APR in basis points.
    pub fn get_reward_rate_bps(env: Env) -> Result<u32, VaultError> {
        let _ = admin::get_admin(&env)?;
        Ok(balance::get_reward_rate_bps(&env))
    }

    /// Read-only: returns the current effective APR in basis points.
    pub fn current_apr_bps(env: Env) -> u32 {
        balance::get_reward_rate_bps(&env)
    }

    /// Read-only: returns time-weighted average APR over the last N ledgers.
    /// Calculates the weighted average of rates based on how many ledgers each rate was active.
    pub fn twap_apr_bps(env: Env, window_ledgers: u32) -> Result<u32, VaultError> {
        let _ = admin::get_admin(&env)?;
        
        if window_ledgers == 0 {
            return Ok(balance::get_reward_rate_bps(&env));
        }

        let current_ledger = env.ledger().sequence();
        let start_ledger = if current_ledger > window_ledgers {
            current_ledger - window_ledgers
        } else {
            0
        };

        let history = balance::get_rate_history(&env);
        let current_rate = balance::get_reward_rate_bps(&env);

        // If no history, return current rate (assume it's been constant)
        if history.is_empty() {
            return Ok(current_rate);
        }

        // Build timeline: history stores (ledger, old_rate) meaning at that ledger, rate changed from old_rate to new
        // We need to reconstruct the rate timeline
        let mut weighted_sum: u64 = 0;
        let mut total_ledgers: u64 = window_ledgers as u64;
        
        // Find the rate that was active at start_ledger
        // History is ordered chronologically (oldest first)
        // We need to find the last history entry before or at start_ledger
        let mut rate_at_start = 0u32;
        let mut index = 0;
        while index < history.len() {
            let (hist_ledger, hist_rate) = history.get(index).unwrap();
            if hist_ledger <= start_ledger {
                rate_at_start = hist_rate;
            } else {
                break;
            }
            index += 1;
        }

        // Now iterate through history entries within the window
        let mut last_ledger = start_ledger;
        let mut last_rate = rate_at_start;

        while index < history.len() {
            let (hist_ledger, hist_rate) = history.get(index).unwrap();
            if hist_ledger < current_ledger {
                // hist_rate is the old rate that was active from last_ledger up to hist_ledger
                let duration = hist_ledger - last_ledger;
                weighted_sum += (duration as u64) * (hist_rate as u64);
                last_ledger = hist_ledger;
                last_rate = hist_rate;
            } else {
                break;
            }
            index += 1;
        }

        // Add final segment from last change to current ledger with current rate
        let final_duration = current_ledger - last_ledger;
        weighted_sum += (final_duration as u64) * (current_rate as u64);

        // Calculate average
        if total_ledgers == 0 {
            Ok(current_rate)
        } else {
            Ok((weighted_sum / total_ledgers) as u32)
        }
    }

    /// Read-only: returns full rate change history.
    pub fn get_rate_history(env: Env) -> Result<Vec<(u32, u32)>, VaultError> {
        let _ = admin::get_admin(&env)?;
        Ok(balance::get_rate_history(&env))
    }

    /// Admin: fund the separate reward pool used by `claim`.
    pub fn fund_reward_pool(env: Env, admin_addr: Address, amount: i128) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        if amount <= 0 {
            return Err(VaultError::ZeroAmount);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;

        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&admin_addr, &env.current_contract_address(), &amount);

        let reward_pool = balance::get_reward_pool_balance(&env);
        balance::set_reward_pool_balance(&env, reward_pool + amount);

        Ok(())
    }

    /// Read-only reward pool balance.
    pub fn get_reward_pool_balance(env: Env) -> Result<i128, VaultError> {
        let _ = admin::get_admin(&env)?;
        Ok(balance::get_reward_pool_balance(&env))
    }

    /// Admin: set the reward boost schedule, capped at five tiers.
    pub fn set_boost_schedule(env: Env, tiers: Vec<(u32, u32)>) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        if tiers.len() > MAX_BOOST_TIERS {
            return Err(VaultError::TooManyBoostTiers);
        }

        let mut last_ledger = 0;
        let mut index = 0;
        while index < tiers.len() {
            let (tier_ledger, multiplier_bps) = tiers.get(index).unwrap();
            if multiplier_bps < BOOST_BPS_BASE {
                return Err(VaultError::InvalidBoostSchedule);
            }
            if index > 0 && tier_ledger <= last_ledger {
                return Err(VaultError::InvalidBoostSchedule);
            }
            last_ledger = tier_ledger;
            index += 1;
        }

        balance::set_boost_schedule(&env, &tiers);
        Ok(())
    }

    /// Read-only reward boost schedule.
    pub fn get_boost_schedule(env: Env) -> Result<Vec<(u32, u32)>, VaultError> {
        let _ = admin::get_admin(&env)?;
        Ok(balance::get_boost_schedule(&env).unwrap_or(Vec::new(&env)))
    }

    /// Current reward multiplier for a user, based on `staked_at_ledger`.
    pub fn get_boost_multiplier(env: Env, user: Address) -> Result<u32, VaultError> {
        let _ = admin::get_admin(&env)?;
        Ok(Self::boost_multiplier_for_ledger(
            &env,
            &user,
            env.ledger().sequence(),
        ))
    }

    /// Read-only query for the reward token balance held by the contract.
    ///
    /// Returns the current balance of the vault token in the contract's own
    /// account. This covers both staked principal and the funded reward pool,
    /// allowing integrators to assess whether the pool can sustain its current
    /// reward rate before staking. No auth required.
    pub fn reward_token_balance(env: Env) -> Result<i128, VaultError> {
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;
        let token_client = token::Client::new(&env, &token_addr);
        Ok(token_client.balance(&env.current_contract_address()))
    }

    /// Read-only query for how many ledgers ago a user opened their staking position.
    ///
    /// Returns `current_ledger - staked_at_ledger` for the user's position,
    /// which is useful for frontends showing lock-up countdowns, boost tier
    /// eligibility, and time-to-target estimates.
    /// Reverts with `PositionNotFound` if the user has no active position.
    /// No auth required.
    pub fn position_age_ledgers(env: Env, user: Address) -> Result<u32, VaultError> {
        let shares = balance::get_shares(&env, &user);
        if shares == 0 {
            return Err(VaultError::PositionNotFound);
        }
        let staked_at: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::StakedAtLedger(user))
            .unwrap_or(0);
        Ok(env.ledger().sequence().saturating_sub(staked_at))
    }

    // --- Pool statistics (#38) ---

    /// Aggregate pool statistics for frontend dashboards.
    pub fn pool_stats(env: Env) -> Result<PoolStats, VaultError> {
        let _ = admin::get_admin(&env)?;
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;
        let token_client = token::Client::new(&env, &token_addr);
        let reward_token_balance = token_client.balance(&env.current_contract_address());
        let paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        Ok(PoolStats {
            total_staked: balance::get_total_deposited(&env),
            total_stakers: balance::get_total_stakers(&env),
            reward_rate_bps: balance::get_reward_rate_bps(&env) as i128,
            reward_token_balance,
            paused,
            total_rewards_paid: balance::get_total_rewards_paid(&env),
        })
    }

    /// Per-user statistics: position size, pending reward, stake age, last claim ledger.
    pub fn user_stats(env: Env, user: Address) -> Result<UserStats, VaultError> {
        let _ = admin::get_admin(&env)?;
        let position = Self::build_position(&env, &user)?;
        let position_amount = position.as_ref().map(|p| p.amount).unwrap_or(0);
        let pending_reward = Self::pending_reward(&env, &user)?;
        let staked_at_ledger = position
            .as_ref()
            .map(|p| p.staked_at_ledger)
            .unwrap_or(0);
        let last_claim_ledger = position.as_ref().map(|p| p.last_claim_ledger).unwrap_or(0);
        Ok(UserStats {
            position_amount,
            pending_reward,
            staked_at_ledger,
            last_claim_ledger,
        })
    }

    // --- Delegated staking (#37) ---

    /// Grant `delegate` permission to stake on behalf of `user`.
    pub fn approve_delegate(env: Env, user: Address, delegate: Address) -> Result<(), VaultError> {
        user.require_auth();
        balance::set_delegate(&env, &user, &delegate);
        Ok(())
    }

    /// Revoke the current delegate for `user`.
    pub fn revoke_delegate(env: Env, user: Address, delegate: Address) -> Result<(), VaultError> {
        user.require_auth();
        match balance::get_delegate(&env, &user) {
            Some(d) if d == delegate => balance::remove_delegate(&env, &user),
            _ => return Err(VaultError::NotADelegate),
        }
        Ok(())
    }

    /// Read-only check: returns true if `delegate` is approved to stake for `user`.
    pub fn is_delegate(env: Env, user: Address, delegate: Address) -> bool {
        balance::get_delegate(&env, &user)
            .map(|d| d == delegate)
            .unwrap_or(false)
    }

    /// Stake `amount` tokens from `delegate`'s wallet, crediting the position to `beneficiary`.
    /// Only an approved delegate may call this; the beneficiary retains exclusive unstake/claim rights.
    pub fn stake_for(
        env: Env,
        delegate: Address,
        beneficiary: Address,
        amount: i128,
    ) -> Result<i128, VaultError> {
        delegate.require_auth();
        Self::require_not_paused(&env)?;

        if amount <= 0 {
            return Err(VaultError::ZeroAmount);
        }

        match balance::get_delegate(&env, &beneficiary) {
            Some(d) if d == delegate => {}
            _ => return Err(VaultError::NotADelegate),
        }

        // If whitelist is enabled, ensure beneficiary is whitelisted for new stakes
        let whitelist_enabled: bool = env
            .storage()
            .instance()
            .get(&DataKey::WhitelistEnabled)
            .unwrap_or(false);
        if whitelist_enabled {
            let allowed = env
                .storage()
                .persistent()
                .get::<_, bool>(&DataKey::Whitelisted(beneficiary.clone()))
                .unwrap_or(false);
            if !allowed {
                return Err(VaultError::NotWhitelisted);
            }
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;

        let total_shares = balance::get_total_shares(&env);
        let total_deposited = balance::get_total_deposited(&env);
        let current_shares = balance::get_shares(&env, &beneficiary);

        Self::require_min_stake(&env, current_shares, total_shares, total_deposited, amount)?;
        Self::accrue_rewards(&env, &beneficiary, current_shares)?;

        let cap = balance::get_pool_cap(&env);
        if cap > 0 {
            let new_total_deposited = total_deposited
                .checked_add(amount)
                .ok_or(VaultError::ArithmeticError)?;
            if new_total_deposited > cap {
                return Err(VaultError::PoolCapReached);
            }
        }

        let shares = balance::amount_to_shares(total_shares, total_deposited, amount)
            .ok_or(VaultError::ArithmeticError)?;

        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&delegate, &env.current_contract_address(), &amount);

        let new_shares = current_shares + shares;
        balance::set_shares(&env, &beneficiary, new_shares);
        balance::set_total_shares(&env, total_shares + shares);
        balance::set_total_deposited(&env, total_deposited + amount);

        let current_ledger = env.ledger().sequence();
        if current_shares == 0 {
            env.storage()
                .persistent()
                .set(&DataKey::StakedAtLedger(beneficiary.clone()), &current_ledger);
            let total_stakers = balance::get_total_stakers(&env);
            balance::set_total_stakers(&env, total_stakers + 1);
            events::position_opened(&env, &beneficiary, amount);
        }
        Self::record_stake_snapshot(&env, &beneficiary, new_shares);

        events::deposit(&env, &beneficiary, amount, shares);

        Ok(shares)
    }

    /// Admin: slash a user's staked principal. Can be called while paused.
    /// `admin_addr` must be the admin and is provided to follow existing patterns.
    /// Returns the actual slashed token amount.
    pub fn slash(env: Env, admin_addr: Address, user: Address, amount: i128) -> Result<i128, VaultError> {
        // authorization: caller must be admin (enforced by require_admin)
        admin::require_admin(&env)?;
        // admin_addr is an argument (follows other admin methods) but we still check admin auth above

        if amount <= 0 {
            return Err(VaultError::ZeroAmount);
        }

        let user_shares = balance::get_shares(&env, &user);
        if user_shares == 0 {
            return Err(VaultError::PositionNotFound);
        }

        let total_shares = balance::get_total_shares(&env);
        let total_deposited = balance::get_total_deposited(&env);

        // compute user's position amount (token units)
        let position_amount = balance::shares_to_amount(total_shares, total_deposited, user_shares)
            .ok_or(VaultError::ArithmeticError)?;
        if position_amount == 0 {
            return Err(VaultError::PositionNotFound);
        }

        // actual_slash_amount = min(requested, position_amount)
        let actual = if amount > position_amount { position_amount } else { amount };

        // compute shares to remove corresponding to `actual` (may round)
        let mut shares_to_remove = balance::amount_to_shares(total_shares, total_deposited, actual)
            .unwrap_or(user_shares);
        if shares_to_remove > user_shares {
            shares_to_remove = user_shares;
        }

        // token and treasury addresses
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;
        let treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::SlashTreasury)
            .ok_or(VaultError::NotInitialized)?;

        // update user shares and totals
        let new_user_shares = user_shares - shares_to_remove;
        balance::set_shares(&env, &user, new_user_shares);
        balance::set_total_shares(&env, total_shares - shares_to_remove);

        let new_total_deposited = total_deposited
            .checked_sub(actual)
            .ok_or(VaultError::ArithmeticError)?;
        balance::set_total_deposited(&env, new_total_deposited);

        if new_user_shares == 0 {
            env.storage()
                .persistent()
                .remove(&DataKey::StakedAtLedger(user.clone()));
            let total_stakers = balance::get_total_stakers(&env);
            if total_stakers > 0 {
                balance::set_total_stakers(&env, total_stakers - 1);
            }
            events::position_closed(&env, &user);
        }
        Self::record_stake_snapshot(&env, &user, new_user_shares);

        // Reward forfeiture: clear accrued rewards and advance checkpoint so no further claim for pre-slash accrual
        balance::set_accrued_reward(&env, &user, 0);
        balance::set_reward_checkpoint_ledger(&env, &user, env.ledger().sequence());
        balance::set_last_claim_ledger(&env, &user, env.ledger().sequence());

        // transfer slashed tokens from contract to treasury
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&env.current_contract_address(), &treasury, &actual);

        // emit event
        let admin_actual = admin::get_admin(&env)?;
        events::slash(&env, &admin_actual, &user, actual);

        Ok(actual)
    }

    // --- Internal helpers ---

    fn do_stake(env: &Env, staker: &Address, amount: i128) -> Result<i128, VaultError> {
        staker.require_auth();
        Self::require_not_paused(env)?;

        // If whitelist is enabled, reject non-whitelisted stakers. Existing stakers can still unstake/claim.
        let whitelist_enabled: bool = env
            .storage()
            .instance()
            .get(&DataKey::WhitelistEnabled)
            .unwrap_or(false);
        if whitelist_enabled {
            let allowed = env
                .storage()
                .persistent()
                .get::<_, bool>(&DataKey::Whitelisted(staker.clone()))
                .unwrap_or(false);
            if !allowed {
                return Err(VaultError::NotWhitelisted);
            }
        }

        if amount <= 0 {
            return Err(VaultError::ZeroAmount);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;

        let total_shares = balance::get_total_shares(env);
        let total_deposited = balance::get_total_deposited(env);
        let current_shares = balance::get_shares(env, staker);

        Self::require_min_stake(env, current_shares, total_shares, total_deposited, amount)?;
        Self::accrue_rewards(env, staker, current_shares)?;

        let cap = balance::get_pool_cap(env);
        if cap > 0 {
            let new_total_deposited = total_deposited
                .checked_add(amount)
                .ok_or(VaultError::ArithmeticError)?;
            if new_total_deposited > cap {
                return Err(VaultError::PoolCapReached);
            }
        }

        let shares = balance::amount_to_shares(total_shares, total_deposited, amount)
            .ok_or(VaultError::ArithmeticError)?;

        let token_client = token::Client::new(env, &token_addr);
        token_client.transfer(staker, &env.current_contract_address(), &amount);

        let new_shares = current_shares + shares;
        balance::set_shares(env, staker, new_shares);
        balance::set_total_shares(env, total_shares + shares);
        balance::set_total_deposited(env, total_deposited + amount);

        let current_ledger = env.ledger().sequence();
        if current_shares == 0 {
            env.storage()
                .persistent()
                .set(&DataKey::StakedAtLedger(staker.clone()), &current_ledger);
            let total_stakers = balance::get_total_stakers(env);
            balance::set_total_stakers(env, total_stakers + 1);
            events::position_opened(env, staker, amount);
        }
        Self::record_stake_snapshot(env, staker, new_shares);

        events::deposit(env, staker, amount, shares);

        Ok(shares)
    }

    fn do_unstake(env: &Env, staker: &Address, shares: i128) -> Result<i128, VaultError> {
        staker.require_auth();
        Self::require_not_paused(env)?;

        // If cooldown is enabled, force use of request_unstake/execute_unstake flow
        let cooldown: u32 = env.storage().instance().get(&DataKey::CooldownPeriod).unwrap_or(0);
        if cooldown > 0 {
            return Err(VaultError::UseCooldownFlow);
        }

        if shares <= 0 {
            return Err(VaultError::ZeroAmount);
        }

        if let Some(limit) = balance::get_withdrawal_limit(env) {
            if shares > limit {
                return Err(VaultError::WithdrawalLimitExceeded);
            }
        }

        let user_shares = balance::get_shares(env, staker);
        if user_shares < shares {
            return Err(VaultError::InsufficientShares);
        }

        Self::accrue_rewards(env, staker, user_shares)?;

        let total_shares = balance::get_total_shares(env);
        let total_deposited = balance::get_total_deposited(env);

        let amount = balance::shares_to_amount(total_shares, total_deposited, shares)
            .ok_or(VaultError::ArithmeticError)?;

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;

        let lock_period = env
            .storage()
            .instance()
            .get(&DataKey::LockPeriod)
            .unwrap_or(0);
        let penalty_bps = env
            .storage()
            .instance()
            .get(&DataKey::EarlyExitPenaltyBps)
            .unwrap_or(0);

        let current_ledger = env.ledger().sequence();
        let is_locked = if lock_period == 0 {
            false
        } else {
            match env
                .storage()
                .persistent()
                .get::<_, u32>(&DataKey::StakedAtLedger(staker.clone()))
            {
                Some(staked_at) => current_ledger < staked_at.saturating_add(lock_period),
                None => false,
            }
        };

        let amount_returned = if is_locked && penalty_bps > 0 {
            let penalty = amount
                .checked_mul(penalty_bps as i128)
                .ok_or(VaultError::ArithmeticError)?
                .checked_div(BOOST_BPS_BASE as i128)
                .ok_or(VaultError::ArithmeticError)?;
            amount - penalty
        } else {
            amount
        };

        let new_user_shares = user_shares - shares;
        balance::set_shares(env, staker, new_user_shares);
        balance::set_total_shares(env, total_shares - shares);
        balance::set_total_deposited(env, total_deposited - amount_returned);

        if new_user_shares == 0 {
            env.storage()
                .persistent()
                .remove(&DataKey::StakedAtLedger(staker.clone()));
            let total_stakers = balance::get_total_stakers(env);
            if total_stakers > 0 {
                balance::set_total_stakers(env, total_stakers - 1);
            }
            events::position_closed(env, staker);
        }
        Self::record_stake_snapshot(env, staker, new_user_shares);

        let token_client = token::Client::new(env, &token_addr);
        token_client.transfer(&env.current_contract_address(), staker, &amount_returned);

        events::withdraw(env, staker, shares, amount_returned);

        Ok(amount_returned)
    }

    fn require_min_stake(
        env: &Env,
        current_shares: i128,
        total_shares: i128,
        total_deposited: i128,
        amount: i128,
    ) -> Result<(), VaultError> {
        let min_stake = balance::get_min_stake(env);
        if min_stake == 0 {
            return Ok(());
        }

        if current_shares == 0 {
            return if amount < min_stake {
                Err(VaultError::BelowMinimumStake)
            } else {
                Ok(())
            };
        }

        let current_position =
            balance::shares_to_amount(total_shares, total_deposited, current_shares)
                .ok_or(VaultError::ArithmeticError)?;
        let resulting_position = current_position
            .checked_add(amount)
            .ok_or(VaultError::ArithmeticError)?;

        if resulting_position < min_stake {
            Err(VaultError::BelowMinimumStake)
        } else {
            Ok(())
        }
    }

    fn record_stake_snapshot(env: &Env, user: &Address, amount: i128) {
        let current_ledger = env.ledger().sequence();
        let mut history = balance::get_stake_history(env, user).unwrap_or(Vec::new(env));

        if history.len() > 0 {
            let last_index = history.len() - 1;
            let (last_ledger, _) = history.get(last_index).unwrap();
            if last_ledger == current_ledger {
                history.set(last_index, (current_ledger, amount));
            } else {
                history.push_back((current_ledger, amount));
            }
        } else {
            history.push_back((current_ledger, amount));
        }

        while history.len() > MAX_HISTORY_SNAPSHOTS {
            let _ = history.pop_front();
        }

        balance::set_stake_history(env, user, &history);
    }

    fn build_position(env: &Env, user: &Address) -> Result<Option<StakePosition>, VaultError> {
        let shares = balance::get_shares(env, user);
        if shares == 0 {
            return Ok(None);
        }

        let total_shares = balance::get_total_shares(env);
        let total_deposited = balance::get_total_deposited(env);
        let amount = balance::shares_to_amount(total_shares, total_deposited, shares)
            .ok_or(VaultError::ArithmeticError)?;
        let staked_at_ledger = env
            .storage()
            .persistent()
            .get::<_, u32>(&DataKey::StakedAtLedger(user.clone()))
            .unwrap_or(0);
        let last_claim_ledger = balance::get_last_claim_ledger(env, user);

        Ok(Some(StakePosition {
            amount,
            staked_at_ledger,
            last_claim_ledger,
        }))
    }

    fn pending_reward(env: &Env, user: &Address) -> Result<i128, VaultError> {
        let current_shares = balance::get_shares(env, user);
        let accrued = balance::get_accrued_reward(env, user);
        let checkpoint =
            balance::get_reward_checkpoint_ledger(env, user).unwrap_or(env.ledger().sequence());

        let pending_since_checkpoint = Self::reward_between_ledgers(
            env,
            user,
            current_shares,
            checkpoint,
            env.ledger().sequence(),
        )?;

        accrued
            .checked_add(pending_since_checkpoint)
            .ok_or(VaultError::ArithmeticError)
    }

    fn accrue_rewards(env: &Env, user: &Address, current_shares: i128) -> Result<(), VaultError> {
        let current_ledger = env.ledger().sequence();
        let checkpoint = balance::get_reward_checkpoint_ledger(env, user).unwrap_or(current_ledger);
        let additional_reward =
            Self::reward_between_ledgers(env, user, current_shares, checkpoint, current_ledger)?;

        if additional_reward > 0 {
            let accrued = balance::get_accrued_reward(env, user);
            let updated_accrued = accrued
                .checked_add(additional_reward)
                .ok_or(VaultError::ArithmeticError)?;
            balance::set_accrued_reward(env, user, updated_accrued);
        }

        balance::set_reward_checkpoint_ledger(env, user, current_ledger);
        Ok(())
    }

    fn reward_between_ledgers(
        env: &Env,
        user: &Address,
        current_shares: i128,
        start_ledger: u32,
        end_ledger: u32,
    ) -> Result<i128, VaultError> {
        if current_shares == 0 || end_ledger <= start_ledger {
            return Ok(0);
        }

        let rate_bps = balance::get_reward_rate_bps(env);
        if rate_bps == 0 {
            return Ok(0);
        }

        let staked_at = match env
            .storage()
            .persistent()
            .get::<_, u32>(&DataKey::StakedAtLedger(user.clone()))
        {
            Some(ledger) => ledger,
            None => return Ok(0),
        };

        let schedule = balance::get_boost_schedule(env).unwrap_or(Vec::new(env));
        let mut reward: i128 = 0;
        let mut cursor = start_ledger;
        let mut current_multiplier =
            Self::multiplier_for_elapsed(schedule.clone(), cursor.saturating_sub(staked_at));
        let mut index = 0;

        while index < schedule.len() {
            let (tier_ledger, tier_multiplier) = schedule.get(index).unwrap();
            let threshold = staked_at.saturating_add(tier_ledger);

            if threshold <= cursor {
                current_multiplier = tier_multiplier;
                index += 1;
                continue;
            }

            if threshold >= end_ledger {
                break;
            }

            reward = reward
                .checked_add(Self::reward_for_ledgers(
                    current_shares,
                    rate_bps,
                    current_multiplier,
                    threshold - cursor,
                )?)
                .ok_or(VaultError::ArithmeticError)?;

            cursor = threshold;
            current_multiplier = tier_multiplier;
            index += 1;
        }

        reward = reward
            .checked_add(Self::reward_for_ledgers(
                current_shares,
                rate_bps,
                current_multiplier,
                end_ledger - cursor,
            )?)
            .ok_or(VaultError::ArithmeticError)?;

        Ok(reward)
    }

    fn reward_for_ledgers(
        amount: i128,
        rate_bps: u32,
        multiplier_bps: u32,
        elapsed_ledgers: u32,
    ) -> Result<i128, VaultError> {
        if elapsed_ledgers == 0 || amount == 0 {
            return Ok(0);
        }

        let effective_rate_bps = (rate_bps as i128)
            .checked_mul(multiplier_bps as i128)
            .ok_or(VaultError::ArithmeticError)?
            .checked_div(BOOST_BPS_BASE as i128)
            .ok_or(VaultError::ArithmeticError)?;

        amount
            .checked_mul(effective_rate_bps)
            .ok_or(VaultError::ArithmeticError)?
            .checked_mul(elapsed_ledgers as i128)
            .ok_or(VaultError::ArithmeticError)?
            .checked_div(BOOST_BPS_BASE as i128)
            .ok_or(VaultError::ArithmeticError)?
            .checked_div(STELLAR_LEDGERS_PER_YEAR as i128)
            .ok_or(VaultError::ArithmeticError)
    }

    fn boost_multiplier_for_ledger(env: &Env, user: &Address, ledger: u32) -> u32 {
        let staked_at = match env
            .storage()
            .persistent()
            .get::<_, u32>(&DataKey::StakedAtLedger(user.clone()))
        {
            Some(staked_at) => staked_at,
            None => return BOOST_BPS_BASE,
        };

        let schedule = balance::get_boost_schedule(env).unwrap_or(Vec::new(env));
        Self::multiplier_for_elapsed(schedule, ledger.saturating_sub(staked_at))
    }

    fn multiplier_for_elapsed(schedule: Vec<(u32, u32)>, elapsed: u32) -> u32 {
        let mut multiplier = BOOST_BPS_BASE;
        let mut index = 0;

        while index < schedule.len() {
            let (tier_ledger, tier_multiplier) = schedule.get(index).unwrap();
            if elapsed < tier_ledger {
                break;
            }
            multiplier = tier_multiplier;
            index += 1;
        }

        multiplier
    }

    fn require_not_paused(env: &Env) -> Result<(), VaultError> {
        let paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if paused {
            Err(VaultError::VaultPaused)
        } else {
            Ok(())
        }
    }
}
