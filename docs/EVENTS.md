# Event Schema

All events include the current ledger sequence number (`u32`) in their data payload so
that indexers can reconstruct full pool history from events alone.

## Topic conventions
- First topic: action symbol (≤ 9 ASCII chars)
- Second topic (where present): the relevant address (user, admin, etc.)

## Event Reference

### `deposit`
Emitted on every successful stake/deposit.

| Field | Type | Description |
|-------|------|-------------|
| topic[0] | Symbol | `"deposit"` |
| topic[1] | Address | depositor |
| data.0 | i128 | token amount deposited |
| data.1 | i128 | shares minted |
| data.2 | u32 | ledger sequence |

### `withdraw`
Emitted on every successful unstake/withdraw.

| Field | Type | Description |
|-------|------|-------------|
| topic[0] | Symbol | `"withdraw"` |
| topic[1] | Address | withdrawer |
| data.0 | i128 | shares burned |
| data.1 | i128 | token amount returned |
| data.2 | u32 | ledger sequence |

### `pos_open` — position_opened
Emitted only on the **first stake** for a user (when their position goes from 0 → non-zero).

| Field | Type | Description |
|-------|------|-------------|
| topic[0] | Symbol | `"pos_open"` |
| topic[1] | Address | user |
| data.0 | i128 | initial stake amount |
| data.1 | u32 | ledger sequence |

### `pos_clos` — position_closed
Emitted when a user fully unstakes (position reaches 0).

| Field | Type | Description |
|-------|------|-------------|
| topic[0] | Symbol | `"pos_clos"` |
| topic[1] | Address | user |
| data.0 | u32 | ledger sequence |

### `paused`
Emitted when the vault is paused by the admin.

| Field | Type | Description |
|-------|------|-------------|
| topic[0] | Symbol | `"paused"` |
| topic[1] | Address | admin |
| data.0 | u32 | ledger sequence |

### `unpaused`
Emitted when the vault is unpaused by the admin.

| Field | Type | Description |
|-------|------|-------------|
| topic[0] | Symbol | `"unpaused"` |
| topic[1] | Address | admin |
| data.0 | u32 | ledger sequence |

### `rate_chg` — rate_changed
Emitted when the admin changes the reward rate.

| Field | Type | Description |
|-------|------|-------------|
| topic[0] | Symbol | `"rate_chg"` |
| data.0 | u32 | old rate in basis points |
| data.1 | u32 | new rate in basis points |
| data.2 | u32 | ledger sequence |

### `yield_add` — yield_added
Emitted when the admin injects yield into the vault.

| Field | Type | Description |
|-------|------|-------------|
| topic[0] | Symbol | `"yield_add"` |
| topic[1] | Address | admin |
| data.0 | i128 | amount added |
| data.1 | u32 | ledger sequence |

### `admin_set` — admin_transferred
Emitted when the admin role is transferred to a new address.

| Field | Type | Description |
|-------|------|-------------|
| topic[0] | Symbol | `"admin_set"` |
| topic[1] | Address | old admin |
| data.0 | Address | new admin |
| data.1 | u32 | ledger sequence |

### `wd_limit` — withdrawal_limit_updated
Emitted when the admin sets a new per-transaction withdrawal limit.

| Field | Type | Description |
|-------|------|-------------|
| topic[0] | Symbol | `"wd_limit"` |
| topic[1] | Address | admin |
| data.0 | i128 | new limit in shares |
| data.1 | u32 | ledger sequence |
