use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum VaultError {
    NotInitialized = 1,
    AlreadyInitialized = 2,
    Unauthorized = 3,
    ZeroAmount = 4,
    InsufficientShares = 5,
    VaultPaused = 6,
    InvalidToken = 7,
    ArithmeticError = 8,
    WithdrawalLimitExceeded = 9,
    InvalidPenaltyBps = 10,
    BelowMinimumStake = 11,
    TooManyBoostTiers = 12,
    InvalidBoostSchedule = 13,
    InsufficientRewardPool = 14,
    NotADelegate = 15,
}
