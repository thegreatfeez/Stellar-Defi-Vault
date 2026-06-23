use soroban_sdk::{Address, Env, Symbol, symbol_short};

pub fn deposit(env: &Env, depositor: &Address, amount: i128, shares_minted: i128) {
    let topics = (symbol_short!("deposit"), depositor);
    env.events().publish(topics, (amount, shares_minted));
}

pub fn withdraw(env: &Env, withdrawer: &Address, shares_burned: i128, amount_returned: i128) {
    let topics = (symbol_short!("withdraw"), withdrawer);
    env.events().publish(topics, (shares_burned, amount_returned));
}

pub fn paused(env: &Env, admin: &Address) {
    let topics = (symbol_short!("paused"), admin);
    env.events().publish(topics, ());
}

pub fn unpaused(env: &Env, admin: &Address) {
    let topics = (symbol_short!("unpaused"), admin);
    env.events().publish(topics, ());
}

pub fn yield_added(env: &Env, admin: &Address, amount: i128) {
    let topics = (symbol_short!("yield_add"), admin);
    env.events().publish(topics, (amount,));
}
