use soroban_sdk::{contract, contractimpl, Address, Env, token};

use crate::{
    admin,
    balance,
    errors::VaultError,
    events,
    storage::DataKey,
};

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
        Ok(())
    }

    /// Deposit `amount` of the vault token. Returns shares minted to caller.
    pub fn deposit(env: Env, depositor: Address, amount: i128) -> Result<i128, VaultError> {
        depositor.require_auth();
        Self::require_not_paused(&env)?;

        if amount <= 0 {
            return Err(VaultError::ZeroAmount);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;

        let total_shares = balance::get_total_shares(&env);
        let total_deposited = balance::get_total_deposited(&env);

        let shares = balance::amount_to_shares(total_shares, total_deposited, amount)
            .ok_or(VaultError::ArithmeticError)?;

        // Transfer tokens from depositor to vault
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&depositor, &env.current_contract_address(), &amount);

        // Mint shares
        let current_shares = balance::get_shares(&env, &depositor);
        balance::set_shares(&env, &depositor, current_shares + shares);
        balance::set_total_shares(&env, total_shares + shares);
        balance::set_total_deposited(&env, total_deposited + amount);

        events::deposit(&env, &depositor, amount, shares);

        Ok(shares)
    }

    /// Withdraw by burning `shares`. Returns underlying token amount returned.
    pub fn withdraw(env: Env, withdrawer: Address, shares: i128) -> Result<i128, VaultError> {
        withdrawer.require_auth();
        Self::require_not_paused(&env)?;

        if shares <= 0 {
            return Err(VaultError::ZeroAmount);
        }

        if let Some(limit) = balance::get_withdrawal_limit(&env) {
            if shares > limit {
                return Err(VaultError::WithdrawalLimitExceeded);
            }
        }

        let user_shares = balance::get_shares(&env, &withdrawer);
        if user_shares < shares {
            return Err(VaultError::InsufficientShares);
        }

        let total_shares = balance::get_total_shares(&env);
        let total_deposited = balance::get_total_deposited(&env);

        let amount = balance::shares_to_amount(total_shares, total_deposited, shares)
            .ok_or(VaultError::ArithmeticError)?;

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(VaultError::NotInitialized)?;

        // Burn shares
        balance::set_shares(&env, &withdrawer, user_shares - shares);
        balance::set_total_shares(&env, total_shares - shares);
        balance::set_total_deposited(&env, total_deposited - amount);

        // Return tokens
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&env.current_contract_address(), &withdrawer, &amount);

        events::withdraw(&env, &withdrawer, shares, amount);

        Ok(amount)
    }

    /// Query share balance of a user.
    pub fn shares_of(env: Env, user: Address) -> i128 {
        balance::get_shares(&env, &user)
    }

    /// Query how many tokens a given share count is worth right now.
    pub fn preview_redeem(env: Env, shares: i128) -> Result<i128, VaultError> {
        let total_shares = balance::get_total_shares(&env);
        let total_deposited = balance::get_total_deposited(&env);
        balance::shares_to_amount(total_shares, total_deposited, shares)
            .ok_or(VaultError::ArithmeticError)
    }

    /// Query total shares and deposited amounts.
    pub fn vault_state(env: Env) -> Result<(i128, i128), VaultError> {
        let _ = admin::get_admin(&env)?; // ensures initialized
        Ok((
            balance::get_total_shares(&env),
            balance::get_total_deposited(&env),
        ))
    }

    /// Pause all deposits and withdrawals (admin only).
    ///
    /// Sets the `Paused` flag to `true`. Any subsequent call to `deposit` or
    /// `withdraw` will return `VaultError::VaultPaused` and make no state
    /// changes until `unpause` is called. `add_yield` is also blocked while
    /// paused. User share balances and the vault's token holdings are
    /// unaffected by this call.
    ///
    /// Emits a `paused` event containing the admin address.
    ///
    /// # Errors
    /// - `VaultError::NotInitialized` — contract has not been initialized.
    /// - `VaultError::Unauthorized` — caller is not the current admin.
    pub fn pause(env: Env) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &true);
        let admin = admin::get_admin(&env)?;
        events::paused(&env, &admin);
        Ok(())
    }

    /// Resume deposits and withdrawals after a pause (admin only).
    ///
    /// Sets the `Paused` flag to `false`. Normal vault operations resume
    /// immediately; no funds are moved by this call itself.
    ///
    /// Emits an `unpaused` event containing the admin address.
    ///
    /// # Errors
    /// - `VaultError::NotInitialized` — contract has not been initialized.
    /// - `VaultError::Unauthorized` — caller is not the current admin.
    pub fn unpause(env: Env) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &false);
        let admin = admin::get_admin(&env)?;
        events::unpaused(&env, &admin);
        Ok(())
    }

    /// Inject yield into the vault by transferring tokens from the admin wallet (admin only).
    ///
    /// Transfers `amount` tokens from `admin_addr` into the vault contract and
    /// increments `total_deposited` by the same amount **without** minting new
    /// shares. Because existing shares now represent a larger pool, the share
    /// price rises proportionally for all current holders.
    ///
    /// The admin must hold at least `amount` tokens and must authorize the
    /// transfer. This function cannot remove or redirect tokens that belong to
    /// depositors — it only moves tokens *into* the vault.
    ///
    /// Requires the vault to be unpaused.
    ///
    /// Emits a `yield_add` event containing the admin address and amount.
    ///
    /// # Errors
    /// - `VaultError::NotInitialized` — contract has not been initialized.
    /// - `VaultError::Unauthorized` — caller is not the current admin.
    /// - `VaultError::VaultPaused` — vault is currently paused.
    /// - `VaultError::ZeroAmount` — `amount` is zero or negative.
    pub fn add_yield(env: Env, admin_addr: Address, amount: i128) -> Result<(), VaultError> {
        // ensure caller is admin
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

        // Transfer tokens from admin into the vault contract
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&admin_addr, &env.current_contract_address(), &amount);

        // Increase total deposited, do NOT mint shares
        let total_deposited = balance::get_total_deposited(&env);
        balance::set_total_deposited(&env, total_deposited + amount);

        // Emit event
        let admin_actual = admin::get_admin(&env)?;
        events::yield_added(&env, &admin_actual, amount);

        Ok(())
    }

    /// Transfer the admin role to a new address (admin only).
    ///
    /// Atomically replaces the stored admin address with `new_admin`. The
    /// current admin loses all privileges the moment this transaction is
    /// confirmed; `new_admin` gains them immediately. There is no two-phase
    /// handoff — verify `new_admin` is accessible before calling.
    ///
    /// # Errors
    /// - `VaultError::NotInitialized` — contract has not been initialized.
    /// - `VaultError::Unauthorized` — caller is not the current admin.
    pub fn transfer_admin(env: Env, new_admin: Address) -> Result<(), VaultError> {
        admin::require_admin(&env)?;
        let old_admin = admin::get_admin(&env)?;
        admin::set_admin(&env, &new_admin);
        events::admin_changed(&env, &old_admin, &new_admin);
        Ok(())
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

    /// Query the current withdrawal limit per transaction.
    pub fn get_withdrawal_limit(env: Env) -> Result<i128, VaultError> {
        let _ = admin::get_admin(&env)?; // ensures initialized
        Ok(balance::get_withdrawal_limit(&env).unwrap_or(0))
    }

    // --- Internal helpers ---

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
