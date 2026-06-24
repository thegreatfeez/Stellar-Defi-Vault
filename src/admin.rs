use crate::errors::VaultError;
use crate::storage::DataKey;
use soroban_sdk::{Address, Env};

pub fn set_admin(env: &Env, admin: &Address) {
    env.storage().instance().set(&DataKey::Admin, admin);
}

pub fn get_admin(env: &Env) -> Result<Address, VaultError> {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(VaultError::NotInitialized)
}

pub fn require_admin(env: &Env) -> Result<(), VaultError> {
    let admin = get_admin(env)?;
    admin.require_auth();
    Ok(())
}
