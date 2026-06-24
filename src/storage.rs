use soroban_sdk::{contracttype, Address};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Token,
    TotalShares,
    TotalDeposited,
    MinStake,
    RewardRateBps,
    RewardPoolBalance,
    BoostSchedule,
    ShareBalance(Address),
    StakeHistory(Address),
    RewardCheckpointLedger(Address),
    LastClaimLedger(Address),
    AccruedReward(Address),
    Paused,
    WithdrawalLimit,
    LockPeriod,
    EarlyExitPenaltyBps,
    StakedAtLedger(Address),
    TotalStakers,
    TotalRewardsPaid,
    Delegate(Address),
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct VaultState {
    pub total_shares: i128,
    pub total_deposited: i128,
    pub paused: bool,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PoolStats {
    pub total_staked: i128,
    pub total_stakers: u32,
    pub reward_rate_bps: i128,
    pub reward_token_balance: i128,
    pub paused: bool,
    pub total_rewards_paid: i128,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct UserStats {
    pub position_amount: i128,
    pub pending_reward: i128,
    pub staked_at_ledger: u32,
    pub last_claim_ledger: u32,
}
