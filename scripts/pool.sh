#!/usr/bin/env sh
set -eu

DECIMALS=10000000
DRY_RUN=0

load_env() {
  if [ -f .env ]; then
    set -a
    # shellcheck disable=SC1091
    . ./.env
    set +a
  fi
}

show_help() {
  cat <<'EOF'
Usage:
  scripts/pool.sh [--dry-run] [command] [args...]

Commands:
  stake [amount] [address]
  unstake [shares] [address]
  claim [address]
  position [address]
  pending [address]
  pool-info

Environment:
  CONTRACT_ID   Soroban contract ID
  IDENTITY      Local stellar identity or address used as --source
  NETWORK       Named Stellar network, defaults to testnet
EOF
}

prompt_value() {
  label=$1
  default_value=${2-}

  if [ -n "$default_value" ]; then
    printf "%s [%s]: " "$label" "$default_value" >&2
  else
    printf "%s: " "$label" >&2
  fi

  IFS= read -r REPLY_VALUE
  if [ -z "$REPLY_VALUE" ]; then
    REPLY_VALUE=$default_value
  fi
}

ensure_config() {
  load_env

  if [ -z "${CONTRACT_ID:-}" ]; then
    prompt_value "Contract ID" ""
    CONTRACT_ID=$REPLY_VALUE
  fi

  if [ -z "${IDENTITY:-}" ]; then
    prompt_value "Identity" ""
    IDENTITY=$REPLY_VALUE
  fi

  NETWORK=${NETWORK:-testnet}
}

require_stellar() {
  if [ "$DRY_RUN" -eq 0 ] && ! command -v stellar >/dev/null 2>&1; then
    printf "Error: stellar CLI is not installed or not on PATH.\n" >&2
    exit 1
  fi
}

quote_arg() {
  printf "%s" "$1" | sed "s/'/'\\\\''/g"
}

print_command() {
  printf "stellar"
  for arg in "$@"; do
    printf " '%s'" "$(quote_arg "$arg")"
  done
  printf "\n"
}

run_contract() {
  fn=$1
  shift

  ensure_config
  require_stellar

  set -- contract invoke --id "$CONTRACT_ID" --source "$IDENTITY" --network "$NETWORK" --fn "$fn" "$@"

  if [ "$DRY_RUN" -eq 1 ]; then
    print_command "$@"
    return 0
  fi

  output=$(stellar "$@" 2>&1) || {
    printf "Error running '%s':\n%s\n" "$fn" "$output" >&2
    return 1
  }

  printf "%s" "$output" | tr -d '\r'
}

is_integer() {
  case "$1" in
    -[0-9]*|[0-9]*) return 0 ;;
    *) return 1 ;;
  esac
}

format_amount() {
  raw=$1
  sign=""

  if [ "${raw#-}" != "$raw" ]; then
    sign="-"
    raw=${raw#-}
  fi

  whole=$((raw / DECIMALS))
  fraction=$((raw % DECIMALS))
  printf "%s%s.%07d" "$sign" "$whole" "$fraction"
}

human_or_raw() {
  value=$1
  if is_integer "$value"; then
    format_amount "$value"
  else
    printf "%s" "$value"
  fi
}

run_stake() {
  amount=${1-}
  address=${2-${IDENTITY:-}}

  if [ -z "$amount" ]; then
    prompt_value "Amount (raw units, 7 decimals)" ""
    amount=$REPLY_VALUE
  fi

  prompt_value "Staker address" "$address"
  address=$REPLY_VALUE

  output=$(run_contract stake --staker "$address" --amount "$amount") || return 1
  [ "$DRY_RUN" -eq 1 ] && return 0

  printf "Staked %s tokens for %s\n" "$(format_amount "$amount")" "$address"
  printf "Minted shares: %s (%s raw)\n" "$(human_or_raw "$output")" "$output"
}

run_unstake() {
  shares=${1-}
  address=${2-${IDENTITY:-}}

  if [ -z "$shares" ]; then
    prompt_value "Shares to unstake (raw units)" ""
    shares=$REPLY_VALUE
  fi

  prompt_value "Staker address" "$address"
  address=$REPLY_VALUE

  output=$(run_contract unstake --staker "$address" --shares "$shares") || return 1
  [ "$DRY_RUN" -eq 1 ] && return 0

  printf "Burned shares: %s (%s raw)\n" "$(human_or_raw "$shares")" "$shares"
  printf "Returned tokens: %s (%s raw)\n" "$(human_or_raw "$output")" "$output"
}

run_claim() {
  address=${1-${IDENTITY:-}}
  prompt_value "Staker address" "$address"
  address=$REPLY_VALUE

  output=$(run_contract claim --staker "$address") || return 1
  [ "$DRY_RUN" -eq 1 ] && return 0

  printf "Claimed reward for %s: %s (%s raw)\n" "$address" "$(human_or_raw "$output")" "$output"
}

run_position() {
  address=${1-${IDENTITY:-}}
  prompt_value "Address" "$address"
  address=$REPLY_VALUE

  stake_weight=$(run_contract current_vote_weight --user "$address") || return 1
  pending_reward=$(run_contract calc_pending_reward --user "$address") || return 1
  boost_multiplier=$(run_contract get_boost_multiplier --user "$address") || return 1
  [ "$DRY_RUN" -eq 1 ] && return 0

  printf "Address: %s\n" "$address"
  printf "Staked shares: %s (%s raw)\n" "$(human_or_raw "$stake_weight")" "$stake_weight"
  printf "Pending reward: %s (%s raw)\n" "$(human_or_raw "$pending_reward")" "$pending_reward"
  printf "Boost multiplier: %s bps\n" "$boost_multiplier"
}

run_pending() {
  address=${1-${IDENTITY:-}}
  prompt_value "Address" "$address"
  address=$REPLY_VALUE

  output=$(run_contract calc_pending_reward --user "$address") || return 1
  [ "$DRY_RUN" -eq 1 ] && return 0

  printf "Pending reward for %s: %s (%s raw)\n" "$address" "$(human_or_raw "$output")" "$output"
}

run_pool_info() {
  total_staked=$(run_contract total_staked) || return 1
  vault_state=$(run_contract vault_state) || return 1
  min_stake=$(run_contract get_min_stake) || return 1
  reward_rate=$(run_contract get_reward_rate_bps) || return 1
  reward_pool=$(run_contract get_reward_pool_balance) || return 1
  withdrawal_limit=$(run_contract get_withdrawal_limit) || return 1
  lock_config=$(run_contract get_lock_config) || return 1
  boost_schedule=$(run_contract get_boost_schedule) || return 1
  [ "$DRY_RUN" -eq 1 ] && return 0

  printf "Pool info\n"
  printf "Total staked shares: %s (%s raw)\n" "$(human_or_raw "$total_staked")" "$total_staked"
  printf "Vault state: %s\n" "$vault_state"
  printf "Minimum stake: %s (%s raw)\n" "$(human_or_raw "$min_stake")" "$min_stake"
  printf "Reward APR: %s bps\n" "$reward_rate"
  printf "Reward pool: %s (%s raw)\n" "$(human_or_raw "$reward_pool")" "$reward_pool"
  printf "Withdrawal limit: %s (%s raw)\n" "$(human_or_raw "$withdrawal_limit")" "$withdrawal_limit"
  printf "Lock config: %s\n" "$lock_config"
  printf "Boost schedule: %s\n" "$boost_schedule"
}

choose_action() {
  cat <<'EOF' >&2
Choose an action:
  1. stake
  2. unstake
  3. claim
  4. check position
  5. check pending reward
  6. pool info
EOF
  prompt_value "Selection" "1"

  case "$REPLY_VALUE" in
    1|stake) ACTION=stake ;;
    2|unstake) ACTION=unstake ;;
    3|claim) ACTION=claim ;;
    4|position) ACTION=position ;;
    5|pending) ACTION=pending ;;
    6|pool-info|pool_info) ACTION=pool-info ;;
    *)
      printf "Unknown selection: %s\n" "$REPLY_VALUE" >&2
      exit 1
      ;;
  esac
}

load_env

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      show_help
      exit 0
      ;;
    *)
      break
      ;;
  esac
done

ACTION=${1-}
if [ -n "$ACTION" ]; then
  shift
else
  choose_action
fi

case "$ACTION" in
  stake)
    run_stake "${1-}" "${2-}"
    ;;
  unstake)
    run_unstake "${1-}" "${2-}"
    ;;
  claim)
    run_claim "${1-}"
    ;;
  position)
    run_position "${1-}"
    ;;
  pending)
    run_pending "${1-}"
    ;;
  pool-info|pool_info)
    run_pool_info
    ;;
  *)
    printf "Unknown command: %s\n" "$ACTION" >&2
    show_help >&2
    exit 1
    ;;
esac
