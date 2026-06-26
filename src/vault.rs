use soroban_sdk::{contract, contractimpl, token, Address, Env, String, Vec};

use crate::{
    admin, balance, errors::VaultError, events,
    nft::StakeReceiptNFTClient,
    storage::{
        CampaignInfo, ClaimWindow, DataKey, InterfaceId, LeaderboardEntry, OptionalPosition,
        PoolConfig, PoolStats, StakeAction, StakeHistoryEntry, StakePosition, StakeStreak,
        UnbondingPosition, UnstakeCheckResult, UserStats, UserSummary,
    },
};

/// Maximum number of stake/unstake history entries kept per user (issue #105).
pub(crate) const MAX_STAKE_HISTORY: u32 = 5;

pub(crate) const CONTRACT_VERSION: &str = "0.1.0";
pub(crate) const BOOST_BPS_BASE: u32 = 10_000;
pub(crate) const MAX_BOOST_TIERS: u32 = 5;
pub(crate) const MAX_HISTORY_SNAPSHOTS: u32 = 100;
pub(crate) const STELLAR_LEDGERS_PER_YEAR: u32 = 6_307_200;
pub(crate) const MAX_UNSTAKE_FEE_BPS: u32 = 500;

#[contract]
pub struct VaultContract;

#[contractimpl]
impl VaultContract {
    /// Initialize the vault with an admin and the token it accepts.
    ///
    /// `reward_rate_bps` sets the initial APR in basis points (max `MAX_RATE_BPS`).
    /// Pass `0` to start with no reward rate and configure it later via `set_reward_rate_bps`.
    ///
    /// `stake_decimals` and `reward_decimals` declare the decimal precision of
    /// the stake and reward tokens so reward amounts can be normalized when the
    /// two tokens differ. Both are optional and default to 7 (the Stellar
    /// standard) when `None` is passed, keeping pools initialized without
    /// explicit decimals backward compatible.
    pub fn initialize(
        env: Env,
        admin: Address,
        token: Address,
        reward_rate_bps: u32,
        stake_decimals: Option<u32>,
        reward_decimals: Option<u32>,
    ) -> Result<(), VaultError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(VaultError::AlreadyInitialized);
        }

        // Issue #70: reject zero/self-referential addresses.
        let self_addr = env.current_contract_address();
        if admin == self_addr {
            return Err(VaultError::InvalidAddress);
        }
        if token == self_addr {
            return Err(VaultError::InvalidAddress);
        }

        // Issue #72: validate reward rate.
        Self::validate_rate_bps(reward_rate_bps)?;

        admin::set_admin(&env, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::Paused, &false);
        // By default, set the slash treasury to the admin address. Can be updated by admin later.
        env.storage().instance().set(&DataKey::SlashTreasury, &admin);

        if reward_rate_bps > 0 {
            balance::set_reward_rate_bps(&env, reward_rate_bps);
        }

        // Persist token decimals so reward math can normalize across mismatched
        // precisions. Unspecified values fall back to the Stellar standard of 7.
        balance::set_stake_decimals(
            &env,
            stake_decimals.unwrap_or(balance::DEFAULT_TOKEN_DECIMALS),
        );
        balance::set_reward_decimals(
            &env,
            reward_decimals.unwrap_or(balance::DEFAULT_TOKEN_DECIMALS),
        );

        events::pool_initialized(&env, &admin, &token, &token, reward_rate_bps);
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
    ///
    /// Accrues any pending rewards up to the current ledger, then transfers the
    /// full accrued balance to `staker`. If an admin-configured claim cap is
    /// active the payout is limited to whatever headroom remains in the current
    /// window; the remainder stays accrued and can be claimed in the next window.
    ///
    /// Returns the token amount transferred. Returns 0 if there is nothing to claim.
    pub fn claim(env: Env, staker: Address) -> Result<i128, VaultError> {
        staker.require_auth();
        Self::do_claim(&env, &staker)
    }

    /// Convenience function that claims pending rewards and adds a new stake
    /// position in a single transaction, requiring only one user authorisation.
    ///
    /// Claim logic runs first so that any reward accrued on the existing stake
    /// is settled before the new deposit changes the share ratio. The staking
    /// logic then runs exactly as `stake` would. Events emitted in order:
    /// `claimed` (reward amount) then `deposit` (new stake shares).
    ///
    /// Returns the reward amount paid out. Returns 0 if there was nothing to
    /// claim before the stake was added.
    pub fn stake_and_claim(env: Env, user: Address, amount: i128) -> Result<i128, VaultError> {
        user.require_auth();

        // Settle pending rewards on the existing position first.
        let claimed_amount = Self::do_claim(&env, &user)?;

        // Stake the requested amount; do_stake_inner skips require_auth since
        // the single auth above already covers both actions.
        Self::do_stake_inner(&env, &user, amount)?;

        Ok(claimed_amount)
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

    /// Read-only query for the token address that users must deposit to stake.
    pub fn get_stake_token(env: Env) -> Result<Address, VaultError> {
        env.storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)
    }

    /// Read-only: ledger sequence of the last state-changing operation (issue #69).
    ///
    /// Returns 0 if no state-changing operation has been recorded yet.
    /// Updated by stake, unstake, claim, pause, and unpause.
    pub fn get_last_updated_ledger(env: Env) -> u32 {
        balance::get_last_updated_ledger(&env)
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

    /// Returns positions for a list of addresses in a single contract call.
    ///
    /// Results are returned in the same order as the input list. `None` is returned
    /// for users with no active position — the call never reverts on a missing user.
    /// Reverts with `BatchTooLarge` when more than 20 addresses are supplied to prevent
    /// excessive compute costs per invocation. No auth required.
    pub fn batch_position_query(
        env: Env,
        users: Vec<Address>,
    ) -> Result<Vec<Option<StakePosition>>, VaultError> {
        if users.len() > 20 {
            return Err(VaultError::BatchTooLarge);
        }
        let mut results = Vec::new(&env);
        let mut i = 0;
        while i < users.len() {
            let user = users.get(i).unwrap();
            results.push_back(Self::build_position(&env, &user)?);
            i += 1;
        }
        Ok(results)
    }

    /// Returns the ledger at which the user's current reward accrual period started.
    ///
    /// Reads `last_claim_ledger` from the user's `StakePosition`. This value is reset
    /// on every reward settlement (claim, stake top-up, or unstake), so it marks the
    /// ledger from which rewards are currently accruing. Reverts with `PositionNotFound`
    /// if the user has no active position.
    pub fn claimable_since(env: Env, user: Address) -> Result<u32, VaultError> {
        match Self::build_position(&env, &user)? {
            Some(p) => Ok(p.last_claim_ledger),
            None => Err(VaultError::PositionNotFound),
        }
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

    /// Read-only query for the total rewards paid out since deployment.
    pub fn total_rewards_paid(env: Env) -> i128 {
        balance::get_total_rewards_paid(&env)
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

    /// Read-only query for pending staking rewards, expressed in reward token
    /// decimals. Internally rewards accrue in stake token precision, so the
    /// result is normalized to the reward token's precision before returning.
    pub fn calc_pending_reward(env: Env, user: Address) -> Result<i128, VaultError> {
        let _ = admin::get_admin(&env)?;
        let raw = Self::pending_reward(&env, &user)?;
        Self::normalize_to_reward_decimals(&env, raw)
    }

    /// Read-only query for the configured stake token decimal precision.
    pub fn stake_decimals(env: Env) -> u32 {
        balance::get_stake_decimals(&env)
    }

    /// Read-only query for the configured reward token decimal precision.
    pub fn reward_decimals(env: Env) -> u32 {
        balance::get_reward_decimals(&env)
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
        // Issue #107: stopped contracts cannot be re-paused or unpaused.
        Self::require_not_stopped(&env)?;
        env.storage().instance().set(&DataKey::Paused, &true);
        let admin = admin::get_admin(&env)?;
        events::paused(&env, &admin);
        events::admin_action_pause(&env, &admin);
        balance::increment_admin_action_count(&env);
        balance::set_last_updated_ledger(&env, env.ledger().sequence()); // Issue #69
        Ok(())
    }

    /// Resume deposits and withdrawals after a pause (admin only).
    pub fn unpause(env: Env) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        // Issue #107: stopped contracts cannot be re-paused or unpaused.
        Self::require_not_stopped(&env)?;
        env.storage().instance().set(&DataKey::Paused, &false);
        let admin = admin::get_admin(&env)?;
        events::unpaused(&env, &admin);
        events::admin_action_unpause(&env, &admin);
        balance::increment_admin_action_count(&env);
        balance::set_last_updated_ledger(&env, env.ledger().sequence()); // Issue #69
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
        events::admin_action_add_yield(&env, &admin_actual, amount);
        balance::increment_admin_action_count(&env);

        Ok(())
    }

    /// Transfer the admin role to a new address (admin only).
    pub fn transfer_admin(env: Env, new_admin: Address) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        let old_admin = admin::get_admin(&env)?;
        admin::set_admin(&env, &new_admin);
        events::admin_changed(&env, &old_admin, &new_admin);
        events::admin_action_transfer_admin(&env, &old_admin, &new_admin);
        balance::increment_admin_action_count(&env);
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
        events::admin_action_set_cap(&env, &admin, limit);
        balance::increment_admin_action_count(&env);
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
            Self::remove_from_staker_list(&env, &user);
            events::position_closed(&env, &user);
        }
        Self::record_stake_snapshot(&env, &user, new_user_shares);
        Self::update_leaderboard(&env, &user, new_user_shares);

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
        let admin = admin::get_admin(&env)?;
        events::admin_action_set_lock_period(&env, &admin, ledgers);
        balance::increment_admin_action_count(&env);
        Ok(())
    }

    /// Admin: set the early exit penalty in basis points (max 2000 bps).
    pub fn set_early_exit_penalty_bps(env: Env, bps: u32) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        if bps > 2000 {
            return Err(VaultError::InvalidPenaltyBps);
        }
        env.storage().instance().set(&DataKey::EarlyExitPenaltyBps, &bps);
        let admin = admin::get_admin(&env)?;
        events::admin_action_set_early_exit_penalty(&env, &admin, bps);
        balance::increment_admin_action_count(&env);
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

    /// Admin: set the unstake fee in basis points charged on exit.
    ///
    /// The fee is deducted from the principal returned to the user (after any
    /// lock-up penalty) and routed to the reward pool treasury. Pass `0` to
    /// disable. The maximum is 500 bps (5%); higher values are rejected with
    /// `UnstakeFeeTooHigh`.
    pub fn set_unstake_fee_bps(env: Env, admin: Address, bps: u32) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        let _ = admin; // argument follows existing admin patterns; auth enforced above
        if bps > MAX_UNSTAKE_FEE_BPS {
            return Err(VaultError::UnstakeFeeTooHigh);
        }
        balance::set_unstake_fee_bps(&env, bps);
        Ok(())
    }

    /// Read-only query for the current unstake fee in basis points.
    pub fn get_unstake_fee_bps(env: Env) -> u32 {
        balance::get_unstake_fee_bps(&env)
    }

    /// Admin: set the minimum stake. Zero disables the minimum.
    pub fn set_min_stake(env: Env, amount: i128) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        if amount < 0 {
            return Err(VaultError::ZeroAmount);
        }
        balance::set_min_stake(&env, amount);
        let admin = admin::get_admin(&env)?;
        events::admin_action_set_min_stake(&env, &admin, amount);
        balance::increment_admin_action_count(&env);
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

    /// Return all pool-level configuration in a single call.
    ///
    /// Reduces frontend RPC overhead by aggregating `admin`, `stake_token`,
    /// `reward_token`, `reward_rate_bps`, and `paused` into one `PoolConfig`.
    /// This is a pure read — no state is modified. Reverts with `NotInitialized`
    /// if the contract has not yet been initialised.
    pub fn get_pool_config(env: Env) -> Result<PoolConfig, VaultError> {
        let admin = admin::get_admin(&env)?;
        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;
        let reward_rate_bps = balance::get_reward_rate_bps(&env);
        let paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        Ok(PoolConfig {
            admin,
            stake_token: token.clone(),
            reward_token: token,
            reward_rate_bps,
            paused,
        })
    }

    /// Admin: set the per-user reward claim cap and rolling window size.
    ///
    /// `max_amount` is the maximum cumulative reward any single user may claim
    /// within a window of `window_ledgers` ledgers. Pass `0` for `max_amount`
    /// to disable the cap entirely. The window resets automatically once
    /// `current_ledger > window_started_at + window_ledgers`.
    ///
    /// Unclaimed remainder accrues into the next window — it is never lost.
    pub fn set_claim_cap(
        env: Env,
        admin: Address,
        max_amount: i128,
        window_ledgers: u32,
    ) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        let _ = admin; // argument follows existing admin patterns; auth enforced above
        if max_amount < 0 {
            return Err(VaultError::ZeroAmount);
        }
        balance::set_claim_cap(&env, max_amount);
        balance::set_claim_cap_window(&env, window_ledgers);
        Ok(())
    }

    /// Read-only query: return the current claim window state for a user.
    ///
    /// Returns `None` when the user has never claimed or the cap is disabled.
    /// Frontend can use this to show how much of the cap has been consumed and
    /// when the window resets.
    pub fn get_claim_window(env: Env, user: Address) -> Option<ClaimWindow> {
        balance::get_user_claim_window(&env, &user)
    }

    /// Admin: set the base reward APR in basis points.
    pub fn set_reward_rate_bps(env: Env, rate_bps: u32) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        Self::validate_rate_bps(rate_bps)?; // Issue #72
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
        let admin = admin::get_admin(&env)?;
        events::admin_action_set_reward_rate(&env, &admin, old_rate, rate_bps);
        balance::increment_admin_action_count(&env);
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
        
        // Each history entry (L, old_rate) means "at L, rate changed FROM old_rate".
        // The rate active FROM ledger L is the old_rate of the NEXT entry, or current_rate if last.
        // Find the first entry strictly after start_ledger.
        let mut index: u32 = 0;
        while index < history.len() {
            let (hist_ledger, _) = history.get(index).unwrap();
            if hist_ledger <= start_ledger {
                index += 1;
            } else {
                break;
            }
        }
        // Rate at start_ledger = old_rate of the first entry after start, or current_rate.
        let rate_at_start = if index < history.len() {
            let (_, next_old_rate) = history.get(index).unwrap();
            next_old_rate
        } else {
            current_rate
        };

        // Iterate through history entries within the window.
        let mut last_ledger = start_ledger;
        let mut last_rate = rate_at_start;

        while index < history.len() {
            let (hist_ledger, _) = history.get(index).unwrap();
            if hist_ledger < current_ledger {
                // hist_rate is the old rate that was active from last_ledger up to hist_ledger
                let duration = hist_ledger - last_ledger;
                weighted_sum += (duration as u64) * (last_rate as u64);
                last_ledger = hist_ledger;
                index += 1;
                // Rate active from hist_ledger = old_rate of next entry, or current_rate.
                last_rate = if index < history.len() {
                    let (_, next_old_rate) = history.get(index).unwrap();
                    next_old_rate
                } else {
                    current_rate
                };
            } else {
                break;
            }
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

        let admin_actual = admin::get_admin(&env)?;
        events::admin_action_fund_reward_pool(&env, &admin_actual, amount);
        balance::increment_admin_action_count(&env);

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

        let num_tiers = tiers.len();
        balance::set_boost_schedule(&env, &tiers);
        let admin = admin::get_admin(&env)?;
        events::admin_action_set_boost_schedule(&env, &admin, num_tiers);
        balance::increment_admin_action_count(&env);
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

    // --- Issue #39: rescue stuck tokens ---

    /// Admin: register a separate reward token address (distinct from the stake token).
    /// Once set, `rescue_token` will also reject this token with CannotRescueRewardToken.
    pub fn set_reward_token(env: Env, token: Address) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        balance::set_reward_token(&env, &token);
        let admin = admin::get_admin(&env)?;
        events::admin_action_set_reward_token(&env, &admin, &token);
        balance::increment_admin_action_count(&env);
        Ok(())
    }

    /// Admin: transfer `amount` of a stuck non-stake, non-reward token to `recipient`.
    /// Rejects if the token is the stake token or the registered reward token.
    pub fn rescue_token(
        env: Env,
        admin_addr: Address,
        token: Address,
        amount: i128,
        recipient: Address,
    ) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        let admin = admin::get_admin(&env)?;
        if admin_addr != admin {
            return Err(VaultError::Unauthorized);
        }

        if amount <= 0 {
            return Err(VaultError::ZeroAmount);
        }

        let stake_token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;

        if token == stake_token {
            return Err(VaultError::CannotRescueStakeToken);
        }

        if let Some(reward_token) = balance::get_reward_token(&env) {
            if token == reward_token {
                return Err(VaultError::CannotRescueRewardToken);
            }
        }

        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&env.current_contract_address(), &recipient, &amount);

        events::token_rescued(&env, &token, amount, &recipient);
        events::admin_action_rescue_token(&env, &admin, &token, amount, &recipient);
        balance::increment_admin_action_count(&env);

        Ok(())
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
        let balance = token::Client::new(&env, &token_addr)
            .balance(&env.current_contract_address());
        Ok(balance)
    }

    // --- Issue #40: NFT receipt ---

    /// Admin: register the companion StakeReceiptNFT contract address.
    pub fn set_nft_contract(env: Env, nft: Address) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        balance::set_nft_contract(&env, &nft);
        let admin = admin::get_admin(&env)?;
        events::admin_action_set_nft_contract(&env, &admin, &nft);
        balance::increment_admin_action_count(&env);
        Ok(())
    }

    // --- Issue #41: restake grace window ---

    /// Admin: set the restake grace window in ledgers. Zero disables the feature.
    pub fn set_restake_window(env: Env, ledgers: u32) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        balance::set_restake_window(&env, ledgers);
        let admin = admin::get_admin(&env)?;
        events::admin_action_set_restake_window(&env, &admin, ledgers);
        balance::increment_admin_action_count(&env);
        Ok(())
    }

    // --- Issue #42: admin action audit log ---

    /// Read-only running count of admin actions taken on this contract.
    pub fn get_admin_action_count(env: Env) -> Result<u32, VaultError> {
        let _ = admin::get_admin(&env)?;
        Ok(balance::get_admin_action_count(&env))
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
            balance::set_last_claim_ledger(&env, &beneficiary, current_ledger);
            let total_stakers = balance::get_total_stakers(&env);
            balance::set_total_stakers(&env, total_stakers + 1);
            let mut all_stakers = balance::get_all_stakers(&env);
            all_stakers.push_back(beneficiary.clone());
            balance::set_all_stakers(&env, &all_stakers);
            events::position_opened(&env, &beneficiary, amount);

            // Issue #41: mark as restaked if within grace window
            let restake_window = balance::get_restake_window(&env);
            if restake_window > 0 {
                if let Some(last_unstake) = balance::get_last_unstake_ledger(&env, &beneficiary) {
                    if current_ledger.saturating_sub(last_unstake) <= restake_window {
                        balance::set_restaked(&env, &beneficiary, true);
                    }
                }
            }

            // Issue #40: mint NFT receipt
            if let Some(nft_addr) = balance::get_nft_contract(&env) {
                let nft_client = StakeReceiptNFTClient::new(&env, &nft_addr);
                nft_client.mint(
                    &beneficiary.clone(),
                    &env.current_contract_address(),
                    &amount,
                    &current_ledger,
                );
            }
        }
        Self::record_stake_snapshot(&env, &beneficiary, new_shares);
        Self::update_leaderboard(&env, &beneficiary, new_shares);

        events::deposit(&env, &beneficiary, amount, shares);

        Ok(shares)
    }

    /// Admin: slash a user's staked principal. Can be called while paused.
    /// `admin_addr` must equal the stored admin address; mismatches return `Unauthorized`.
    /// Returns the actual slashed token amount.
    pub fn slash(env: Env, admin_addr: Address, user: Address, amount: i128) -> Result<i128, VaultError> {
        let stored_admin = admin::get_admin(&env)?;
        if admin_addr != stored_admin {
            return Err(VaultError::Unauthorized);
        }
        admin_addr.require_auth();

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
            Self::remove_from_staker_list(&env, &user);
            events::position_closed(&env, &user);
        }
        Self::record_stake_snapshot(&env, &user, new_user_shares);
        Self::update_leaderboard(&env, &user, new_user_shares);

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

    // --- Time-to-target queries (#49) ---

    /// Read-only estimate of ledgers remaining until `user` accumulates `target_reward` tokens.
    ///
    /// # Formula
    /// `ledgers = ceil(remaining * BOOST_BPS_BASE * STELLAR_LEDGERS_PER_YEAR / (position_amount * boosted_rate_bps))`
    /// where `boosted_rate_bps = rate_bps * tier_mult / 10000 * campaign_mult / 10000`.
    ///
    /// Returns 0 if pending reward already meets or exceeds target.
    /// Returns `u32::MAX` if user has no active position, rate is 0, or effective rate rounds to 0.
    pub fn ledgers_to_target(
        env: Env,
        user: Address,
        target_reward: i128,
    ) -> Result<u32, VaultError> {
        let _ = admin::get_admin(&env)?;
        let pending = Self::pending_reward(&env, &user)?;
        if pending >= target_reward {
            return Ok(0);
        }

        let shares = balance::get_shares(&env, &user);
        if shares == 0 {
            return Ok(u32::MAX);
        }

        let rate_bps = balance::get_reward_rate_bps(&env);
        if rate_bps == 0 {
            return Ok(u32::MAX);
        }

        let current_ledger = env.ledger().sequence();
        let tier_mult = Self::boost_multiplier_for_ledger(&env, &user, current_ledger);

        let campaign_mult: u32 = match env
            .storage()
            .instance()
            .get::<_, CampaignInfo>(&DataKey::BoostCampaign)
        {
            Some(c)
                if current_ledger >= c.starts_at_ledger && current_ledger < c.ends_at_ledger =>
            {
                c.multiplier_bps
            }
            _ => BOOST_BPS_BASE,
        };

        // Match the integer-division order used in reward_for_ledgers
        let effective_rate = (rate_bps as i128)
            .checked_mul(tier_mult as i128)
            .ok_or(VaultError::ArithmeticError)?
            .checked_div(BOOST_BPS_BASE as i128)
            .ok_or(VaultError::ArithmeticError)?;
        let boosted_rate = effective_rate
            .checked_mul(campaign_mult as i128)
            .ok_or(VaultError::ArithmeticError)?
            .checked_div(BOOST_BPS_BASE as i128)
            .ok_or(VaultError::ArithmeticError)?;

        if boosted_rate == 0 {
            return Ok(u32::MAX);
        }

        let total_shares = balance::get_total_shares(&env);
        let total_deposited = balance::get_total_deposited(&env);
        let user_amount = balance::shares_to_amount(total_shares, total_deposited, shares)
            .ok_or(VaultError::ArithmeticError)?;
        if user_amount == 0 {
            return Ok(u32::MAX);
        }

        let denominator = user_amount
            .checked_mul(boosted_rate)
            .ok_or(VaultError::ArithmeticError)?;

        let remaining = target_reward - pending;
        let numerator = remaining
            .checked_mul(BOOST_BPS_BASE as i128)
            .ok_or(VaultError::ArithmeticError)?
            .checked_mul(STELLAR_LEDGERS_PER_YEAR as i128)
            .ok_or(VaultError::ArithmeticError)?;

        // Ceiling division
        let ledgers = numerator
            .checked_add(denominator - 1)
            .ok_or(VaultError::ArithmeticError)?
            .checked_div(denominator)
            .ok_or(VaultError::ArithmeticError)?;

        Ok(if ledgers > u32::MAX as i128 { u32::MAX } else { ledgers as u32 })
    }

    /// Read-only estimate of days remaining until `user` accumulates `target_reward` tokens.
    ///
    /// Uses 5 seconds per ledger (Stellar's approximate close time) and 86 400 seconds per day.
    /// Returns `u32::MAX` when `ledgers_to_target` returns `u32::MAX`.
    pub fn days_to_target(
        env: Env,
        user: Address,
        target_reward: i128,
    ) -> Result<u32, VaultError> {
        let ledgers = Self::ledgers_to_target(env, user, target_reward)?;
        if ledgers == u32::MAX {
            return Ok(u32::MAX);
        }
        // ceil(ledgers * 5 / 86400) — 5 s/ledger, 86400 s/day
        let days = ((ledgers as u64) * 5 + 86399) / 86400;
        Ok(days.min(u32::MAX as u64) as u32)
    }

    // --- Boost campaign (#48) ---

    /// Admin: activate a time-limited reward boost for all stakers.
    ///
    /// The campaign `multiplier_bps` stacks with per-user tier multipliers.
    /// Only one campaign may be active at a time — call `end_boost_campaign` first if one is running.
    pub fn start_boost_campaign(
        env: Env,
        multiplier_bps: u32,
        duration_ledgers: u32,
    ) -> Result<(), VaultError> {
        admin::require_admin(&env)?;

        if multiplier_bps < BOOST_BPS_BASE {
            return Err(VaultError::InvalidBoostSchedule);
        }
        if duration_ledgers == 0 {
            return Err(VaultError::ZeroAmount);
        }

        let current_ledger = env.ledger().sequence();

        if let Some(existing) = env
            .storage()
            .instance()
            .get::<_, CampaignInfo>(&DataKey::BoostCampaign)
        {
            if current_ledger < existing.ends_at_ledger {
                return Err(VaultError::CampaignAlreadyActive);
            }
        }

        let ends_at_ledger = current_ledger.saturating_add(duration_ledgers);
        env.storage().instance().set(
            &DataKey::BoostCampaign,
            &CampaignInfo {
                multiplier_bps,
                starts_at_ledger: current_ledger,
                ends_at_ledger,
            },
        );

        let admin = admin::get_admin(&env)?;
        events::campaign_started(&env, &admin, multiplier_bps, ends_at_ledger);
        Ok(())
    }

    /// Admin: cancel the active boost campaign early.
    pub fn end_boost_campaign(env: Env) -> Result<(), VaultError> {
        admin::require_admin(&env)?;

        if !env.storage().instance().has(&DataKey::BoostCampaign) {
            return Err(VaultError::NoCampaignActive);
        }

        env.storage().instance().remove(&DataKey::BoostCampaign);

        let admin = admin::get_admin(&env)?;
        events::campaign_ended(&env, &admin);
        Ok(())
    }

    /// Read-only: returns `(multiplier_bps, ends_at_ledger)` if a boost campaign is currently active.
    pub fn active_campaign(env: Env) -> Result<Option<(u32, u32)>, VaultError> {
        let _ = admin::get_admin(&env)?;
        let current_ledger = env.ledger().sequence();
        let result = match env
            .storage()
            .instance()
            .get::<_, CampaignInfo>(&DataKey::BoostCampaign)
        {
            Some(c)
                if current_ledger >= c.starts_at_ledger && current_ledger < c.ends_at_ledger =>
            {
                Some((c.multiplier_bps, c.ends_at_ledger))
            }
            _ => None,
        };
        Ok(result)
    }

    // --- Position transfer (#43) ---

    /// Transfer the caller's full staking position to `to`.
    ///
    /// Pending rewards are settled into `from`'s accrued balance before the transfer and remain
    /// claimable by `from` via `claim`. The recipient inherits the lock-up timer (`staked_at_ledger`)
    /// but starts fresh on reward accrual. Recipient must have no active staking position.
    pub fn transfer_position(env: Env, from: Address, to: Address) -> Result<(), VaultError> {
        from.require_auth();
        Self::require_not_paused(&env)?;

        let from_shares = balance::get_shares(&env, &from);
        if from_shares == 0 {
            return Err(VaultError::PositionNotFound);
        }

        let to_shares = balance::get_shares(&env, &to);
        if to_shares > 0 {
            return Err(VaultError::RecipientAlreadyStaking);
        }

        let total_shares = balance::get_total_shares(&env);
        let total_deposited = balance::get_total_deposited(&env);
        let position_amount =
            balance::shares_to_amount(total_shares, total_deposited, from_shares)
                .ok_or(VaultError::ArithmeticError)?;

        // Settle pending rewards so `from` can still claim them after the transfer
        Self::accrue_rewards(&env, &from, from_shares)?;

        let current_ledger = env.ledger().sequence();

        // Transfer shares
        balance::set_shares(&env, &to, from_shares);
        balance::set_shares(&env, &from, 0);

        // Copy lock-up timer to recipient (lock status is inherited)
        let staked_at: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::StakedAtLedger(from.clone()))
            .unwrap_or(current_ledger);
        env.storage()
            .persistent()
            .set(&DataKey::StakedAtLedger(to.clone()), &staked_at);
        env.storage()
            .persistent()
            .remove(&DataKey::StakedAtLedger(from.clone()));

        // Recipient starts fresh on reward accrual
        balance::set_reward_checkpoint_ledger(&env, &to, current_ledger);
        balance::set_last_claim_ledger(&env, &to, current_ledger);
        balance::set_accrued_reward(&env, &to, 0);

        // Advance sender's checkpoint so no further rewards accrue on the transferred shares
        balance::set_reward_checkpoint_ledger(&env, &from, current_ledger);

        // total_shares and total_deposited are unchanged — same tokens, different owner
        // total_stakers is also unchanged — one exits (from), one enters (to)

        // Update governance snapshots for both parties
        Self::record_stake_snapshot(&env, &from, 0);
        Self::record_stake_snapshot(&env, &to, from_shares);

        // Update leaderboard for both parties
        Self::update_leaderboard(&env, &from, 0);
        Self::update_leaderboard(&env, &to, from_shares);

        events::position_transferred(&env, &from, &to, position_amount);
        Ok(())
    }

    // --- Leaderboard (#46) ---

    /// Admin: set the maximum number of entries tracked in the staking leaderboard (max 20).
    ///
    /// Setting `n` to 0 disables leaderboard tracking. Existing entries are trimmed if the
    /// new size is smaller than the current leaderboard length.
    pub fn set_leaderboard_size(env: Env, n: u32) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        if n > 20 {
            return Err(VaultError::LeaderboardSizeTooLarge);
        }
        env.storage().instance().set(&DataKey::LeaderboardSize, &n);

        // Trim existing leaderboard to new size if necessary
        if n > 0 {
            let board: Vec<LeaderboardEntry> = env
                .storage()
                .instance()
                .get(&DataKey::Leaderboard)
                .unwrap_or(Vec::new(&env));
            if board.len() as u32 > n {
                let mut trimmed: Vec<LeaderboardEntry> = Vec::new(&env);
                let mut i = 0u32;
                while i < n {
                    trimmed.push_back(board.get(i).unwrap());
                    i += 1;
                }
                env.storage().instance().set(&DataKey::Leaderboard, &trimmed);
            }
        }
        Ok(())
    }

    /// Read-only: returns the current top stakers sorted descending by position size.
    pub fn get_leaderboard(env: Env) -> Result<Vec<LeaderboardEntry>, VaultError> {
        let _ = admin::get_admin(&env)?;
        Ok(env
            .storage()
            .instance()
            .get(&DataKey::Leaderboard)
            .unwrap_or(Vec::new(&env)))
    }

    // --- Simulation functions (Issue #54) ---

    /// Simulate the reward for staking `amount` tokens for `ledgers` ledger sequences
    /// at the current reward rate and boost multiplier. This is a read-only estimate
    /// and does not modify any state.
    pub fn simulate_stake(env: Env, amount: i128, ledgers: u32) -> Result<i128, VaultError> {
        let _ = admin::get_admin(&env)?;
        let rate_bps = balance::get_reward_rate_bps(&env);
        if rate_bps == 0 {
            return Ok(0);
        }
        let multiplier = BOOST_BPS_BASE;
        Self::reward_for_ledgers(amount, rate_bps, multiplier, BOOST_BPS_BASE, ledgers)
    }

    /// Simulate compounded rewards by claiming every `claim_interval` ledgers
    /// and restaking the reward. Returns the total compounded reward after `ledgers`
    /// ledger sequences. This is a read-only estimate — compounding intervals vary
    /// in practice.
    pub fn simulate_compound(
        env: Env,
        amount: i128,
        ledgers: u32,
        claim_interval: u32,
    ) -> Result<i128, VaultError> {
        let _ = admin::get_admin(&env)?;
        let rate_bps = balance::get_reward_rate_bps(&env);
        if rate_bps == 0 || claim_interval == 0 {
            return Ok(0);
        }

        let multiplier = BOOST_BPS_BASE;
        let mut total_reward: i128 = 0;
        let mut remaining = ledgers;
        let mut current_amount = amount;

        while remaining > 0 {
            let interval = if remaining < claim_interval {
                remaining
            } else {
                claim_interval
            };
            let reward =
                Self::reward_for_ledgers(current_amount, rate_bps, multiplier, BOOST_BPS_BASE, interval)?;
            total_reward = total_reward
                .checked_add(reward)
                .ok_or(VaultError::ArithmeticError)?;
            current_amount = current_amount
                .checked_add(reward)
                .ok_or(VaultError::ArithmeticError)?;
            remaining -= interval;
        }

        Ok(total_reward)
    }

    /// Simulate the difference in rewards with and without the current boost schedule.
    /// Returns `(base_reward, boosted_reward)` for staking `amount` tokens for `ledgers`
    /// ledger sequences. This is a read-only estimate.
    pub fn simulate_boost_impact(
        env: Env,
        amount: i128,
        ledgers: u32,
    ) -> Result<(i128, i128), VaultError> {
        let _ = admin::get_admin(&env)?;
        let rate_bps = balance::get_reward_rate_bps(&env);
        if rate_bps == 0 {
            return Ok((0, 0));
        }

        let base_reward = Self::reward_for_ledgers(amount, rate_bps, BOOST_BPS_BASE, BOOST_BPS_BASE, ledgers)?;

        let schedule = balance::get_boost_schedule(&env).unwrap_or(Vec::new(&env));
        let mut boosted_reward: i128 = 0;
        let mut cursor: u32 = 0;
        let mut current_multiplier = BOOST_BPS_BASE;
        let mut index = 0;

        while index < schedule.len() {
            let (tier_ledger, tier_multiplier) = schedule.get(index).unwrap();
            if tier_ledger <= cursor {
                current_multiplier = tier_multiplier;
                index += 1;
                continue;
            }
            if tier_ledger >= ledgers {
                break;
            }
            let segment = tier_ledger - cursor;
            let segment_reward =
                Self::reward_for_ledgers(amount, rate_bps, current_multiplier, BOOST_BPS_BASE, segment)?;
            boosted_reward = boosted_reward
                .checked_add(segment_reward)
                .ok_or(VaultError::ArithmeticError)?;
            cursor = tier_ledger;
            current_multiplier = tier_multiplier;
            index += 1;
        }

        if cursor < ledgers {
            let segment_reward = Self::reward_for_ledgers(
                amount,
                rate_bps,
                current_multiplier,
                BOOST_BPS_BASE,
                ledgers - cursor,
            )?;
            boosted_reward = boosted_reward
                .checked_add(segment_reward)
                .ok_or(VaultError::ArithmeticError)?;
        }

        Ok((base_reward, boosted_reward))
    }

    // --- Internal helpers ---

    /// Issue #72: shared rate validation used by initialize and set_reward_rate_bps.
    fn validate_rate_bps(rate_bps: u32) -> Result<(), VaultError> {
        if rate_bps > balance::MAX_RATE_BPS {
            return Err(VaultError::RateTooHigh);
        }
        Ok(())
    }

    fn remove_from_staker_list(env: &Env, user: &Address) {
        let stakers = balance::get_all_stakers(env);
        let mut updated = Vec::new(env);
        let mut i = 0;
        while i < stakers.len() {
            let s = stakers.get(i).unwrap();
            if s != *user {
                updated.push_back(s);
            }
            i += 1;
        }
        balance::set_all_stakers(env, &updated);
    }

    fn do_stake(env: &Env, staker: &Address, amount: i128) -> Result<i128, VaultError> {
        staker.require_auth();
        Self::require_not_stopped(env)?;
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

        // Issue #106: KYC enforcement — block unapproved stakers when required.
        // Unstake and claim are intentionally not gated so users can always exit.
        let kyc_required: bool = env
            .storage()
            .instance()
            .get(&DataKey::KycRequired)
            .unwrap_or(false);
        if kyc_required {
            let kyc_approved: bool = env
                .storage()
                .persistent()
                .get(&DataKey::KycApproved(staker.clone()))
                .unwrap_or(false);
            if !kyc_approved {
                return Err(VaultError::KycNotApproved);
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
            balance::set_last_claim_ledger(env, staker, current_ledger);
            let total_stakers = balance::get_total_stakers(env);
            balance::set_total_stakers(env, total_stakers + 1);
            let mut all_stakers = balance::get_all_stakers(env);
            all_stakers.push_back(staker.clone());
            balance::set_all_stakers(env, &all_stakers);
            events::position_opened(env, staker, amount);

            // Issue #41: mark position as restaked if within the grace window
            let restake_window = balance::get_restake_window(env);
            if restake_window > 0 {
                if let Some(last_unstake) = balance::get_last_unstake_ledger(env, staker) {
                    if current_ledger.saturating_sub(last_unstake) <= restake_window {
                        balance::set_restaked(env, staker, true);
                    }
                }
            }

            // Issue #40: mint NFT receipt for the new position
            if let Some(nft_addr) = balance::get_nft_contract(env) {
                let nft_client = StakeReceiptNFTClient::new(env, &nft_addr);
                nft_client.mint(
                    &staker.clone(),
                    &env.current_contract_address(),
                    &amount,
                    &current_ledger,
                );
            }
        }
        Self::record_stake_snapshot(env, staker, new_shares);
        Self::update_leaderboard(env, staker, new_shares);
        Self::append_stake_history(env, staker, StakeAction::Stake, amount);

        events::deposit(env, staker, amount, shares);
        balance::set_last_updated_ledger(env, env.ledger().sequence()); // Issue #69

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

        let lock_period: u32 = env
            .storage()
            .instance()
            .get(&DataKey::LockPeriod)
            .unwrap_or(0);
        // Must be read as u32 to match how `set_early_exit_penalty_bps` stores
        // it; an inferred `i32` would panic on deserialization once a penalty
        // is configured.
        let penalty_bps: u32 = env
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

        // Issue #41: restaked positions are exempt from early-exit penalty for one unstake cycle
        // Issue #41: restaked positions are exempt from early-exit penalty for one unstake cycle
        let is_restaked = balance::is_restaked(env, staker);
        let amount_after_penalty = if is_restaked || !is_locked || penalty_bps == 0 {
            amount
        } else {
            let penalty = amount
                .checked_mul(penalty_bps as i128)
                .ok_or(VaultError::ArithmeticError)?
                .checked_div(BOOST_BPS_BASE as i128)
                .ok_or(VaultError::ArithmeticError)?;
            amount - penalty
        };

        // Unstake fee: charged on the post-penalty amount returned to the user
        // and routed to the reward pool treasury (not burned). Applied after the
        // lock-up penalty so both can be active simultaneously.
        let unstake_fee_bps = balance::get_unstake_fee_bps(env);
        let unstake_fee = if unstake_fee_bps > 0 {
            amount_after_penalty
                .checked_mul(unstake_fee_bps as i128)
                .ok_or(VaultError::ArithmeticError)?
                .checked_div(BOOST_BPS_BASE as i128)
                .ok_or(VaultError::ArithmeticError)?
        } else {
            0
        };
        let amount_returned = amount_after_penalty - unstake_fee;

        let new_user_shares = user_shares - shares;
        balance::set_shares(env, staker, new_user_shares);
        balance::set_total_shares(env, total_shares - shares);
        // Both the returned principal and the fee leave the staked pool; the fee
        // is credited to the reward treasury below.
        balance::set_total_deposited(env, total_deposited - amount_returned - unstake_fee);

        if unstake_fee > 0 {
            let reward_pool = balance::get_reward_pool_balance(env);
            balance::set_reward_pool_balance(env, reward_pool + unstake_fee);
        }

        if new_user_shares == 0 {
            env.storage()
                .persistent()
                .remove(&DataKey::StakedAtLedger(staker.clone()));
            // Issue #41: record the ledger of this full unstake and clear restaked flag
            balance::set_last_unstake_ledger(env, staker, current_ledger);
            balance::remove_restaked(env, staker);
            let total_stakers = balance::get_total_stakers(env);
            if total_stakers > 0 {
                balance::set_total_stakers(env, total_stakers - 1);
            }
            Self::remove_from_staker_list(env, staker);
            events::position_closed(env, staker);

            // Issue #40: burn NFT receipt on full unstake
            if let Some(nft_addr) = balance::get_nft_contract(env) {
                let nft_client = StakeReceiptNFTClient::new(env, &nft_addr);
                nft_client.burn(&staker.clone());
            }
        }
        Self::record_stake_snapshot(env, staker, new_user_shares);
        Self::update_leaderboard(env, staker, new_user_shares);
        Self::append_stake_history(env, staker, StakeAction::Unstake, amount_returned);

        let token_client = token::Client::new(env, &token_addr);
        token_client.transfer(&env.current_contract_address(), staker, &amount_returned);

        events::withdraw(env, staker, shares, amount_returned);
        balance::set_last_updated_ledger(env, env.ledger().sequence()); // Issue #69

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
            false,
        )?;

        accrued
            .checked_add(pending_since_checkpoint)
            .ok_or(VaultError::ArithmeticError)
    }

    fn accrue_rewards(env: &Env, user: &Address, current_shares: i128) -> Result<(), VaultError> {
        let current_ledger = env.ledger().sequence();
        let checkpoint = balance::get_reward_checkpoint_ledger(env, user).unwrap_or(current_ledger);
        let additional_reward =
            Self::reward_between_ledgers(env, user, current_shares, checkpoint, current_ledger, true)?;

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
        persist: bool,
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

        // Load campaign once so reward_for_ledgers can split at campaign boundaries (#48)
        let campaign: Option<CampaignInfo> = env.storage().instance().get(&DataKey::BoostCampaign);

        let schedule = balance::get_boost_schedule(env).unwrap_or(Vec::new(env));
        let mut reward: i128 = 0;
        let mut total_dust: i128 = 0;
        let mut cursor = start_ledger;
        let mut current_multiplier = BOOST_BPS_BASE;
        let mut tier_index = 0u32;

        // Advance past boost tiers already fully elapsed at start_ledger
        while tier_index < schedule.len() {
            let (tier_ledger, tier_mult) = schedule.get(tier_index).unwrap();
            let threshold = staked_at.saturating_add(tier_ledger);
            if threshold <= cursor {
                current_multiplier = tier_mult;
                tier_index += 1;
            } else {
                break;
            }
        }

        // Walk segments split by BOTH boost-tier boundaries and campaign boundaries
        while cursor < end_ledger {
            let next_tier_boundary = if tier_index < schedule.len() {
                let (tier_ledger, _) = schedule.get(tier_index).unwrap();
                staked_at.saturating_add(tier_ledger)
            } else {
                u32::MAX
            };

            let (campaign_mult, next_campaign_boundary) =
                Self::campaign_info_at(cursor, &campaign);

            let seg_end = next_tier_boundary
                .min(next_campaign_boundary)
                .min(end_ledger);

            if seg_end > cursor {
                reward = reward
                    .checked_add(Self::reward_for_ledgers(
                        current_shares,
                        rate_bps,
                        current_multiplier,
                        campaign_mult,
                        seg_end - cursor,
                    )?)
                    .ok_or(VaultError::ArithmeticError)?;
            }

            let segment_dust = Self::reward_dust_for_ledgers(
                current_shares,
                rate_bps,
                current_multiplier,
                seg_end - cursor,
            )?;
            total_dust = total_dust
                .checked_add(segment_dust)
                .ok_or(VaultError::ArithmeticError)?;

            // Advance tier multiplier when we land exactly on a tier boundary
            if seg_end == next_tier_boundary && tier_index < schedule.len() {
                let (_, tier_mult) = schedule.get(tier_index).unwrap();
                current_multiplier = tier_mult;
                tier_index += 1;
            }

            cursor = seg_end;
        }

        let final_segment_dust = Self::reward_dust_for_ledgers(
            current_shares,
            rate_bps,
            current_multiplier,
            end_ledger - cursor,
        )?;
        total_dust = total_dust
            .checked_add(final_segment_dust)
            .ok_or(VaultError::ArithmeticError)?;

        let divisor = (BOOST_BPS_BASE as i128)
            .checked_mul(STELLAR_LEDGERS_PER_YEAR as i128)
            .ok_or(VaultError::ArithmeticError)?;

        let current_remainder = balance::get_reward_remainder(env, user);
        let total_dust_with_remainder = total_dust
            .checked_add(current_remainder)
            .ok_or(VaultError::ArithmeticError)?;

        let reward = total_dust_with_remainder
            .checked_div(divisor)
            .ok_or(VaultError::ArithmeticError)?;

        let new_remainder = total_dust_with_remainder
            .checked_rem(divisor)
            .ok_or(VaultError::ArithmeticError)?;

        if persist {
            balance::set_reward_remainder(env, user, new_remainder);
        }

        Ok(reward)
    }

    /// Calculate reward dust (numerator before division by BOOST_BPS_BASE * STELLAR_LEDGERS_PER_YEAR).
    ///
    /// ROUNDING BEHAVIOR WARNING:
    /// In traditional fixed-point math, calculating reward using standard division leads to severe
    /// rounding loss where small stakes over short periods truncate to 0. For example, with an amount
    /// of 100 shares, a rate of 100 bps (1%), and 300 elapsed ledgers, the reward calculation is:
    /// reward = (100 * 100 * 300) / (10,000 * 6,307,200) = 3,000,000 / 63,072,000_000 = 0.
    /// To mitigate this value loss, we track the sub-unit remainder (dust) per-user and carry it forward.
    fn reward_dust_for_ledgers(
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
            .ok_or(VaultError::ArithmeticError)
    }

    fn reward_for_ledgers(
        amount: i128,
        rate_bps: u32,
        multiplier_bps: u32,
        campaign_multiplier_bps: u32,
        elapsed_ledgers: u32,
    ) -> Result<i128, VaultError> {
        if elapsed_ledgers == 0 || amount == 0 {
            return Ok(0);
        }

        // Apply tier multiplier: effective_rate = rate_bps * tier_mult / 10000
        let effective_rate_bps = (rate_bps as i128)
            .checked_mul(multiplier_bps as i128)
            .ok_or(VaultError::ArithmeticError)?
            .checked_div(BOOST_BPS_BASE as i128)
            .ok_or(VaultError::ArithmeticError)?;

        // Stack campaign multiplier: boosted_rate = effective_rate * campaign_mult / 10000
        let boosted_rate_bps = effective_rate_bps
            .checked_mul(campaign_multiplier_bps as i128)
            .ok_or(VaultError::ArithmeticError)?
            .checked_div(BOOST_BPS_BASE as i128)
            .ok_or(VaultError::ArithmeticError)?;

        amount
            .checked_mul(boosted_rate_bps)
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

    /// Returns `(campaign_multiplier_bps, next_boundary_ledger)` for a given cursor position.
    fn campaign_info_at(cursor: u32, campaign: &Option<CampaignInfo>) -> (u32, u32) {
        match campaign {
            Some(c) if cursor >= c.starts_at_ledger && cursor < c.ends_at_ledger => {
                (c.multiplier_bps, c.ends_at_ledger)
            }
            Some(c) if cursor < c.starts_at_ledger => (BOOST_BPS_BASE, c.starts_at_ledger),
            _ => (BOOST_BPS_BASE, u32::MAX),
        }
    }

    /// Normalize a reward amount from stake-token precision to reward-token precision.
    fn normalize_to_reward_decimals(env: &Env, amount: i128) -> Result<i128, VaultError> {
        let stake_dec = balance::get_stake_decimals(env);
        let reward_dec = balance::get_reward_decimals(env);
        if stake_dec == reward_dec {
            return Ok(amount);
        }
        if reward_dec > stake_dec {
            let factor = 10i128.pow(reward_dec - stake_dec);
            amount.checked_mul(factor).ok_or(VaultError::ArithmeticError)
        } else {
            let factor = 10i128.pow(stake_dec - reward_dec);
            Ok(amount / factor)
        }
    }

    /// Update the on-chain leaderboard after a stake or unstake operation (#46).
    ///
    /// Rebuilds the sorted `Vec<LeaderboardEntry>` (descending by amount) removing the old entry
    /// for `user` and reinserting at the correct position with their current position size.
    /// No-op when `LeaderboardSize` is 0 (leaderboard tracking disabled).
    fn update_leaderboard(env: &Env, user: &Address, new_shares: i128) {
        let max_size: u32 = env
            .storage()
            .instance()
            .get(&DataKey::LeaderboardSize)
            .unwrap_or(0);
        if max_size == 0 {
            return;
        }

        let total_shares = balance::get_total_shares(env);
        let total_deposited = balance::get_total_deposited(env);
        let new_amount = if new_shares == 0 || total_shares == 0 {
            0i128
        } else {
            balance::shares_to_amount(total_shares, total_deposited, new_shares).unwrap_or(0)
        };

        let old_board: Vec<LeaderboardEntry> = env
            .storage()
            .instance()
            .get(&DataKey::Leaderboard)
            .unwrap_or(Vec::new(env));

        // Rebuild board excluding the user's existing entry
        let mut board: Vec<LeaderboardEntry> = Vec::new(env);
        let mut i = 0u32;
        while i < old_board.len() {
            let entry = old_board.get(i).unwrap();
            if entry.staker != *user {
                board.push_back(entry);
            }
            i += 1;
        }

        if new_amount > 0 {
            let board_len = board.len();
            // Qualifies if board has room or user beats the last entry
            let qualifies = (board_len as u32) < max_size || {
                if board_len > 0 {
                    new_amount > board.get(board_len - 1).unwrap().amount
                } else {
                    true
                }
            };

            if qualifies {
                // Find insertion point (sorted descending by amount)
                let mut insert_idx = board.len();
                let mut j = 0u32;
                while j < board.len() {
                    if new_amount > board.get(j).unwrap().amount {
                        insert_idx = j;
                        break;
                    }
                    j += 1;
                }

                // Rebuild with the new entry inserted at insert_idx
                let mut final_board: Vec<LeaderboardEntry> = Vec::new(env);
                let mut k = 0u32;
                while k < board.len() {
                    if k == insert_idx {
                        final_board.push_back(LeaderboardEntry {
                            staker: user.clone(),
                            amount: new_amount,
                        });
                    }
                    final_board.push_back(board.get(k).unwrap());
                    k += 1;
                }
                if insert_idx == board.len() {
                    final_board.push_back(LeaderboardEntry {
                        staker: user.clone(),
                        amount: new_amount,
                    });
                }

                // Trim to max_size
                while (final_board.len() as u32) > max_size {
                    final_board.pop_back();
                }

                env.storage().instance().set(&DataKey::Leaderboard, &final_board);
                return;
            }
        }

        env.storage().instance().set(&DataKey::Leaderboard, &board);
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

    /// Issue #107: returns `ContractStopped` if the emergency stop has been triggered.
    fn require_not_stopped(env: &Env) -> Result<(), VaultError> {
        let stopped: bool = env
            .storage()
            .instance()
            .get(&DataKey::Stopped)
            .unwrap_or(false);
        if stopped {
            Err(VaultError::ContractStopped)
        } else {
            Ok(())
        }
    }

    /// Append one entry to the user's stake/unstake history ring buffer (max 5).
    fn append_stake_history(env: &Env, user: &Address, action: StakeAction, amount: i128) {
        // Uses a tuple key to avoid collision with DataKey::StakeHistory used for
        // governance vote-weight snapshots (Vec<(u32, i128)> vs Vec<StakeHistoryEntry>).
        let key = (soroban_sdk::Symbol::new(env, "stkh"), user.clone());
        let mut history: Vec<StakeHistoryEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));

        if history.len() >= MAX_STAKE_HISTORY {
            history.remove(0);
        }
        history.push_back(StakeHistoryEntry {
            action,
            amount,
            ledger: env.ledger().sequence(),
        });
        env.storage().persistent().set(&key, &history);
    }

    // ── Inner claim helper (no require_auth) ──────────────────────────────────

    /// Core claim logic shared by `claim` and `stake_and_claim`.
    ///
    /// Accrues rewards, applies the optional claim cap, transfers tokens, and
    /// emits the `claimed` event. Does NOT call `require_auth` — callers are
    /// responsible for gating access.
    fn do_claim(env: &Env, staker: &Address) -> Result<i128, VaultError> {
        let current_shares = balance::get_shares(env, staker);
        Self::accrue_rewards(env, staker, current_shares)?;

        let accrued = balance::get_accrued_reward(env, staker);
        if accrued == 0 {
            balance::set_last_claim_ledger(env, staker, env.ledger().sequence());
            balance::set_last_updated_ledger(env, env.ledger().sequence()); // Issue #69
            return Ok(0);
        }

        // Apply per-user claim cap if configured (issue #78).
        let reward = Self::apply_claim_cap(env, staker, accrued)?;
        if reward == 0 {
            // Cap is exhausted for this window; nothing to pay out now.
            balance::set_last_claim_ledger(env, staker, env.ledger().sequence());
            balance::set_last_updated_ledger(env, env.ledger().sequence()); // Issue #69
            return Ok(0);
        }

        let reward_pool = balance::get_reward_pool_balance(env);
        if reward_pool < reward {
            return Err(VaultError::InsufficientRewardPool);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;

        let token_client = token::Client::new(env, &token_addr);
        token_client.transfer(&env.current_contract_address(), staker, &reward);

        balance::set_reward_pool_balance(env, reward_pool - reward);
        // Reduce accrued by the amount paid; cap-deferred remainder stays in accrued.
        let remaining_accrued = accrued
            .checked_sub(reward)
            .ok_or(VaultError::ArithmeticError)?;
        balance::set_accrued_reward(env, staker, remaining_accrued);
        balance::set_last_claim_ledger(env, staker, env.ledger().sequence());

        let paid = balance::get_total_rewards_paid(env);
        balance::set_total_rewards_paid(env, paid + reward);

        events::claimed(env, staker, reward);
        balance::set_last_updated_ledger(env, env.ledger().sequence()); // Issue #69

        Ok(reward)
    }

    // ── Inner stake helper (no require_auth) ──────────────────────────────────

    /// Core stake logic shared by `do_stake` and `stake_and_claim`.
    ///
    /// Performs all the same side-effects as `do_stake` (pool cap check, share
    /// minting, event emission) without calling `require_auth`. Callers must
    /// have already authenticated the staker.
    fn do_stake_inner(env: &Env, staker: &Address, amount: i128) -> Result<i128, VaultError> {
        Self::require_not_stopped(env)?;
        Self::require_not_paused(env)?;

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
            balance::set_last_claim_ledger(env, staker, current_ledger);
            let total_stakers = balance::get_total_stakers(env);
            balance::set_total_stakers(env, total_stakers + 1);
            let mut all_stakers = balance::get_all_stakers(env);
            all_stakers.push_back(staker.clone());
            balance::set_all_stakers(env, &all_stakers);
            events::position_opened(env, staker, amount);
        }
        Self::record_stake_snapshot(env, staker, new_shares);

        events::deposit(env, staker, amount, shares);

        Ok(shares)
    }

    // ── Claim cap enforcement (issue #78) ─────────────────────────────────────

    /// Apply the per-user rolling claim cap and return the payable reward.
    ///
    /// If the cap is disabled (max_amount == 0), returns `full_reward` unchanged.
    /// Otherwise checks the user's `ClaimWindow`, resets it if the window has
    /// expired, and returns `min(full_reward, remaining_headroom)`. The window
    /// state is updated to reflect whatever will be paid out.
    fn apply_claim_cap(env: &Env, user: &Address, full_reward: i128) -> Result<i128, VaultError> {
        let max_amount = balance::get_claim_cap(env);
        if max_amount == 0 {
            return Ok(full_reward);
        }

        let window_ledgers = balance::get_claim_cap_window(env);
        let current_ledger = env.ledger().sequence();

        let mut window = balance::get_user_claim_window(env, user).unwrap_or(ClaimWindow {
            claimed_in_window: 0,
            window_started_at: current_ledger,
        });

        // Reset window if it has expired.
        if window_ledgers > 0
            && current_ledger > window.window_started_at.saturating_add(window_ledgers)
        {
            window = ClaimWindow {
                claimed_in_window: 0,
                window_started_at: current_ledger,
            };
        }

        let headroom = max_amount
            .checked_sub(window.claimed_in_window)
            .unwrap_or(0)
            .max(0);

        let payable = full_reward.min(headroom);

        if payable > 0 {
            window.claimed_in_window = window
                .claimed_in_window
                .checked_add(payable)
                .ok_or(VaultError::ArithmeticError)?;
            balance::set_user_claim_window(env, user, &window);
        }

        Ok(payable)
    }
    // --- Issue #100: paginated admin query over all positions ---

    /// Admin-only paginated query over all registered staking positions.
    ///
    /// Returns up to `page_size` `(Address, StakePosition)` pairs in insertion
    /// order (first-stake first). `page` is zero-indexed. Reverts with
    /// `PageSizeTooLarge` when `page_size > 20` to cap per-call compute.
    /// Returns an empty vec when `page` is past the last page. Users with
    /// zero shares are skipped silently. Admin auth required.
    pub fn view_all_positions(
        env: Env,
        page: u32,
        page_size: u32,
    ) -> Result<Vec<(Address, StakePosition)>, VaultError> {
        admin::require_admin(&env)?;
        if page_size == 0 || page_size > 20 {
            return Err(VaultError::PageSizeTooLarge);
        }

        let all_stakers = balance::get_all_stakers(&env);
        let start = page * page_size;
        let mut results: Vec<(Address, StakePosition)> = Vec::new(&env);

        let mut i = start;
        while i < all_stakers.len() && i < start + page_size {
            let user = all_stakers.get(i).unwrap();
            if let Some(pos) = Self::build_position(&env, &user)? {
                results.push_back((user, pos));
            }
            i += 1;
        }

        Ok(results)
    }

    // --- Issue #101: frozen position mechanism ---

    /// Admin: set the inactivity threshold in ledgers.
    ///
    /// Positions that have not claimed or updated in more than this many ledgers
    /// since their last activity can be flagged by `flag_frozen`. Pass `0` to
    /// disable the threshold (threshold is informational only — no automatic
    /// freezing occurs).
    pub fn set_inactivity_threshold(env: Env, admin: Address, ledgers: u32) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        let _ = admin;
        env.storage().instance().set(&DataKey::InactivityThreshold, &ledgers);
        Ok(())
    }

    /// Admin: mark a user's position as frozen.
    ///
    /// Freezing is informational only — it does not block stake, unstake, or
    /// claim operations. Emits `FrozenPosition` with the current ledger.
    /// Reverts with `PositionNotFound` when the user has no active stake.
    pub fn flag_frozen(env: Env, admin: Address, user: Address) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        let _ = admin;
        if balance::get_shares(&env, &user) == 0 {
            return Err(VaultError::PositionNotFound);
        }
        let frozen_at = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::FrozenAt(user.clone()), &frozen_at);
        let admin_addr = admin::get_admin(&env)?;
        events::frozen_position(&env, &admin_addr, &user, frozen_at);
        Ok(())
    }

    /// Read-only: returns `true` when the user's position carries a frozen flag.
    pub fn is_frozen(env: Env, user: Address) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::FrozenAt(user))
    }

    /// Admin: remove the frozen flag from a user's position.
    pub fn unfreeze(env: Env, admin: Address, user: Address) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        let _ = admin;
        env.storage()
            .persistent()
            .remove(&DataKey::FrozenAt(user));
        Ok(())
    }

    // --- Issue #102: reward_per_token_per_ledger metric ---

    /// Read-only metric: reward earned per staked token per ledger at the current rate.
    ///
    /// Returns `reward_rate_bps / (10_000 * STELLAR_LEDGERS_PER_YEAR)`.
    ///
    /// Note: integer division causes this to truncate to 0 for all practical
    /// rate values (e.g. 500 bps / 63_072_000_000 = 0). Callers that need
    /// sub-integer precision should multiply the rate by the position size
    /// first, then divide. Returns 0 when rate is zero or total staked is zero.
    /// No auth required.
    pub fn reward_per_token_per_ledger(env: Env) -> i128 {
        let rate_bps = balance::get_reward_rate_bps(&env);
        if rate_bps == 0 {
            return 0;
        }
        let total_staked = balance::get_total_deposited(&env);
        if total_staked == 0 {
            return 0;
        }
        (rate_bps as i128)
            / (BOOST_BPS_BASE as i128 * STELLAR_LEDGERS_PER_YEAR as i128)
    }

    // --- Issue #103: user_summary aggregated query ---

    /// Read-only aggregate: returns the user's position, pending reward, and
    /// pool-share fraction (in basis points) in a single contract call.
    ///
    /// `pool_share_bps` is `user_shares * 10_000 / total_shares` (0 when no
    /// shares exist globally). Returns `UserSummary { position: None,
    /// pending_reward: 0, pool_share_bps: 0 }` for users with no stake.
    /// No auth required.
    pub fn user_summary(env: Env, user: Address) -> Result<UserSummary, VaultError> {
        let position = Self::build_position(&env, &user)?;
        let pending_reward = Self::pending_reward(&env, &user)?;
        let user_shares = balance::get_shares(&env, &user);
        let total_shares = balance::get_total_shares(&env);
        let pool_share_bps = if total_shares == 0 || user_shares == 0 {
            0
        } else {
            user_shares
                .checked_mul(BOOST_BPS_BASE as i128)
                .unwrap_or(0)
                .checked_div(total_shares)
                .unwrap_or(0)
        };
        Ok(UserSummary {
            position: match position {
                Some(p) => OptionalPosition::Some(p),
                None => OptionalPosition::None,
            },
            pending_reward,
            pool_share_bps,
        })
    }

    // ── Issue #105: stake history ─────────────────────────────────────────────

    /// Returns the last (up to 5) stake/unstake actions for `user`.
    ///
    /// Returns an empty vec for a user who has never staked. No auth required.
    pub fn stake_history(env: Env, user: Address) -> Vec<StakeHistoryEntry> {
        let key = (soroban_sdk::Symbol::new(&env, "stkh"), user);
        env.storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env))
    }

    // ── Issue #104: interface detection ──────────────────────────────────────

    /// The compile-time set of interfaces this deployment supports.
    ///
    /// `Base` is always present. `Lockup` and `Whitelist` are supported because
    /// the vault includes lock-period and whitelist features. `Compounding`,
    /// `EpochMode`, and `VestingSchedule` are NOT included in this build.
    const SUPPORTED_INTERFACES: &'static [InterfaceId] = &[
        InterfaceId::Base,
        InterfaceId::Lockup,
        InterfaceId::Whitelist,
    ];

    /// Returns `true` if this contract deployment supports the given interface.
    ///
    /// Callers can use this for on-chain feature detection before invoking
    /// optional functions. No auth required, no state changes.
    pub fn supports_interface(_env: Env, interface: InterfaceId) -> bool {
        Self::SUPPORTED_INTERFACES.contains(&interface)
    }

    // ── Issue #106: KYC enforcement ───────────────────────────────────────────

    /// Toggle global KYC enforcement on or off (admin only).
    ///
    /// When `required` is `true`, only addresses marked approved via
    /// `set_kyc_status` may call `stake`. Existing positions are unaffected —
    /// users can always unstake and claim regardless of KYC status.
    pub fn set_kyc_required(env: Env, required: bool) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        env.storage()
            .instance()
            .set(&DataKey::KycRequired, &required);
        Ok(())
    }

    /// Approve or revoke KYC status for a specific address (admin only).
    ///
    /// Revoking KYC does not auto-unstake an existing position — it only
    /// prevents the user from adding new stake while KYC enforcement is on.
    pub fn set_kyc_status(env: Env, user: Address, approved: bool) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        env.storage()
            .persistent()
            .set(&DataKey::KycApproved(user), &approved);
        Ok(())
    }

    /// Returns `true` if `user` has been marked KYC-approved by the admin.
    ///
    /// Note: returns `false` when KYC enforcement is off — query
    /// `kyc_required` separately if you need to distinguish these cases.
    /// No auth required.
    pub fn is_kyc_approved(env: Env, user: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::KycApproved(user))
            .unwrap_or(false)
    }

    // ── Issue #107: permanent emergency stop ──────────────────────────────────

    /// Permanently freeze the contract — no new stakes will ever be accepted.
    ///
    /// **This action is irreversible.** Once triggered, `stake` is permanently
    /// blocked, and `pause`/`unpause` both revert with `ContractStopped`.
    /// `unstake` and `claim` continue to work so all users can exit safely.
    ///
    /// Emits the `stopped` event. Can be called even when the contract is
    /// already paused. Admin only.
    pub fn emergency_stop(env: Env) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        env.storage().instance().set(&DataKey::Stopped, &true);
        let admin = admin::get_admin(&env)?;
        events::stopped(&env, &admin);
        Ok(())
    }

    /// Returns `true` if the contract has been permanently stopped.
    ///
    /// No auth required.
    pub fn is_stopped(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Stopped)
            .unwrap_or(false)
    }

    // ── Issue #98: can_unstake pre-flight check ────────────────────────────────

    /// Read-only pre-flight check that simulates whether an unstake of the given
    /// `amount` (in token units) would succeed for `user`, without modifying
    /// any state or requiring authentication.
    ///
    /// Mirrors the exact same checks as `do_unstake` in the same order so the
    /// result accurately reflects what would happen on-chain.
    pub fn can_unstake(env: Env, user: Address, amount: i128) -> UnstakeCheckResult {
        let paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if paused {
            return UnstakeCheckResult::PoolPaused;
        }

        let user_shares = balance::get_shares(&env, &user);
        if user_shares == 0 {
            return UnstakeCheckResult::NoPosition;
        }

        if amount <= 0 {
            return UnstakeCheckResult::InsufficientAmount;
        }

        if let Some(limit) = balance::get_withdrawal_limit(&env) {
            if amount > limit {
                return UnstakeCheckResult::InsufficientAmount;
            }
        }

        if user_shares < amount {
            return UnstakeCheckResult::InsufficientAmount;
        }

        let lock_period: u32 = env
            .storage()
            .instance()
            .get(&DataKey::LockPeriod)
            .unwrap_or(0);
        if lock_period > 0 {
            let staked_at: u32 = env
                .storage()
                .persistent()
                .get(&DataKey::StakedAtLedger(user.clone()))
                .unwrap_or(0);
            let current_ledger = env.ledger().sequence();
            if current_ledger < staked_at.saturating_add(lock_period) {
                return UnstakeCheckResult::StillLocked;
            }
        }

        UnstakeCheckResult::Ok
    }

    // ── Issue #97: pool description ────────────────────────────────────────────

    /// Admin: set or update the on-chain pool description.
    ///
    /// The description is stored as a `soroban_sdk::String` in instance storage
    /// and can be queried via `get_pool_description`. Maximum length is 200
    /// characters — reverts with `DescriptionTooLong` if exceeded.
    ///
    /// Emits a `description_updated` event on every change.
    pub fn set_pool_description(
        env: Env,
        admin: Address,
        description: soroban_sdk::String,
    ) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        let _ = admin;

        if description.len() > 200 {
            return Err(VaultError::DescriptionTooLong);
        }

        balance::set_pool_description(&env, &description);
        let admin_addr = admin::get_admin(&env)?;
        events::description_updated(&env, &admin_addr, &description);
        Ok(())
    }

    /// Read-only query for the pool description.
    ///
    /// Returns `None` if no description has been set yet. No auth required.
    pub fn get_pool_description(env: Env) -> Option<soroban_sdk::String> {
        balance::get_pool_description(&env)
    }

    // ── Issue #96: percentage_of_pool ──────────────────────────────────────────

    /// Read-only query that returns the user's staked amount as a percentage of
    /// the total pool, expressed in basis points (10 000 = 100%).
    ///
    /// Formula: `(user_staked * 10_000) / total_staked`. Integer arithmetic
    /// truncates — see doc comment. Returns 0 if the user has no position or
    /// total staked is 0. No auth required.
    pub fn percentage_of_pool(env: Env, user: Address) -> i128 {
        let user_shares = balance::get_shares(&env, &user);
        if user_shares == 0 {
            return 0;
        }

        let total_shares = balance::get_total_shares(&env);
        let total_deposited = balance::get_total_deposited(&env);
        if total_shares == 0 || total_deposited == 0 {
            return 0;
        }

        let user_amount = match balance::shares_to_amount(total_shares, total_deposited, user_shares)
        {
            Some(a) => a,
            None => return 0,
        };

        user_amount
            .checked_mul(BOOST_BPS_BASE as i128)
            .unwrap_or(0)
            .checked_div(total_deposited)
            .unwrap_or(0)
    }

    // ── Issue #99: staking streak tracker ──────────────────────────────────────

    /// Admin: record which users were active in a completed Wave.
    ///
    /// `wave_id` must be monotonically increasing (greater than the last
    /// recorded wave_id). Users present in consecutive calls have their
    /// `current_streak` incremented; users absent from a wave have their
    /// streak reset to 0. `longest_streak` is never decremented.
    ///
    /// Maximum 50 active users per call to bound compute cost.
    /// Reverts with `NonMonotonicWaveId` or `TooManyActiveUsers` on violation.
    pub fn record_wave_activity(
        env: Env,
        admin: Address,
        wave_id: u32,
        active_users: Vec<Address>,
    ) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        let _ = admin;

        if active_users.len() > 50 {
            return Err(VaultError::TooManyActiveUsers);
        }

        let last_wave = balance::get_last_recorded_wave(&env).unwrap_or(0);
        if wave_id <= last_wave {
            return Err(VaultError::NonMonotonicWaveId);
        }

        // Reset streaks for all existing stakers who are NOT in active_users
        let all_stakers = balance::get_all_stakers(&env);
        let mut i = 0u32;
        while i < all_stakers.len() {
            let staker = all_stakers.get(i).unwrap();
            let mut found = false;
            let mut j = 0u32;
            while j < active_users.len() {
                if active_users.get(j).unwrap() == staker {
                    found = true;
                    break;
                }
                j += 1;
            }
            if !found {
                let mut streak =
                    balance::get_user_streak(&env, &staker).unwrap_or(StakeStreak {
                        current_streak: 0,
                        longest_streak: 0,
                        last_active_wave: 0,
                    });
                if streak.current_streak > 0 {
                    streak.current_streak = 0;
                    balance::set_user_streak(&env, &staker, &streak);
                }
            }
            i += 1;
        }

        // Update streaks for active users
        i = 0;
        while i < active_users.len() {
            let user = active_users.get(i).unwrap();
            let mut streak = balance::get_user_streak(&env, &user).unwrap_or(StakeStreak {
                current_streak: 0,
                longest_streak: 0,
                last_active_wave: 0,
            });

            if last_wave > 0 && streak.last_active_wave == last_wave {
                streak.current_streak += 1;
            } else {
                streak.current_streak = 1;
            }

            if streak.current_streak > streak.longest_streak {
                streak.longest_streak = streak.current_streak;
            }
            streak.last_active_wave = wave_id;

            balance::set_user_streak(&env, &user, &streak);
            i += 1;
        }

        balance::set_last_recorded_wave(&env, wave_id);
        Ok(())
    }

    /// Read-only query for a user's staking streak.
    ///
    /// Returns a `StakeStreak` with `current_streak`, `longest_streak`, and
    /// `last_active_wave`. Returns default (all zeros) if no streak data exists.
    /// No auth required.
    pub fn get_streak(env: Env, user: Address) -> StakeStreak {
        balance::get_user_streak(&env, &user).unwrap_or(StakeStreak {
            current_streak: 0,
            longest_streak: 0,
            last_active_wave: 0,
        })
    }

}