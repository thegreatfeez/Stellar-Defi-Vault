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
    CannotRescueStakeToken = 16,
    CannotRescueRewardToken = 17,
    /// No active position found for a given user when expected.
    PositionNotFound = 18,
    /// Caller or beneficiary is not whitelisted while whitelist is enabled.
    NotWhitelisted = 19,
    /// Unstake must use request_unstake / execute_unstake flow when cooldown is enabled.
    UseCooldownFlow = 20,
    /// Unstake fee exceeds the maximum allowed (500 bps / 5%).
    UnstakeFeeTooHigh = 21,
    /// batch_position_query was called with more than 20 addresses.
    BatchTooLarge = 22,
    /// get_total_claimable was called when more than 200 stakers are registered.
    TooManyStakers = 23,
    /// Recipient already has an active staking position.
    RecipientAlreadyStaking = 24,
    /// A boost campaign is already active; end it before starting a new one.
    CampaignAlreadyActive = 25,
    /// No boost campaign is currently active.
    NoCampaignActive = 26,
    /// Leaderboard size exceeds the maximum of 20.
    LeaderboardSizeTooLarge = 27,
    /// view_all_positions page_size exceeds the maximum of 20.
    PageSizeTooLarge = 28,
    /// Staker is not KYC-approved and KYC enforcement is enabled (issue #106).
    KycNotApproved = 29,
    /// Contract has been permanently stopped via emergency_stop (issue #107).
    ContractStopped = 30,
    /// Total pool deposits would exceed the configured pool cap.
    PoolCapReached = 31,
    /// Pool description exceeds the maximum length of 200 characters (issue #97).
    DescriptionTooLong = 32,
    /// record_wave_activity received a wave_id <= the last recorded wave (issue #99).
    NonMonotonicWaveId = 33,
    /// record_wave_activity was called with more than 50 active users (issue #99).
    TooManyActiveUsers = 34,
    /// admin, stake_token, or reward_token is the zero/invalid address (issue #70).
    InvalidAddress = 35,
    /// reward_rate_bps exceeds the maximum allowed cap (issue #72).
    RateTooHigh = 36,
}
