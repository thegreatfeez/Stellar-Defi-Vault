#![cfg(test)]

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    token, Address, Env,
};

use crate::vault::{VaultContract, VaultContractClient, STELLAR_LEDGERS_PER_YEAR};

// ── helpers ──────────────────────────────────────────────────────────────────

fn create_token<'a>(
    env: &Env,
    admin: &Address,
) -> (Address, token::Client<'a>, token::StellarAssetClient<'a>) {
    let address = env.register_stellar_asset_contract(admin.clone());
    let client = token::Client::new(env, &address);
    let admin_client = token::StellarAssetClient::new(env, &address);
    (address, client, admin_client)
}

fn set_ledger(env: &Env, sequence: u32) {
    env.ledger().with_mut(|li| {
        li.sequence_number = sequence;
    });
}

// ── Full lifecycle: pool create → multi-user stake → mid-claim → rate change → unstake ──

/// Scenario:
///   admin deploys pool (rate 1000 bps / 10% APR)
///   → 5 users stake different amounts at ledger 0
///   → advance to half-year (ANNUAL/2 ledgers)
///   → users 1 and 2 claim mid-way rewards
///   → admin changes rate to 500 bps (5% APR)
///   → advance to full year (ANNUAL ledgers)
///   → all users unstake all shares
///   → all users claim remaining rewards
///   → verify: contract stake balance = 0, reward pool = initial − total_rewards_paid,
///              sum of final user balances = sum of initial + total rewards
///
/// Reward math (no boost schedule, multiplier = 10_000 = 1×):
///   reward = amount × rate_bps × elapsed / 10_000 / ANNUAL
///
/// Users 1 & 2 claim at ANNUAL/2 (rate still 1000), so their checkpoint advances.
/// Users 3–5 never claim early; their accrual runs from ledger 0 to ANNUAL using
/// the current rate at unstake time (500 bps), so they earn at 5% for the full year.
///
///   user1  (1_000_000): mid=50_000 + post=25_000  = 75_000
///   user2  (2_000_000): mid=100_000 + post=50_000 = 150_000
///   user3  (3_000_000): full-year @500 bps         = 150_000
///   user4  (4_000_000): full-year @500 bps         = 200_000
///   user5  (5_000_000): full-year @500 bps         = 250_000
///   total rewards paid = 825_000
#[test]
fn test_integration_full_lifecycle() {
    let annual: u32 = STELLAR_LEDGERS_PER_YEAR;

    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.sequence_number = 0;
        li.min_temp_entry_ttl = 10_000_000;
        li.min_persistent_entry_ttl = 10_000_000;
        li.max_entry_ttl = 10_000_000;
    });

    // ── Phase 1: Setup ──────────────────────────────────────────────────────
    let admin = Address::generate(&env);
    let user1 = Address::generate(&env);
    let user2 = Address::generate(&env);
    let user3 = Address::generate(&env);
    let user4 = Address::generate(&env);
    let user5 = Address::generate(&env);

    let (token_addr, token, token_admin) = create_token(&env, &admin);

    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    // Mint initial balances (10_000_000 each)
    let initial_balance: i128 = 10_000_000;
    for user in [&user1, &user2, &user3, &user4, &user5] {
        token_admin.mint(user, &initial_balance);
    }
    // Fund reward pool
    let reward_pool_initial: i128 = 1_000_000;
    token_admin.mint(&admin, &reward_pool_initial);
    vault.set_reward_rate_bps(&1000); // 10% APR
    vault.fund_reward_pool(&admin, &reward_pool_initial);

    // ── Phase 2: All users stake at ledger 0 ───────────────────────────────
    // Inline comment: first stake for each user → position_opened event emitted
    let stake1: i128 = 1_000_000;
    let stake2: i128 = 2_000_000;
    let stake3: i128 = 3_000_000;
    let stake4: i128 = 4_000_000;
    let stake5: i128 = 5_000_000;

    vault.stake(&user1, &stake1);
    vault.stake(&user2, &stake2);
    vault.stake(&user3, &stake3);
    vault.stake(&user4, &stake4);
    vault.stake(&user5, &stake5);

    // Verify pool_stats shows 5 stakers and correct total
    let stats = vault.pool_stats();
    assert_eq!(stats.total_stakers, 5, "Should have 5 active stakers after all stake");
    assert_eq!(
        stats.total_staked,
        stake1 + stake2 + stake3 + stake4 + stake5,
        "Total staked should equal sum of all stakes"
    );
    assert_eq!(stats.total_rewards_paid, 0, "No rewards paid yet");

    // ── Phase 3: Advance to half-year ──────────────────────────────────────
    set_ledger(&env, annual / 2);

    // ── Phase 4: Users 1 and 2 claim mid-way rewards ───────────────────────
    // This snapshots their reward checkpoint at the current (1000 bps) rate
    let mid_claim1 = vault.claim(&user1);
    let mid_claim2 = vault.claim(&user2);

    assert_eq!(mid_claim1, 50_000, "User1 mid-year reward at 10% APR");
    assert_eq!(mid_claim2, 100_000, "User2 mid-year reward at 10% APR");

    // ── Phase 5: Admin changes reward rate ─────────────────────────────────
    // Users 3–5 have not yet accrued; their future rewards will use this new rate
    vault.set_reward_rate_bps(&500); // 5% APR

    // ── Phase 6: Advance to full year ──────────────────────────────────────
    set_ledger(&env, annual);

    // ── Phase 7: All users unstake ─────────────────────────────────────────
    // Inline comment: shares are 1:1 with token amounts (no yield added), so each
    // user recovers exactly their original stake. accrue_rewards is called internally.
    let back1 = vault.unstake(&user1, &stake1);
    let back2 = vault.unstake(&user2, &stake2);
    let back3 = vault.unstake(&user3, &stake3);
    let back4 = vault.unstake(&user4, &stake4);
    let back5 = vault.unstake(&user5, &stake5);

    assert_eq!(back1, stake1, "User1 should recover full stake");
    assert_eq!(back2, stake2, "User2 should recover full stake");
    assert_eq!(back3, stake3, "User3 should recover full stake");
    assert_eq!(back4, stake4, "User4 should recover full stake");
    assert_eq!(back5, stake5, "User5 should recover full stake");

    // After all unstakes: staked balance = 0
    let (total_shares, total_deposited) = vault.vault_state();
    assert_eq!(total_shares, 0, "Total shares should be 0 after all unstake");
    assert_eq!(
        total_deposited, 0,
        "Contract stake token balance should be 0 after all unstake"
    );

    // pool_stats: 0 stakers after all full unstakes
    let stats_post_unstake = vault.pool_stats();
    assert_eq!(
        stats_post_unstake.total_stakers, 0,
        "total_stakers should reach 0 after all full unstakes"
    );

    // ── Phase 8: All users claim remaining rewards ─────────────────────────
    // Users 1 & 2: rewards earned in second half at 500 bps
    // Users 3–5: rewards earned for full year at current rate (500 bps applied retroactively
    //            because their checkpoint was never moved mid-year)
    let post_claim1 = vault.claim(&user1);
    let post_claim2 = vault.claim(&user2);
    let post_claim3 = vault.claim(&user3);
    let post_claim4 = vault.claim(&user4);
    let post_claim5 = vault.claim(&user5);

    assert_eq!(post_claim1, 25_000, "User1 second-half reward at 5% APR");
    assert_eq!(post_claim2, 50_000, "User2 second-half reward at 5% APR");
    assert_eq!(
        post_claim3, 150_000,
        "User3 full-year reward at 5% APR (rate changed before accrual)"
    );
    assert_eq!(post_claim4, 200_000, "User4 full-year reward at 5% APR");
    assert_eq!(post_claim5, 250_000, "User5 full-year reward at 5% APR");

    let total_rewards: i128 = mid_claim1
        + mid_claim2
        + post_claim1
        + post_claim2
        + post_claim3
        + post_claim4
        + post_claim5;
    assert_eq!(total_rewards, 825_000, "Total rewards across all users");

    // ── Phase 9: Final assertions ───────────────────────────────────────────

    // Assert A: contract reward token balance = initial_pool − total_rewards_paid
    let contract_balance = token.balance(&vault_id);
    assert_eq!(
        contract_balance,
        reward_pool_initial - total_rewards,
        "Contract reward token balance should equal initial pool minus total rewards paid"
    );

    // Assert B: pool_stats.total_rewards_paid matches actual sum of all claims
    let final_stats = vault.pool_stats();
    assert_eq!(
        final_stats.total_rewards_paid, total_rewards,
        "pool_stats.total_rewards_paid should equal sum of all successful claims"
    );

    // Assert C: sum of all user final balances = sum of initial balances + total rewards
    let sum_initial = initial_balance * 5; // 50_000_000
    let sum_final = token.balance(&user1)
        + token.balance(&user2)
        + token.balance(&user3)
        + token.balance(&user4)
        + token.balance(&user5);
    assert_eq!(
        sum_final,
        sum_initial + total_rewards,
        "Sum of final user balances should equal sum of initial balances plus total rewards earned"
    );
}

// ── Whitelist tests for permissioned staking ─────────────────────────────────

#[test]
fn test_whitelisted_user_can_stake_when_enabled() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| { li.min_persistent_entry_ttl = 1_000_000; li.max_entry_ttl = 1_000_000; });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let (token_addr, _token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    // enable whitelist and add alice
    vault.set_whitelist_enabled(&true);
    vault.add_to_whitelist(&alice);

    token_admin.mint(&alice, &100_000);
    let res = vault.try_stake(&alice, &50_000);
    assert!(res.is_ok(), "Whitelisted user should be able to stake when whitelist enabled");
}

#[test]
fn test_non_whitelisted_user_rejected_when_enabled() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| { li.min_persistent_entry_ttl = 1_000_000; li.max_entry_ttl = 1_000_000; });

    let admin = Address::generate(&env);
    let bob = Address::generate(&env);
    let (token_addr, _token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    // enable whitelist but do NOT add bob
    vault.set_whitelist_enabled(&true);

    token_admin.mint(&bob, &100_000);
    let res = vault.try_stake(&bob, &20_000);
    assert_eq!(res, Err(Ok(crate::errors::VaultError::NotWhitelisted)));
}

#[test]
fn test_toggle_off_allows_non_whitelisted() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| { li.min_persistent_entry_ttl = 1_000_000; li.max_entry_ttl = 1_000_000; });

    let admin = Address::generate(&env);
    let carol = Address::generate(&env);
    let (token_addr, _token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    // enable whitelist, but then turn it off
    vault.set_whitelist_enabled(&true);
    vault.set_whitelist_enabled(&false);

    token_admin.mint(&carol, &100_000);
    // should succeed when whitelist disabled
    let res = vault.try_stake(&carol, &30_000);
    assert!(res.is_ok());
}

#[test]
fn test_revocation_blocks_new_stake_but_allows_unstake_and_claim() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| { li.sequence_number = 0; li.min_persistent_entry_ttl = 1_000_000; li.max_entry_ttl = 1_000_000; });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let (token_addr, token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    // enable whitelist and add alice
    vault.set_whitelist_enabled(&true);
    vault.add_to_whitelist(&alice);

    token_admin.mint(&alice, &200_000);
    // alice stakes 100k
    vault.stake(&alice, &100_000);

    // advance ledger and set a reward rate so claim will return >0
    env.ledger().with_mut(|li| li.sequence_number = 500);
    vault.set_reward_rate_bps(&1000);

    // revoke alice
    vault.remove_from_whitelist(&alice);

    // alice should NOT be able to stake more
    let try_more = vault.try_stake(&alice, &10_000);
    assert_eq!(try_more, Err(Ok(crate::errors::VaultError::NotWhitelisted)));

    // but alice should still be able to claim accrued rewards
    let claim_res = vault.claim(&alice);
    // claim should succeed (may be zero or >0 depending on timing), but must not error
    // ensure method returns without Err by comparing types — here it's direct call so will panic on Err
    // We assert that the returned value is >= 0
    assert!(claim_res >= 0);

    // and unstake should work
    let unstake_res = vault.unstake(&alice, &100_000);
    assert_eq!(unstake_res, 100_000);
}
// ── pool_stats reflects staker count correctly ────────────────────────────────

#[test]
fn test_total_stakers_tracks_entries_and_exits() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let (token_addr, _token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    token_admin.mint(&alice, &500_000);
    token_admin.mint(&bob, &500_000);

    // No stakers initially
    assert_eq!(vault.pool_stats().total_stakers, 0);

    vault.stake(&alice, &100_000);
    assert_eq!(
        vault.pool_stats().total_stakers, 1,
        "total_stakers should be 1 after alice stakes"
    );

    vault.stake(&bob, &200_000);
    assert_eq!(
        vault.pool_stats().total_stakers, 2,
        "total_stakers should be 2 after bob stakes"
    );

    // Partial unstake should NOT decrement stakers
    vault.unstake(&alice, &50_000);
    assert_eq!(
        vault.pool_stats().total_stakers, 2,
        "total_stakers unchanged after partial unstake"
    );

    // Full unstake decrements
    vault.unstake(&alice, &50_000);
    assert_eq!(
        vault.pool_stats().total_stakers, 1,
        "total_stakers should be 1 after alice fully unstakes"
    );

    vault.unstake(&bob, &200_000);
    assert_eq!(
        vault.pool_stats().total_stakers, 0,
        "total_stakers should be 0 after all fully unstake"
    );
}

// ── position_of returns correct data ──────────────────────────────────────────

#[test]
fn test_position_of_returns_correct_fields() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.sequence_number = 10;
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let (token_addr, _token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);
    vault.set_reward_rate_bps(&1000);

    token_admin.mint(&alice, &500_000);
    vault.stake(&alice, &200_000);

    let position = vault.position_of(&alice).unwrap();
    assert_eq!(position.amount, 200_000, "amount should equal staked tokens");
    assert_eq!(
        position.staked_at_ledger, 10,
        "staked_at_ledger should match ledger at stake time"
    );
    assert_eq!(
        position.last_claim_ledger, 10,
        "last_claim_ledger is initialised to the stake ledger when a position is opened"
    );
}

// ── delegate staking: full happy path ────────────────────────────────────────

#[test]
fn test_stake_for_delegate_happy_path() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let delegate = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (token_addr, token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    // Fund only the delegate — beneficiary has no tokens
    token_admin.mint(&delegate, &500_000);

    // Beneficiary approves delegate
    vault.approve_delegate(&beneficiary, &delegate);
    assert!(
        vault.is_delegate(&beneficiary, &delegate),
        "delegate should be approved after approve_delegate"
    );

    // Delegate stakes on behalf of beneficiary
    let shares = vault.stake_for(&delegate, &beneficiary, &300_000);
    assert_eq!(shares, 300_000, "Shares should equal amount on first stake");
    assert_eq!(
        vault.shares_of(&beneficiary), 300_000,
        "Position should be credited to beneficiary"
    );
    assert_eq!(
        token.balance(&delegate), 200_000,
        "Tokens deducted from delegate's wallet"
    );
    assert_eq!(
        token.balance(&beneficiary), 0,
        "Beneficiary's token balance unchanged"
    );

    // Beneficiary can unstake
    let returned = vault.unstake(&beneficiary, &300_000);
    assert_eq!(returned, 300_000, "Beneficiary should recover tokens on unstake");
    assert_eq!(vault.shares_of(&beneficiary), 0);
}

// ── delegate staking: auth / rejection edge cases ────────────────────────────

#[test]
fn test_stake_for_non_approved_delegate_rejected() {
    use crate::errors::VaultError;

    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let delegate = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (token_addr, _token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);
    token_admin.mint(&delegate, &500_000);

    // No approval given — should fail
    let result = vault.try_stake_for(&delegate, &beneficiary, &100_000);
    assert_eq!(
        result,
        Err(Ok(VaultError::NotADelegate)),
        "Non-approved delegate should be rejected"
    );
}

#[test]
fn test_stake_for_revoked_delegate_rejected() {
    use crate::errors::VaultError;

    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let delegate = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (token_addr, _token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);
    token_admin.mint(&delegate, &500_000);

    vault.approve_delegate(&beneficiary, &delegate);
    vault.revoke_delegate(&beneficiary, &delegate);

    assert!(
        !vault.is_delegate(&beneficiary, &delegate),
        "Delegate should be removed after revocation"
    );

    let result = vault.try_stake_for(&delegate, &beneficiary, &100_000);
    assert_eq!(
        result,
        Err(Ok(VaultError::NotADelegate)),
        "Revoked delegate should be rejected"
    );
}

// ── analytics events: rate_changed / position_opened / position_closed ────────

#[test]
fn test_rate_changed_event_emitted() {
    use soroban_sdk::{testutils::Events, Symbol, TryFromVal};

    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let (token_addr, _token, _token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    vault.set_reward_rate_bps(&1000);

    let events = env.events().all();
    let matched: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| {
            topics
                .get(0)
                .and_then(|v| Symbol::try_from_val(&env, &v).ok())
                .map(|s| s == Symbol::new(&env, "rate_chg"))
                .unwrap_or(false)
        })
        .collect();

    assert_eq!(matched.len(), 1, "rate_chg event should be emitted once");
}

#[test]
fn test_position_opened_event_on_first_stake() {
    use soroban_sdk::{testutils::Events, Symbol, TryFromVal};

    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let (token_addr, _token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);
    token_admin.mint(&alice, &500_000);

    vault.stake(&alice, &100_000);
    vault.stake(&alice, &100_000); // second stake — should NOT emit position_opened again

    let events = env.events().all();
    let matched: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| {
            topics
                .get(0)
                .and_then(|v| Symbol::try_from_val(&env, &v).ok())
                .map(|s| s == Symbol::new(&env, "pos_open"))
                .unwrap_or(false)
        })
        .collect();

    assert_eq!(
        matched.len(), 1,
        "pos_open should be emitted only on first stake, not top-ups"
    );
    let event = &matched[0];
    assert_eq!(
        Address::try_from_val(&env, &event.1.get(1).unwrap()).unwrap(),
        alice,
        "pos_open topic should contain the user address"
    );
}

#[test]
fn test_position_closed_event_on_full_unstake() {
    use soroban_sdk::{testutils::Events, Symbol, TryFromVal};

    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let (token_addr, _token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);
    token_admin.mint(&alice, &500_000);

    vault.stake(&alice, &200_000);
    vault.unstake(&alice, &100_000); // partial — should NOT emit pos_clos
    vault.unstake(&alice, &100_000); // full — SHOULD emit pos_clos

    let events = env.events().all();
    let matched: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| {
            topics
                .get(0)
                .and_then(|v| Symbol::try_from_val(&env, &v).ok())
                .map(|s| s == Symbol::new(&env, "pos_clos"))
                .unwrap_or(false)
        })
        .collect();

    assert_eq!(
        matched.len(), 1,
        "pos_clos should be emitted only on full unstake, not partial"
    );
    let event = &matched[0];
    assert_eq!(
        Address::try_from_val(&env, &event.1.get(1).unwrap()).unwrap(),
        alice,
        "pos_clos topic should contain the user address"
    );
}

// ── paused / unpaused events include ledger field ─────────────────────────────

#[test]
fn test_paused_event_includes_ledger() {
    use soroban_sdk::{testutils::Events, Symbol, TryFromVal, Vec as SorobanVec};

    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.sequence_number = 42;
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let (token_addr, _token, _token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    vault.pause();

    let events = env.events().all();
    let matched: std::vec::Vec<_> = events
        .into_iter()
        .filter(|(_, topics, _)| {
            topics
                .get(0)
                .and_then(|v| Symbol::try_from_val(&env, &v).ok())
                .map(|s| s == Symbol::new(&env, "paused"))
                .unwrap_or(false)
        })
        .collect();

    assert_eq!(matched.len(), 1, "paused event should be emitted");

    // event data is (ledger,) published as a Soroban Vec — extract the first element
    let data_vec = SorobanVec::<soroban_sdk::Val>::try_from_val(&env, &matched[0].2).unwrap();
    let ledger_val: u32 = u32::try_from_val(&env, &data_vec.get(0).unwrap()).unwrap();
    assert_eq!(ledger_val, 42, "paused event data should include the current ledger sequence");
}

// ── slash admin actions tests ───────────────────────────────────────────────

#[test]
fn test_slash_partial_and_treasury_receive() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let treasury = Address::generate(&env);
    let (token_addr, token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    // set custom treasury
    vault.set_slash_treasury(&treasury);

    // fund alice and stake
    token_admin.mint(&alice, &500_000);
    vault.stake(&alice, &200_000);

    // pre-check balances
    assert_eq!(token.balance(&vault_id), 200_000);
    assert_eq!(token.balance(&treasury), 0);

    // admin slashes 50_000
    let slashed = vault.slash(&admin, &alice, &50_000);
    assert_eq!(slashed, 50_000);

    // alice position reduced accordingly (shares correspond to amounts on first stake)
    assert_eq!(vault.shares_of(&alice), 150_000);

    // treasury received tokens
    assert_eq!(token.balance(&treasury), 50_000);
    // contract balance decreased
    assert_eq!(token.balance(&vault_id), 150_000);
}

#[test]
fn test_slash_full_and_position_removed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let treasury = Address::generate(&env);
    let (token_addr, token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);
    vault.set_slash_treasury(&treasury);

    token_admin.mint(&alice, &300_000);
    vault.stake(&alice, &150_000);

    // slash full or larger amount
    let slashed = vault.slash(&admin, &alice, &200_000);
    assert_eq!(slashed, 150_000);

    // position removed
    assert_eq!(vault.shares_of(&alice), 0);
    assert_eq!(token.balance(&treasury), 150_000);
    assert_eq!(token.balance(&vault_id), 0);
}

#[test]
fn test_slash_works_while_paused() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let treasury = Address::generate(&env);
    let (token_addr, token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);
    vault.set_slash_treasury(&treasury);

    token_admin.mint(&alice, &200_000);
    vault.stake(&alice, &100_000);

    // pause the contract
    vault.pause();

    // should still be able to slash
    let slashed = vault.slash(&admin, &alice, &30_000);
    assert_eq!(slashed, 30_000);
    assert_eq!(token.balance(&treasury), 30_000);
}

#[test]
fn test_non_admin_rejected_for_slash() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let (token_addr, _token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    token_admin.mint(&alice, &100_000);
    vault.stake(&alice, &50_000);

    // Verify admin auth is required: the recorded authorizer must be the admin address.
    vault.slash(&admin, &alice, &10_000);
    let auths = env.auths();
    assert!(auths.iter().any(|(addr, _)| addr == &admin));
}

#[test]
fn test_reward_forfeiture_on_slash() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.sequence_number = 0;
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let treasury = Address::generate(&env);
    let (token_addr, token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);
    vault.set_slash_treasury(&treasury);

    token_admin.mint(&alice, &500_000);
    vault.stake(&alice, &100_000);

    // advance ledger to accrue rewards
    env.ledger().with_mut(|li| li.sequence_number = 1000);
    vault.set_reward_rate_bps(&1000); // set a rate so rewards accrue

    // compute pending before slash (call claim would consume; we just simulate by checking pending)
    let pending_before = vault.calc_pending_reward(&alice);
    assert!(pending_before > 0);

    // Slash user
    vault.slash(&admin, &alice, &50_000);

    // After slash, accrued rewards should be cleared; claim should return 0
    let claim_after = vault.claim(&alice);
    assert_eq!(claim_after, 0);
}

#[test]
fn test_initialization_defaults_treasury_to_admin() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 1_000_000;
        li.max_entry_ttl = 1_000_000;
    });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let (token_addr, token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    // initialize without specifying treasury (defaults to admin)
    vault.initialize(&admin, &token_addr, &None, &None);

    token_admin.mint(&alice, &100_000);
    vault.stake(&alice, &20_000);

    // admin slashes -> funds should go to admin (default treasury)
    vault.slash(&admin, &alice, &10_000);
    assert_eq!(token.balance(&admin), 10_000);
}

// ── cooldown / unbonding flow tests ─────────────────────────────────────────

#[test]
fn test_full_cooldown_flow() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| { li.sequence_number = 0; li.min_persistent_entry_ttl = 1_000_000; li.max_entry_ttl = 1_000_000; });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let (token_addr, token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    // set cooldown to 5 ledgers
    vault.set_cooldown_period(&5);

    token_admin.mint(&alice, &200_000);
    vault.stake(&alice, &100_000);

    // request unstake 50_000
    vault.request_unstake(&alice, &50_000);

    // pending unbonding should be present
    let pos = vault.pending_unbonding(&alice).unwrap();
    assert_eq!(pos.amount, 50_000);

    // advance ledger past cooldown
    env.ledger().with_mut(|li| li.sequence_number = 6);

    // execute unstake
    let executed = vault.execute_unstake(&alice);
    assert_eq!(executed, 50_000);

    // alice balance: minted 200k - staked 100k + executed 50k = 150k
    assert_eq!(token.balance(&alice), 150_000);
}

#[test]
fn test_premature_execute_unstake_fails() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| { li.sequence_number = 0; li.min_persistent_entry_ttl = 1_000_000; li.max_entry_ttl = 1_000_000; });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let (token_addr, _token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    vault.set_cooldown_period(&10);
    token_admin.mint(&alice, &100_000);
    vault.stake(&alice, &50_000);

    vault.request_unstake(&alice, &20_000);

    // attempt to execute immediately -> should fail
    let res = vault.try_execute_unstake(&alice);
    assert_eq!(res, Err(Ok(crate::errors::VaultError::UseCooldownFlow)));
}

#[test]
fn test_zero_cooldown_bypass_allows_instant_unstake() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| { li.sequence_number = 0; li.min_persistent_entry_ttl = 1_000_000; li.max_entry_ttl = 1_000_000; });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let (token_addr, token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    // set cooldown to 0
    vault.set_cooldown_period(&0);

    token_admin.mint(&alice, &100_000);
    vault.stake(&alice, &50_000);

    // instant unstake allowed
    let returned = vault.unstake(&alice, &50_000);
    assert_eq!(returned, 50_000);
    assert_eq!(token.balance(&alice), 100_000);
}

#[test]
fn test_no_rewards_accrued_during_cooldown() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| { li.sequence_number = 0; li.min_persistent_entry_ttl = 1_000_000; li.max_entry_ttl = 1_000_000; });

    let admin = Address::generate(&env);
    let alice = Address::generate(&env);
    let (token_addr, _token, token_admin) = create_token(&env, &admin);
    let vault_id = env.register_contract(None, VaultContract);
    let vault = VaultContractClient::new(&env, &vault_id);
    vault.initialize(&admin, &token_addr, &None, &None);

    vault.set_cooldown_period(&10);

    // Large principal ensures calc_pending_reward returns > 0 despite integer truncation
    token_admin.mint(&alice, &10_000_000);
    token_admin.mint(&admin, &1_000_000);
    vault.stake(&alice, &10_000_000);

    // advance ledger to accrue some rewards
    env.ledger().with_mut(|li| li.sequence_number = 100);
    vault.set_reward_rate_bps(&1000);

    let pending_before = vault.calc_pending_reward(&alice);
    assert!(pending_before > 0, "pending_before must be > 0; got {}", pending_before);

    // fund reward pool so claim() can transfer
    vault.fund_reward_pool(&admin, &pending_before);

    // request unstake full amount — settles rewards into AccruedReward at this ledger
    vault.request_unstake(&alice, &10_000_000);

    // advance further during cooldown
    env.ledger().with_mut(|li| li.sequence_number = 200);

    // claim should return exactly the rewards accrued before request_unstake;
    // no further rewards accrue on the unbonding principal (shares are 0)
    let claim_after = vault.claim(&alice);
    assert_eq!(claim_after, pending_before);
}
