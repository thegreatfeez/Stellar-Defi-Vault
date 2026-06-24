#![no_std]

mod admin;
mod balance;
mod errors;
mod events;
mod storage;
mod vault;

pub use vault::VaultContract;

#[cfg(test)]
mod test;
