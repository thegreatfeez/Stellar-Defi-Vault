# Security Model

This document describes the trust model of the Stellar DeFi Vault contract: what the admin account can and cannot do, what happens in failure scenarios, and how to rotate the admin key. All claims are verified against the deployed source code in `src/vault.rs`.

---

## Admin-Only Functions

The following functions gate on `admin::require_admin`, which calls `admin.require_auth()` using the Soroban auth framework. Any call that does not carry a valid authorization signature from the current admin address is rejected by the host before the function body executes.

### `pause()`

Flips the `Paused` flag in instance storage to `true`.

**Effect:** Subsequent calls to `deposit` and `withdraw` return `VaultError::VaultPaused` (error code 6) and no state changes occur. No funds are moved. User share balances and the underlying token holdings of the contract are unaffected.

**Emits:** `paused` event with the admin address.

### `unpause()`

Flips the `Paused` flag back to `false`.

**Effect:** Deposits and withdrawals resume normally. No funds are moved during the call itself.

**Emits:** `unpaused` event with the admin address.

### `add_yield(admin_addr, amount)`

Transfers `amount` tokens **from the admin's own wallet** into the vault contract, then increments `total_deposited` by the same amount without minting new shares.

**Effect:** The share price rises for all existing holders. No user share balances change. The admin must hold sufficient token balance and approve the transfer. This function requires the vault to be unpaused.

**Emits:** `yield_add` event with the admin address and the amount added.

**Key constraint:** The admin is sending their own tokens *into* the vault, not extracting anything from it. This function cannot be used to remove user principal.

### `transfer_admin(new_admin)`

Replaces the stored admin address with `new_admin` in a single atomic step.

**Effect:** The calling address (current admin) loses admin privileges immediately. The new address gains them immediately. There is no two-phase handoff — the current admin must trust the new address before calling.

**Does not emit an event.** Monitor on-chain storage changes or set up an indexer alert on this function call if admin rotation observability matters.

---

## What Admin Cannot Do

The following operations are **not possible** for the admin, confirmed by code review:

### Admin cannot access user principal

There is no function in the contract that allows the admin to withdraw, transfer, or redirect tokens that belong to depositors. The only path by which tokens leave the vault is `withdraw()`, which requires:

```rust
withdrawer.require_auth();
```

This means the `withdrawer` address must sign the transaction. The admin key alone cannot satisfy this requirement for another user's address. The admin and a user are distinct addresses — even if the same entity controlled both, they are separate authorization contexts enforced by the Soroban host.

User share balances are stored in persistent storage under `DataKey::ShareBalance(user_address)`. The contract exposes no setter for this key that is gated on admin auth alone.

### Admin cannot mint shares to themselves

`add_yield` increases `total_deposited` but explicitly does **not** call `balance::set_shares` or `balance::set_total_shares`. New shares are only minted inside `deposit()`, which requires the depositor's own auth.

### Admin cannot change the vault token

The `Token` key is written once during `initialize` and there is no setter for it exposed through any function.

### Admin cannot re-initialize the contract

`initialize` checks `env.storage().instance().has(&DataKey::Admin)` and returns `VaultError::AlreadyInitialized` (error code 2) if the admin key is already set.

---

## Failure Scenarios

### Vault is paused

Both `deposit` and `withdraw` call `require_not_paused` before any state changes:

```rust
Self::require_not_paused(&env)?;
```

If paused:
- Deposits return `VaultError::VaultPaused`. No tokens are moved.
- Withdrawals return `VaultError::VaultPaused`. User principal stays in the vault contract.
- User share balances are unchanged — they continue to represent the same ownership fraction.
- `add_yield` also checks `require_not_paused`, so the share price cannot change while paused.

User funds are frozen, not lost. As soon as `unpause()` is called, full functionality resumes and users can withdraw their proportional share at the current (unchanged) price.

### Yield injection exhausted (admin has no tokens to add)

This vault uses a share-price appreciation model rather than a streaming reward token. Yield is delivered only when the admin calls `add_yield`. If the admin stops calling `add_yield` (whether due to insufficient balance, operational decision, or key compromise):

- Existing depositors retain their shares at the last recorded share price.
- No yield accrues passively — share price is constant until the next `add_yield` call.
- Withdrawals continue to work normally; users receive `shares × (total_deposited / total_shares)` tokens, which reflects all previously added yield.

There is no separate reward token pool that can be "emptied." The vault holds only the single token specified at `initialize`. Yield additions and user principal are fungible in the contract balance; the accounting invariant is:

```
contract token balance ≥ total_deposited
```

This invariant holds as long as no external mechanism drains the contract (no such mechanism exists in this contract).

### Arithmetic overflow or zero-division

Share minting and redemption use `checked_mul` / `checked_div`. On failure these return `None`, which is mapped to `VaultError::ArithmeticError` (error code 8). The transaction reverts with no state changes.

### Admin key compromise

If the admin key is compromised, an attacker can:
- Pause the vault (freezing user withdrawals).
- Call `add_yield` with a zero-value amount (no effect due to `amount <= 0` guard).
- Transfer admin to another address, locking out the legitimate admin.

An attacker with the admin key **cannot** drain user funds (see above). The highest-impact action is a sustained pause that prevents users from withdrawing. This is mitigated by rotating the admin key promptly (see below).

---

## Admin Key Rotation

To rotate the admin key:

1. Generate or designate a new Stellar keypair (or a multisig policy address).
2. Call `transfer_admin(new_admin)` signed by the **current** admin key.
3. Verify on-chain that `DataKey::Admin` now holds the new address.
4. Revoke or destroy the old private key.

This is a single-step, irreversible operation. The current admin loses authority the moment the transaction is confirmed. There is no recovery path if the new address is inaccessible, so verify the new address is under your control before calling.

**Recommended practice:** Use a hardware wallet or threshold-signature scheme for the admin address in any deployment holding significant value.

---

## Audit Status

This contract is unaudited. Do not use in production without an independent security audit. If you discover a vulnerability, please open a private [GitHub Security Advisory](../../security/advisories/new) rather than a public issue.
