use soroban_sdk::{symbol_short, Address, Env};

pub fn deposit(env: &Env, depositor: &Address, amount: i128, shares_minted: i128) {
    let topics = (symbol_short!("deposit"), depositor);
    env.events()
        .publish(topics, (amount, shares_minted, env.ledger().sequence()));
}

pub fn withdraw(env: &Env, withdrawer: &Address, shares_burned: i128, amount_returned: i128) {
    let topics = (symbol_short!("withdraw"), withdrawer);
    env.events()
        .publish(topics, (shares_burned, amount_returned, env.ledger().sequence()));
}

pub fn paused(env: &Env, admin: &Address) {
    let topics = (symbol_short!("paused"), admin);
    env.events().publish(topics, (env.ledger().sequence(),));
}

pub fn unpaused(env: &Env, admin: &Address) {
    let topics = (symbol_short!("unpaused"), admin);
    env.events().publish(topics, (env.ledger().sequence(),));
}

pub fn yield_added(env: &Env, admin: &Address, amount: i128) {
    let topics = (symbol_short!("yield_add"), admin);
    env.events()
        .publish(topics, (amount, env.ledger().sequence()));
}

pub fn admin_changed(env: &Env, old_admin: &Address, new_admin: &Address) {
    let topics = (symbol_short!("admin_set"), old_admin);
    env.events()
        .publish(topics, (new_admin, env.ledger().sequence()));
}

pub fn withdrawal_limit_updated(env: &Env, admin: &Address, new_limit: i128) {
    let topics = (symbol_short!("wd_limit"), admin);
    env.events()
        .publish(topics, (new_limit, env.ledger().sequence()));
}

pub fn rate_changed(env: &Env, old_rate_bps: u32, new_rate_bps: u32) {
    let topics = (symbol_short!("rate_chg"),);
    env.events()
        .publish(topics, (old_rate_bps, new_rate_bps, env.ledger().sequence()));
}

pub fn pool_cap_updated(env: &Env, admin: &Address, new_cap: i128) {
    let topics = (symbol_short!("cap_upd"), admin);
    env.events()
        .publish(topics, (new_cap, env.ledger().sequence()));
}

pub fn position_opened(env: &Env, user: &Address, amount: i128) {
    let topics = (symbol_short!("pos_open"), user);
    env.events()
        .publish(topics, (amount, env.ledger().sequence()));
}

pub fn position_closed(env: &Env, user: &Address) {
    let topics = (symbol_short!("pos_clos"), user);
    env.events().publish(topics, (env.ledger().sequence(),));
}

pub fn slash(env: &Env, admin: &Address, user: &Address, amount: i128) {
    let topics = (symbol_short!("slash"), admin);
    env.events()
        .publish(topics, (user.clone(), amount, env.ledger().sequence()));
}

pub fn position_transferred(env: &Env, from: &Address, to: &Address, amount: i128) {
    let topics = (symbol_short!("pos_xfer"), from);
    env.events()
        .publish(topics, (to.clone(), amount, env.ledger().sequence()));
}

pub fn campaign_started(env: &Env, admin: &Address, multiplier_bps: u32, ends_at_ledger: u32) {
    let topics = (symbol_short!("camp_on"), admin);
    env.events()
        .publish(topics, (multiplier_bps, ends_at_ledger, env.ledger().sequence()));
}

pub fn campaign_ended(env: &Env, admin: &Address) {
    let topics = (symbol_short!("camp_off"), admin);
    env.events().publish(topics, (env.ledger().sequence(),));
}

/// Emitted when a user claims staking rewards (via `claim` or `stake_and_claim`).
pub fn claimed(env: &Env, user: &Address, reward: i128) {
    let topics = (symbol_short!("claimed"), user);
    env.events()
        .publish(topics, (reward, env.ledger().sequence()));
}

/// Emitted by `initialize` so indexers can detect new pool deployments on-chain.
pub fn pool_initialized(
    env: &Env,
    admin: &Address,
    stake_token: &Address,
    reward_token: &Address,
    reward_rate_bps: u32,
) {
    let topics = (symbol_short!("init"),);
    env.events().publish(
        topics,
        (admin.clone(), stake_token.clone(), reward_token.clone(), reward_rate_bps),
    );
}
