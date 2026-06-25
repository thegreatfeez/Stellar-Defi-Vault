# Stellar DeFi Vault

[![CI](https://github.com/YOUR_ORG/stellar-defi-vault/actions/workflows/ci.yml/badge.svg)](https://github.com/YOUR_ORG/stellar-defi-vault/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)
[![Stellar Wave](https://img.shields.io/badge/Stellar-Wave%20Program-blue)](https://www.drips.network/wave/stellar)

A non-custodial, share-based DeFi yield vault built on **Stellar** using **Soroban** smart contracts (Rust). Users deposit a Stellar token and receive proportional vault shares in return. Shares accrue value as yield is added to the vault, and can be redeemed at any time for the underlying token. The contract also exposes staking-oriented helpers for governance vote snapshots, minimum stake enforcement, reward claims, and time-based reward boosts.

## Architecture

```
VaultContract
├── initialize(admin, token)   — one-time setup
├── deposit(depositor, amount) — mint shares proportional to pool
├── stake(staker, amount)      — staking-friendly alias for deposit
├── withdraw(user, shares)     — burn shares, return tokens
├── unstake(staker, shares)    — staking-friendly alias for withdraw
├── claim(staker)              — claim accrued reward tokens
├── calc_pending_reward(user)  — read-only pending rewards
├── vote_weight_at(user, lgr)  — historical governance weight
├── current_vote_weight(user)  — current governance weight
├── total_vote_weight()        — pool-wide governance weight
├── preview_redeem(shares)     — read-only: how much would I get?
├── vault_state()              — total shares & total deposited
├── set_min_stake(amount)      — admin dust-position control
├── set_boost_schedule(tiers)  — admin reward multiplier tiers
├── pause() / unpause()        — admin circuit breaker
└── transfer_admin(new_admin)  — rotate admin key
```

### Share Price Formula

```
shares_minted = amount × (total_shares / total_deposited)   # existing pool
shares_minted = amount                                       # first deposit (1:1)

amount_returned = shares × (total_deposited / total_shares)
```

This is the same ratio model used by ERC-4626 vaults, adapted for Soroban.

## Getting Started

### Prerequisites

```bash
rustup target add wasm32-unknown-unknown
```

### Build

```bash
cargo build --target wasm32-unknown-unknown --release
```

### Test

```bash
cargo test --features testutils
```

### Lint

```bash
cargo fmt --check
cargo clippy --features testutils -- -D warnings
```

## Contract Interface

| Function | Auth Required | Description |
|---|---|---|
| `initialize(admin, token)` | — | One-time init |
| `deposit(depositor, amount)` | depositor | Deposit tokens, receive shares |
| `stake(staker, amount)` | staker | Alias for `deposit` |
| `withdraw(user, shares)` | user | Burn shares, receive tokens |
| `unstake(staker, shares)` | staker | Alias for `withdraw` |
| `claim(staker)` | staker | Claim accrued rewards from the reward pool |
| `calc_pending_reward(user)` | — | Pending reward query |
| `shares_of(user)` | — | Query share balance |
| `current_vote_weight(user)` | — | Current governance vote weight |
| `vote_weight_at(user, ledger)` | — | Historical governance vote weight |
| `total_vote_weight()` | — | Pool-wide governance vote weight |
| `preview_redeem(shares)` | — | Preview token return |
| `vault_state()` | — | Query pool totals |
| `set_min_stake(amount)` | admin | Configure minimum stake; `0` disables it |
| `get_min_stake()` | — | Read current minimum stake |
| `set_unstake_fee_bps(admin, bps)` | admin | Configure unstake fee (max 500 bps); `0` disables it |
| `get_unstake_fee_bps()` | — | Read current unstake fee in bps |
| `set_reward_rate_bps(rate_bps)` | admin | Configure base reward APR |
| `fund_reward_pool(admin_addr, amount)` | admin | Deposit claimable rewards |
| `set_boost_schedule(tiers)` | admin | Configure up to 5 reward-boost tiers |
| `get_boost_multiplier(user)` | — | Current reward multiplier for a user |
| `pause()` | admin | Emergency pause |
| `unpause()` | admin | Resume operations |
| `add_yield(admin_addr, amount)` | admin | Inject yield; raises share price |
| `transfer_admin(new_admin)` | admin | Rotate admin key |

## Using the CLI Helper

The repo includes [`scripts/pool.sh`](./scripts/pool.sh), an interactive helper for the most common pool operations:

- `stake`
- `unstake`
- `claim`
- `position`
- `pending`
- `pool-info`

The script reads `CONTRACT_ID` and `IDENTITY` from your shell environment or a local `.env` file:

```bash
CONTRACT_ID=CB...YOUR_CONTRACT_ID
IDENTITY=alice
NETWORK=testnet
```

You can run it interactively:

```bash
scripts/pool.sh
```

Or invoke a specific action directly:

```bash
scripts/pool.sh stake 25000000 GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF
scripts/pool.sh --dry-run pending GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF
```

Example output:

```text
$ scripts/pool.sh position GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF
Address: GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF
Staked shares: 2.5000000 (25000000 raw)
Pending reward: 0.1375000 (1375000 raw)
Boost multiplier: 11000 bps
```

## Events

| Event | Fields |
|---|---|
| `deposit` | `(depositor, amount, shares_minted)` |
| `withdraw` | `(withdrawer, shares_burned, amount_returned)` |
| `paused` | `(admin)` |
| `unpaused` | `(admin)` |
| `yield_add` | `(admin, amount)` |

## Roadmap / Open Issues

The following features are planned and tracked as open issues — great targets for Wave contributors:

- [ ] Yield accrual mechanism (admin deposits yield into the pool)
- [ ] Deposit/withdraw fee with configurable basis points
- [ ] Maximum deposit cap per user
- [ ] Multi-token support
- [ ] Testnet deployment script
- [ ] Integration tests against Stellar testnet

See [Issues](../../issues) for the full list, including those tagged **`Stellar Wave`**.

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for setup instructions and the Wave contribution workflow.

## Security

See [docs/SECURITY.md](./docs/SECURITY.md) for the full security model, including:

- Complete list of admin-only functions and their effects
- What the admin can and cannot do (the admin **cannot** access user principal)
- Failure scenarios: paused vault, halted yield, key compromise
- Admin key rotation procedure via `transfer_admin`

This contract is unaudited. Do not use in production without an independent security audit. If you find a vulnerability, please open a private [GitHub Security Advisory](../../security/advisories/new) rather than a public issue.

## License

[MIT](./LICENSE)
# Stellar-Defi-Vault
