#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    ensure, to_json_binary, Addr, Binary, Decimal, Deps, DepsMut, Env, MessageInfo, Reply,
    Response, StdError, StdResult, SubMsg, WasmMsg,
};
use cw2::ensure_from_older_version;
use cw2::set_contract_version;
use cw20::MinterResponse;
use cw20_base::msg::InstantiateMsg as Cw20InstantiateMsg;

use crate::error::ContractError;
use crate::msg::{
    ConfigResponse, ExecuteMsg, InstantiateMsg, MigrateMsg, QueryMsg, ValidatorSetResponse,
};
use crate::state::{
    Config, StakeInfo, Supply, BONDED, CLAIMS, CONFIG, SLASHINGS, STAKE_INFO, SUPPLY, TMP_STATE,
};
use crate::valset::valset_change_redelegation_messages;

use semver::Version;

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:wynd-lsd-hub";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

const AFTER_TOKEN_CREATION_REPLY: u64 = 1;
/// This id will be set on the last withdrawal submessage
/// so that we know when to reinvest
const AFTER_WITHDRAW_REPLY: u64 = 2;
/// Extra id for all but the last withdrawal submessage
const AFTER_WITHDRAW_INTERMITTENT_REPLY: u64 = 3;

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    // Store the bonded denom for later and create the LSD token
    let supply = Supply::new(deps.querier.query_bonded_denom()?);
    SUPPLY.save(deps.storage, &supply)?;

    // Verify commission is greater than 0.0 and no higher than 0.50
    if msg.commission < Decimal::zero() || msg.commission > Decimal::percent(50) {
        return Err(ContractError::InvalidCommission {});
    }

    // Verify all the weights included in msg.validators sums to 1.0
    let total_weight: Decimal = msg.validators.iter().map(|(_, w)| w).sum();
    if total_weight != Decimal::one() {
        return Err(ContractError::InvalidValidatorWeights {});
    }

    // Verify the liquidity discount
    ensure!(
        msg.liquidity_discount < Decimal::percent(50),
        ContractError::InvalidLiquidityDiscount {}
    );

    let info = StakeInfo {
        validators: msg.validators.clone(),
    };
    STAKE_INFO.save(deps.storage, &info)?;

    SLASHINGS.save(deps.storage, &vec![])?;
    BONDED.save(deps.storage, &vec![])?;

    let mut response = Response::default();

    // add validator attributes
    for (i, (validator, weight)) in msg.validators.into_iter().enumerate() {
        response = response
            .add_attribute(format!("validator_{}", i), validator)
            .add_attribute(format!("validator_{}_weight", i), weight.to_string());
    }

    // sanity checks
    ensure!(
        msg.epoch_period >= 3600 && msg.epoch_period <= 31_536_000,
        ContractError::InvalidEpochPeriod {}
    );
    ensure!(
        msg.unbond_period >= 3600 && msg.unbond_period <= 31_536_000,
        ContractError::InvalidUnbondPeriod {}
    );
    ensure!(
        msg.max_concurrent_unbondings != 0,
        ContractError::InvalidMaxConcurrentUnbondings {}
    );

    let next_epoch = env.block.time.seconds() + msg.epoch_period;
    let config = Config {
        token_contract: Addr::unchecked(""),
        treasury: deps.api.addr_validate(&msg.treasury)?,
        commission: msg.commission,
        epoch_period: msg.epoch_period,
        unbond_period: msg.unbond_period,
        owner: deps.api.addr_validate(&msg.owner)?,
        next_epoch,
        next_unbond: next_epoch,
        max_concurrent_unbondings: msg.max_concurrent_unbondings,
        liquidity_discount: msg.liquidity_discount,
        tombstone_treshold: msg.tombstone_treshold,
        slashing_safety_margin: msg.slashing_safety_margin,
    };
    CONFIG.save(deps.storage, &config)?;

    Ok(response.add_submessage(SubMsg::reply_on_success(
        WasmMsg::Instantiate {
            admin: Some(env.contract.address.to_string()), // use this contract as the initial admin so it can be changed later by a `MsgUpdateAdmin`
            code_id: msg.cw20_init.cw20_code_id,
            msg: to_json_binary(&Cw20InstantiateMsg {
                name: msg.cw20_init.name,
                symbol: msg.cw20_init.symbol,
                decimals: msg.cw20_init.decimals,
                initial_balances: msg.cw20_init.initial_balances,
                mint: Some(MinterResponse {
                    minter: env.contract.address.to_string(),
                    cap: None,
                }),
                marketing: msg.cw20_init.marketing,
            })?,
            funds: vec![],
            label: msg.cw20_init.label,
        },
        AFTER_TOKEN_CREATION_REPLY,
    )))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Receive(msg) => execute::handle_receive(deps, env, info, msg),
        ExecuteMsg::Claim {} => execute::claim(deps, env, info),
        ExecuteMsg::Bond {} => execute::bond(deps, env, info),
        ExecuteMsg::Reinvest {} => execute::reinvest(deps, env),
        ExecuteMsg::SetValidators { new_validators } => {
            execute::set_validators(deps, info, env, new_validators)
        }
        ExecuteMsg::UpdateLiquidityDiscount { new_discount } => {
            execute::update_liquidity_discount(deps, info, new_discount)
        }
        ExecuteMsg::CheckSlash {} => execute::check_slash(deps, env),
    }
}

mod execute {
    use std::collections::HashMap;

    use crate::{
        msg::ReceiveMsg,
        state::{unbondings_expiring_between, Slashing, TmpState, CLAIMS, SLASHINGS, UNBONDING},
        valset::ValsetChange,
    };
    use std::cmp::max;

    use super::*;
    use crate::state::CleanedSupply;
    use cosmwasm_std::{
        ensure, ensure_eq, from_json, to_json_binary, BankMsg, Coin, CosmosMsg, DistributionMsg,
        Order, Timestamp, Uint128, WasmMsg,
    };
    use cw20::{Cw20ExecuteMsg, Cw20ReceiveMsg};
    use cw_utils::must_pay;

    pub fn set_validators(
        deps: DepsMut,
        info: MessageInfo,
        env: Env,
        new_validators: Vec<(String, Decimal)>,
    ) -> Result<Response, ContractError> {
        // Only the 'owner' set in Instantiate can update the validator set
        let config = CONFIG.load(deps.storage)?;
        ensure_eq!(info.sender, config.owner, ContractError::Unauthorized {});

        let mut supply = CleanedSupply::load(deps.storage, &env)?;
        let mut stake_info = STAKE_INFO.load(deps.storage)?;

        let mut response = Response::new();
        // If the sum of all balances is non zero, then we need to redelegate. Otherwise just update the valset
        if supply.total_bonded != Uint128::zero() {
            let bonded = BONDED.load(deps.storage)?;

            let ValsetChange {
                messages,
                new_balances,
            } = valset_change_redelegation_messages(
                &supply,
                bonded.iter().map(|(k, v)| (k, *v)),
                new_validators.iter().map(|(k, v)| (k, *v)),
            )?;
            response = response.add_messages(messages);
            BONDED.save(deps.storage, &new_balances)?;
            supply.total_bonded = new_balances.into_iter().map(|(_, v)| v).sum();
            SUPPLY.save(deps.storage, &supply)?;
        }

        stake_info.validators = new_validators;
        STAKE_INFO.save(deps.storage, &stake_info)?;

        Ok(response)
    }

    pub fn bond(deps: DepsMut, env: Env, info: MessageInfo) -> Result<Response, ContractError> {
        let mut supply = CleanedSupply::load(deps.storage, &env)?;

        // determine the ratio before these funds were received
        let paid = must_pay(&info, &supply.bond_denom)?;
        let balance = supply.balance(deps.as_ref(), &env)?;
        // The bank transfer happens before the contract runs, so `balance` already
        // includes `paid`. Subtract it to get the pre-payment state for correct rate.
        let paid_u128 = Uint128::try_from(paid).map_err(|_| StdError::msg("paid overflow"))?;
        let balance_before = balance.saturating_sub(paid_u128);

        // calculate how many shares to issue, this is determined by the exchange rate
        let issue = paid.mul_floor(supply.shares_per_token(balance_before));
        supply.issued += Uint128::try_from(issue).unwrap();
        SUPPLY.save(deps.storage, &supply)?;

        let config = CONFIG.load(deps.storage)?;
        // issue the stake token for sender
        let mint_msg = Cw20ExecuteMsg::Mint {
            recipient: info.sender.to_string(),
            amount: issue,
        };

        let res: Response = Response::new().add_message(CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr: config.token_contract.to_string(),
            msg: to_json_binary(&mint_msg)?,
            funds: vec![],
        }));

        Ok(res)
    }

    pub fn handle_receive(
        deps: DepsMut,
        env: Env,
        info: MessageInfo,
        msg: Cw20ReceiveMsg,
    ) -> Result<Response, ContractError> {
        match from_json(&msg.msg)? {
            ReceiveMsg::Unbond {} => unbond(
                deps,
                env,
                info.sender,
                Uint128::try_from(msg.amount).unwrap(),
                msg.sender,
            ),
        }
    }

    pub fn unbond(
        deps: DepsMut,
        env: Env,
        contract_sender: Addr,
        amount: Uint128,
        sender: String,
    ) -> Result<Response, ContractError> {
        // make sure the sender is the token contract
        let config = CONFIG.load(deps.storage)?;
        if config.token_contract != contract_sender {
            return Err(ContractError::InvalidToken {});
        }

        let mut supply = CleanedSupply::load(deps.storage, &env)?;
        let balance = supply.balance(deps.as_ref(), &env)?;

        let native_amount = supply.unbond(amount.into(), balance);
        SUPPLY.save(deps.storage, &supply)?;

        // create a claim
        let sender = deps.api.addr_validate(&sender)?;
        // We don't update next_unbond if we never unbond... we must wait at least until next epoch
        let next_unbond = max(config.next_unbond, config.next_epoch);
        CLAIMS.create_claim(
            deps.storage,
            &sender,
            native_amount.into(),
            Timestamp::from_seconds(
                // this might be a little tight because it assumes we immediately call reinvest at next_unbond,
                // but it should not be a problem in practice, since the claiming will just fail until the funds are available
                next_unbond + config.unbond_period,
            ),
        )?;

        // burn the sent tokens
        let burn_msg = WasmMsg::Execute {
            contract_addr: config.token_contract.to_string(),
            msg: to_json_binary(&Cw20ExecuteMsg::Burn {
                amount: amount.into(),
            })?,
            funds: vec![],
        };

        Ok(Response::new().add_message(burn_msg))
    }

    pub fn claim(deps: DepsMut, env: Env, info: MessageInfo) -> Result<Response, ContractError> {
        let mut supply = SUPPLY.load(deps.storage)?;
        let balance = deps
            .querier
            .query_balance(&env.contract.address, &supply.bond_denom)?;

        let slashing_events = SLASHINGS.load(deps.storage)?;

        // check how much to send - min(balance, claims[sender]), and reduce the claim
        // Ensure we have enough balance to cover this and only send some claims if that is all we can cover
        let to_send = CLAIMS.claim_tokens(
            deps.storage,
            &info.sender,
            &env.block,
            |c| {
                let mut amount = c.amount;
                // adjust the claim amounts for slashing
                for slashing in slashing_events
                    .iter()
                    .filter(|s| s.start < c.release_at.seconds() && s.end > c.release_at.seconds())
                {
                    amount = amount.mul_floor(slashing.multiplier);
                }
                amount
            },
            Some(balance.amount.try_into().unwrap()),
        )?;
        if to_send.is_zero() {
            return Err(ContractError::NothingToClaim {});
        }
        // update total supply (lower claims)
        supply.claim(to_send)?;
        SUPPLY.save(deps.storage, &supply)?;

        // transfer tokens to the sender
        let res = Response::new()
            .add_message(BankMsg::Send {
                to_address: info.sender.to_string(),
                amount: vec![Coin {
                    denom: supply.bond_denom,
                    amount: to_send.try_into().unwrap(),
                }],
            })
            .add_attribute("action", "claim")
            .add_attribute("from", info.sender)
            .add_attribute("amount", to_send);
        Ok(res)
    }

    pub fn reinvest(deps: DepsMut, env: Env) -> Result<Response, ContractError> {
        // only allow this to be called once per epoch
        let mut config = CONFIG.load(deps.storage)?;
        config.next_epoch_after(&env)?;
        CONFIG.save(deps.storage, &config)?;

        let mut resp = Response::new();

        // get all validators, skipping any with zero weight
        let validators: Vec<_> = STAKE_INFO
            .load(deps.storage)?
            .validators
            .into_iter()
            .filter(|(_, w)| !w.is_zero())
            .collect();

        let supply = SUPPLY.load(deps.storage)?;

        // save current balance for comparison in reply
        let balance = supply.balance(deps.as_ref(), &env)?;
        TMP_STATE.save(deps.storage, &TmpState { balance })?;

        // withdraw rewards from all delegations
        if supply.total_bonded.is_zero() {
            // if we have never staked before, we can skip the withdraw step
            return reply::after_withdraw_rewards(deps, env).map_err(Into::into);
        } else {
            let len = validators.len();
            for (i, (validator, _)) in validators.into_iter().enumerate() {
                if i == len - 1 {
                    // for the last message, we need to get a reply in any case to continue in
                    // `reply::after_withdraw_rewards`
                    resp = resp.add_submessage(SubMsg::reply_always(
                        DistributionMsg::WithdrawDelegatorReward { validator },
                        AFTER_WITHDRAW_REPLY,
                    ));
                } else {
                    // we need to catch intermittent errors, so they don't fail the whole transaction
                    resp = resp.add_submessage(SubMsg::reply_on_error(
                        DistributionMsg::WithdrawDelegatorReward { validator },
                        AFTER_WITHDRAW_INTERMITTENT_REPLY,
                    ));
                }
            }
        }

        // reinvest execution will continue in `reply::after_withdraw_rewards`
        Ok(resp)
    }

    pub fn update_liquidity_discount(
        deps: DepsMut,
        info: MessageInfo,
        new_discount: Decimal,
    ) -> Result<Response, ContractError> {
        let mut config = CONFIG.load(deps.storage)?;

        // validation
        ensure_eq!(config.owner, info.sender, ContractError::Unauthorized {});
        ensure!(
            new_discount < Decimal::percent(50),
            ContractError::InvalidLiquidityDiscount {}
        );

        config.liquidity_discount = new_discount;
        CONFIG.save(deps.storage, &config)?;

        Ok(Response::new()
            .add_attribute("action", "update_liquidity_discount")
            .add_attribute("liquidity_discount", new_discount.to_string()))
    }

    pub fn check_slash(deps: DepsMut, env: Env) -> Result<Response, ContractError> {
        // 0.00001 = 0.001%
        let slashing_threshold: Decimal = Decimal::new(Uint128::new(10u128.pow(18 - 5)));

        let supply: Supply = SUPPLY.load(deps.storage)?;

        // ensure safety margin around unbonding periods
        let now = env.block.time.seconds();
        let slashing_safety_margin = CONFIG.load(deps.storage)?.slashing_safety_margin;
        // we do not call `supply.cleanup_unbonding` yet,
        // since we want to also check supposedly finished unbondings
        let unbonding = unbondings_expiring_between(
            deps.storage,
            now - slashing_safety_margin,
            now + slashing_safety_margin,
        )
        .next()
        .is_some();
        ensure!(!unbonding, ContractError::UnbondingTooClose {});

        // check if we have any slashing events by comparing queried delegations to our state
        // now we have to cleanup old unbondings
        let mut supply = supply.cleanup_unbonding(deps.storage, &env)?;

        let mut bonded = BONDED.load(deps.storage)?;
        let stored_delegations: HashMap<_, _> = bonded.iter().map(|(v, b)| (v, *b)).collect();

        let queried_delegations = deps.querier.query_all_delegations(env.contract.address)?;
        let slashed_validators: HashMap<_, _> = queried_delegations
            .iter()
            .filter_map(|d| {
                let stored = stored_delegations
                    .get(&d.validator)
                    .copied()
                    .unwrap_or_default();

                // if difference is larger than threshold, this validator was slashed
                if stored.saturating_sub(Uint128::try_from(d.amount.amount).unwrap())
                    >= stored.mul_floor(slashing_threshold)
                {
                    // keep track of multiplier
                    Some((
                        &d.validator,
                        Decimal::from_ratio(Uint128::try_from(d.amount.amount).unwrap(), stored),
                    ))
                } else {
                    None
                }
            })
            .collect();
        if slashed_validators.is_empty() {
            // no slashing detected
            return Ok(Response::new().add_attribute("slashed", "false"));
        }

        // we were slashed, so we need to update our state
        // we also keep track of the old total for calculating the global multiplier to adjust claims
        let (old_total_bonded, old_total_unbonding) = (supply.total_bonded, supply.total_unbonding);
        bonded = bonded
            .into_iter()
            .map(|(validator, mut amount)| {
                if let Some(multiplier) = slashed_validators.get(&validator) {
                    amount = amount.mul_floor(*multiplier);
                }
                (validator, amount)
            })
            .collect();
        supply.total_bonded = bonded.iter().map(|(_, b)| *b).sum();
        BONDED.save(deps.storage, &bonded)?;

        let mut unbondings = UNBONDING
            .range(deps.storage, None, None, Order::Ascending)
            .collect::<StdResult<Vec<_>>>()?;
        for (expiration, unbondings) in unbondings.iter_mut() {
            let mut changed = false;
            for ub in unbondings.iter_mut() {
                if let Some(multiplier) = slashed_validators.get(&ub.validator) {
                    ub.amount = ub.amount.mul_floor(*multiplier);
                    changed = true;
                }
            }
            // only change entries if there actually was a slashed validator
            if changed {
                UNBONDING.save(deps.storage, *expiration, unbondings)?;
            }
        }
        supply.total_unbonding = unbondings
            .iter()
            .flat_map(|(_, u)| u)
            .map(|u| u.amount)
            .sum();

        // sanity check
        #[cfg(debug_assertions)]
        cosmwasm_std::assert_approx_eq!(
            supply.total_bonded,
            queried_delegations
                .iter()
                .map(|d| d.amount.amount)
                .map(|x| Uint128::try_from(x).unwrap())
                .sum::<Uint128>(),
            "0.0002"
        );

        let response = Response::new()
            .add_attribute("slashed", "true")
            .add_attribute("bonded_slashed", old_total_bonded - supply.total_bonded);

        // we also need to update the pending claims
        if old_total_unbonding.is_zero() {
            SUPPLY.save(deps.storage, &supply)?;
            return Ok(response.add_attribute("unbonded_slashed", Uint128::zero()));
        }
        let global_unbonding_multiplier =
            Decimal::from_ratio(supply.total_unbonding, old_total_unbonding);

        // we need to update the pending claims, but only the part that is actually unbonding
        // (part of the claims can be in the contract balance, which is not slashed)
        supply.claims = (supply.claims - old_total_unbonding) + supply.total_unbonding;
        SUPPLY.save(deps.storage, &supply)?;

        let unbonding_period = CONFIG.load(deps.storage)?.unbond_period;
        SLASHINGS.update(deps.storage, |mut slashings| -> StdResult<_> {
            slashings.push(Slashing {
                start: env.block.time.seconds(),
                end: env.block.time.plus_seconds(unbonding_period).seconds(),
                multiplier: global_unbonding_multiplier,
            });
            Ok(slashings)
        })?;

        Ok(response.add_attribute(
            "unbonded_slashed",
            old_total_unbonding - supply.total_unbonding,
        ))
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut, env: Env, reply: Reply) -> Result<Response, ContractError> {
    match reply.id {
        AFTER_TOKEN_CREATION_REPLY => {
            let result = reply
                .result
                .into_result()
                .map_err(|e| StdError::msg(e.to_string()))?;
            let res = cw_utils::parse_instantiate_response_data(
                result
                    .msg_responses
                    .get(0)
                    .cloned()
                    .map(|v| v.value)
                    .or(result.data)
                    .unwrap()
                    .as_slice(),
            )
            .map_err(|_| StdError::msg("MsgInstantiateContractResponse: failed to parse data"))?;

            // Pass the contract admin of this contract to the Token contract
            let contract_info = deps
                .querier
                .query_wasm_contract_info(env.contract.address)?;

            let admin = contract_info.admin.unwrap();

            let mut config = CONFIG.load(deps.storage)?;
            config.token_contract = deps.api.addr_validate(&res.contract_address)?;

            // update the contract admin
            let msg = WasmMsg::UpdateAdmin {
                contract_addr: res.contract_address,
                admin: admin.to_string(),
            };
            let resp = Response::new().add_submessage(SubMsg::new(msg));
            CONFIG.save(deps.storage, &config)?;
            Ok(resp)
        }
        AFTER_WITHDRAW_INTERMITTENT_REPLY => {
            // ignore intermittent replies
            Ok(Response::default())
        }
        AFTER_WITHDRAW_REPLY => {
            // reinvest all received rewards, even if some of the withdrawals failed
            reply::after_withdraw_rewards(deps, env)
        }
        id => Err(ContractError::Std(StdError::msg(format!(
            "invalid reply id: {}; must be 1",
            id
        )))),
    }
}

mod reply {
    use std::{cmp::Ordering, collections::BTreeMap};

    use crate::state::{CleanedSupply, Unbonding, UNBONDING};
    use cosmwasm_std::{coins, BankMsg, Coin, StakingMsg, Uint128};

    use super::*;

    pub fn after_withdraw_rewards(deps: DepsMut, env: Env) -> Result<Response, ContractError> {
        let mut supply = CleanedSupply::load(deps.storage, &env)?;
        let mut balance = supply.balance(deps.as_ref(), &env)?;

        // early return if nothing to delegate
        if balance.is_zero() {
            return Ok(Response::new());
        }

        let mut config = CONFIG.load(deps.storage)?;
        let mut resp = Response::new();

        // send commission to the treasury
        let rewards = balance - TMP_STATE.load(deps.storage)?.balance;
        let commission_amount = rewards.mul_floor(config.commission);
        if !commission_amount.is_zero() {
            balance -= commission_amount;
            resp = resp.add_message(BankMsg::Send {
                to_address: config.treasury.to_string(),
                amount: coins(commission_amount.u128(), &supply.bond_denom),
            });
        }

        let mut bonded = BONDED
            .load(deps.storage)?
            .into_iter()
            .collect::<BTreeMap<_, _>>();

        // this is the amount of assets we (will) have available to pay claims
        let claim_coverage = balance + supply.total_unbonding;

        let stake_info = STAKE_INFO.load(deps.storage)?;
        match claim_coverage.cmp(&supply.claims) {
            Ordering::Greater => {
                // we have enough to pay all claims
                // delegate the surplus to the validators according to their weight
                let surplus = claim_coverage - supply.claims;

                // calculate how much each validator gets
                let mut val_payments: Vec<_> = stake_info
                    .validators
                    .into_iter()
                    .map(|(addr, weight)| (addr, surplus.mul_floor(weight)))
                    .collect();

                // calculate how much is rounded off when multiplying by the weight
                let remainder = surplus - val_payments.iter().map(|(_, amt)| amt).sum::<Uint128>();
                // first validator gets this on top
                val_payments[0].1 += remainder;

                // update bonded
                for (address, amount) in &val_payments {
                    match bonded.get_mut(address) {
                        Some(bonded) => *bonded += amount,
                        None => {
                            bonded.insert(address.clone(), *amount);
                        }
                    }
                }
                // create the messages
                resp = resp.add_messages(
                    val_payments
                        .into_iter()
                        .filter(|(_, amount)| !amount.is_zero())
                        .map(|(address, amount)| StakingMsg::Delegate {
                            validator: address,
                            amount: Coin {
                                denom: supply.bond_denom.clone(),
                                amount: amount.into(),
                            },
                        }),
                );
            }
            Ordering::Less => {
                // only execute this at most `config.max_concurrent_unbondings` times per unbonding period,
                // in order to avoid hitting the unbonding queue limit
                if config.next_unbond_after(&env).is_ok() {
                    CONFIG.save(deps.storage, &config)?;

                    // undelegate the difference from the validators according to their weight
                    let missing_liquidity = supply.claims - claim_coverage;

                    // calculate how much each validator gets
                    let mut val_payments: Vec<_> = stake_info
                        .validators
                        .into_iter()
                        .map(|(addr, weight)| (addr, missing_liquidity.mul_floor(weight)))
                        .collect();

                    // calculate how much is rounded off when multiplying by the weight
                    let mut remainder = missing_liquidity
                        - val_payments.iter().map(|(_, amt)| amt).sum::<Uint128>();
                    // take the remainder from the first validators that have enough stake
                    for (address, amount) in val_payments.iter_mut() {
                        if remainder.is_zero() {
                            break;
                        }

                        // if we have a remainder, add as much of it to the unbond amount as possible
                        let new_amount = std::cmp::min(*amount + remainder, bonded[address]);
                        // subtract the amount we added from the remainder
                        remainder -= new_amount - *amount;
                        *amount = new_amount;
                    }

                    // update bonded
                    for (address, amount) in &val_payments {
                        *bonded
                            .get_mut(address)
                            .expect("tried to undelegate non-existent stake") -= amount;
                    }

                    // store the unbondings
                    let unbondings: Vec<_> = val_payments
                        .into_iter()
                        .filter(|(_, amt)| !amt.is_zero())
                        .map(|(validator, amount)| Unbonding { validator, amount })
                        .collect();
                    let unbond_time = env.block.time.plus_seconds(config.unbond_period);
                    UNBONDING.save(deps.storage, unbond_time.seconds(), &unbondings)?;

                    // update total_unbonding
                    let total_unbonded: Uint128 = unbondings.iter().map(|u| u.amount).sum();
                    supply.total_unbonding += total_unbonded;

                    // generate the messages
                    let messages: Vec<_> = unbondings
                        .into_iter()
                        .map(|Unbonding { validator, amount }| StakingMsg::Undelegate {
                            validator,
                            amount: Coin {
                                denom: supply.bond_denom.clone(),
                                amount: amount.into(),
                            },
                        })
                        .collect();

                    resp = resp.add_messages(messages);
                }
            }
            _ => {}
        }

        // update how much is bonded
        let new_balances = bonded.into_iter().filter(|(_, b)| !b.is_zero()).collect();
        BONDED.save(deps.storage, &new_balances)?;
        supply.total_bonded = new_balances.iter().map(|(_, v)| *v).sum();
        SUPPLY.save(deps.storage, &supply)?;

        Ok(resp)
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    use QueryMsg::*;
    match msg {
        Config {} => query::config(deps),
        Claims { address } => {
            to_json_binary(&CLAIMS.query_claims(deps, &deps.api.addr_validate(&address)?)?)
        }
        ValidatorSet {} => to_json_binary(&ValidatorSetResponse {
            validator_set: STAKE_INFO.load(deps.storage)?.validators,
        }),
        LastReinvest {} => unimplemented!(),
        Supply {} => to_json_binary(&query::supply(deps)?),
        ExchangeRate {} => to_json_binary(&query::exchange_rate(deps, env)?),
        TargetValue {} => to_json_binary(&query::target_value(deps, env)?),
    }
}

pub mod query {
    use crate::msg::{ExchangeRateResponse, SupplyResponse, TargetValueResponse};
    use crate::state::CleanedSupply;

    use super::*;

    pub fn config(deps: Deps) -> StdResult<Binary> {
        let config = CONFIG.load(deps.storage)?;
        let resp: ConfigResponse = ConfigResponse {
            owner: config.owner,
            token_contract: config.token_contract,
            treasury: config.treasury,
            commission: config.commission,
            epoch_period: config.epoch_period,
            unbond_period: config.unbond_period,
        };
        to_json_binary(&resp)
    }

    pub fn exchange_rate(deps: Deps, env: Env) -> StdResult<ExchangeRateResponse> {
        let supply = CleanedSupply::load_for_query(deps.storage, &env)?;
        let exchange_rate = supply.tokens_per_share(supply.balance(deps, &env)?);

        Ok(ExchangeRateResponse { exchange_rate })
    }

    pub fn target_value(deps: Deps, env: Env) -> StdResult<TargetValueResponse> {
        let supply = CleanedSupply::load_for_query(deps.storage, &env)?;
        let exchange_rate = supply.tokens_per_share(supply.balance(deps, &env)?);
        let target_value =
            exchange_rate * (Decimal::one() - CONFIG.load(deps.storage)?.liquidity_discount);

        Ok(TargetValueResponse { target_value })
    }

    pub fn supply(deps: Deps) -> StdResult<SupplyResponse> {
        let loaded = SUPPLY.load(deps.storage)?;
        let supply = crate::msg::Supply {
            bond_denom: loaded.bond_denom,
            issued: loaded.issued,
            total_bonded: loaded.total_bonded,
            claims: loaded.claims,
            total_unbonding: loaded.total_unbonding,
        };
        Ok(SupplyResponse { supply })
    }
}

pub mod migration {
    use cosmwasm_schema::cw_serde;
    use cosmwasm_std::Uint128;
    use cw_utils::Expiration;

    #[cw_serde]
    pub struct OldUnbonding {
        pub amount: Uint128,
        pub expiration: Expiration,
        pub validator: String,
    }

    #[cw_serde]
    pub struct OldSupply {
        pub bond_denom: String,
        pub issued: Uint128,
        pub total_bonded: Uint128,
        pub bonded: Vec<(String, Uint128)>,
        pub claims: Uint128,
        pub unbonding: Vec<OldUnbonding>,
        pub total_unbonding: Uint128,
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn migrate(deps: DepsMut, _env: Env, msg: MigrateMsg) -> Result<Response, ContractError> {
    let version = ensure_from_older_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    if version < "1.1.0".parse::<Version>().unwrap() {
        use cw_storage_plus::Item;
        let old_storage: Item<migration::OldSupply> = Item::new("supply");
        let old_supply = old_storage.load(deps.storage)?;

        let new_supply = Supply {
            bond_denom: old_supply.bond_denom,
            issued: old_supply.issued,
            total_bonded: old_supply.total_bonded,
            claims: old_supply.claims,
            total_unbonding: old_supply.total_unbonding,
        };
        SUPPLY.save(deps.storage, &new_supply)?;

        BONDED.save(deps.storage, &old_supply.bonded)?;

        // UNBONDING doesn't need to be saved; This Map with current state it should be empty
        ensure!(
            old_supply.unbonding.is_empty(),
            ContractError::MigrationFailed {}
        );
    }

    if let Some(new_owner) = msg.new_owner {
        CONFIG.update::<_, StdError>(deps.storage, |mut config| {
            config.owner = deps.api.addr_validate(&new_owner)?;
            Ok(config)
        })?;
    }

    Ok(Response::new())
}

#[cfg(test)]
mod tests {
    use cosmwasm_std::{
        coins,
        testing::{message_info, mock_env, MockApi, MockStorage},
        to_json_binary, Addr, Binary, CosmosMsg, Decimal, DepsMut, Empty, Event, OwnedDeps,
        QuerierWrapper, Reply, ReplyOn, Response, StdError, SubMsg, SubMsgResponse, SubMsgResult,
        Uint128, Uint256, Validator, WasmMsg,
    };
    use cw20::{Cw20ExecuteMsg, MinterResponse};
    use cw20_base::msg::InstantiateMsg as Cw20InstantiateMsg;
    use cw_utils::ParseReplyError;

    use crate::{
        contract::{execute, instantiate},
        mock_querier::{mock_dependencies, WasmMockQuerier},
        msg::{InstantiateMsg, TokenInitInfo},
        state::CLAIMS,
        ContractError,
    };

    use super::reply;

    const TOKEN: &str = "ufun";
    const DAY: u64 = 24 * 60 * 60;
    const EPOCH: u64 = 23 * 60 * 60;

    fn increase_contract_balance(querier: &mut WasmMockQuerier, amount: u128) {
        let addr = mock_env().contract.address;
        let mut funds = QuerierWrapper::<Empty>::new(querier)
            .query_balance(&addr, TOKEN)
            .unwrap();
        funds.amount += Uint256::from(amount);
        querier.base.bank.update_balance(&addr, vec![funds]);
    }

    // this does a proper deposit of x coins, adjusting the balance of the contract
    fn do_deposit(
        deps: &mut OwnedDeps<MockStorage, MockApi, WasmMockQuerier>,
        sender: &Addr,
        amount: u128,
    ) {
        increase_contract_balance(&mut deps.querier, amount);

        let env = mock_env();
        let info = message_info(sender, &coins(amount.into(), TOKEN));
        let res = execute::bond(deps.as_mut(), env, info).unwrap();
        assert_eq!(1, res.messages.len());
    }

    fn register_validator(querier: &mut WasmMockQuerier, validator: &str) {
        let val = Validator::new(
            validator.to_string(),
            Decimal::percent(7),
            Decimal::percent(20),
            Decimal::percent(5),
        );
        querier.base.staking.update(TOKEN, &[val], &[]);
    }

    fn init(deps: DepsMut, owner: &str) -> Response {
        let msg = InstantiateMsg {
            treasury: MockApi::default().addr_make("treasury").to_string(),
            commission: Decimal::percent(10),
            validators: vec![(
                MockApi::default().addr_make("val1").to_string(),
                Decimal::percent(100),
            )],
            owner: owner.to_string(),

            epoch_period: EPOCH,
            unbond_period: 28 * DAY,
            max_concurrent_unbondings: 7,

            cw20_init: TokenInitInfo {
                label: "label".to_string(),
                cw20_code_id: 0,
                name: "funLSD".to_string(),
                symbol: "fLSD".to_string(),
                decimals: 6,
                initial_balances: vec![],
                marketing: None,
            },
            liquidity_discount: Decimal::percent(4),
            tombstone_treshold: Decimal::percent(3),
            slashing_safety_margin: 10 * 60,
        };

        let env = mock_env();
        let info = message_info(&Addr::unchecked(owner), &[]);
        instantiate(deps, env, info, msg).unwrap()
    }

    #[test]
    fn proper_init() {
        let mut deps = mock_dependencies(&[]);

        let msg = InstantiateMsg {
            treasury: MockApi::default().addr_make("treasury").to_string(),
            commission: Decimal::percent(10),
            validators: vec![(
                MockApi::default().addr_make("val1").to_string(),
                Decimal::percent(100),
            )],
            owner: MockApi::default().addr_make("owner").to_string(),

            epoch_period: 3600u64,
            unbond_period: 3600u64,
            max_concurrent_unbondings: 7,
            cw20_init: TokenInitInfo {
                label: "label".to_string(),
                cw20_code_id: 0,
                name: "funLSD".to_string(),
                symbol: "fLSD".to_string(),
                decimals: 6,
                initial_balances: vec![],
                marketing: None,
            },
            liquidity_discount: Decimal::percent(4),
            tombstone_treshold: Decimal::percent(3),
            slashing_safety_margin: 10 * 60,
        };

        let sender = Addr::unchecked("addr0000");
        // We can just call .unwrap() to assert this was a success
        let env = mock_env();
        let contract_addr = env.contract.address.to_string();
        let info = message_info(&sender, &[]);
        let res = instantiate(deps.as_mut(), env, info, msg).unwrap();
        assert_eq!(
            res.messages,
            vec![SubMsg {
                msg: WasmMsg::Instantiate {
                    code_id: 0u64,
                    msg: to_json_binary(&Cw20InstantiateMsg {
                        mint: Some(MinterResponse {
                            minter: contract_addr.clone(),
                            cap: None,
                        }),
                        name: "funLSD".to_string(),
                        symbol: "fLSD".to_string(),
                        decimals: 6,
                        initial_balances: vec![],
                        marketing: None,
                    })
                    .unwrap(),
                    funds: vec![],
                    admin: Some(contract_addr),
                    label: String::from("label"),
                }
                .into(),
                id: 1,
                gas_limit: None,
                reply_on: ReplyOn::Success,
                payload: Binary::new(vec![])
            },]
        );
    }

    #[test]
    fn reply_parse_data() {
        let mut deps = mock_dependencies(&[]);
        let env = mock_env();
        // A SubMsgResponse that is not a MsgInstantiateContractResponse
        let response = SubMsgResponse {
            data: Some(Binary::from_base64("MTIzCg==").unwrap()),
            events: vec![Event::new("wasm").add_attribute("fo", "ba")],
            msg_responses: vec![],
        };
        let result: SubMsgResult = SubMsgResult::Ok(response);
        let reply_msg = Reply {
            id: 1,
            result: result.clone(),
            gas_used: 0,
            payload: Binary::new(vec![]),
        };
        let err = reply(deps.as_mut(), env.clone(), reply_msg).unwrap_err();
        println!("{:#?}", err);
        //  Verify the error failed to parse data for the message type
        assert!(err.to_string().contains("failed to parse data"));

        // Try again with an invalid ID
        let reply_msg = Reply {
            id: 999,
            result,
            gas_used: 0,
            payload: Binary::new(vec![]),
        };
        let err = reply(deps.as_mut(), env, reply_msg).unwrap_err();
        //  Verify the error is invalid reply id
        assert!(err.to_string().contains(
            &ContractError::Std(StdError::msg("invalid reply id: 999; must be 1")).to_string()
        ));
    }

    #[test]
    fn invalid_init() {
        let mut deps = mock_dependencies(&[]);
        // Instantiate message with invalid commission
        let msg = InstantiateMsg {
            treasury: MockApi::default().addr_make("treasury").to_string(),
            commission: Decimal::percent(100),
            validators: vec![(
                MockApi::default().addr_make("val1").to_string(),
                Decimal::percent(100),
            )],
            owner: MockApi::default().addr_make("owner").to_string(),

            epoch_period: 3600u64,
            unbond_period: 3600u64,
            max_concurrent_unbondings: 7,
            cw20_init: TokenInitInfo {
                label: "label".to_string(),
                cw20_code_id: 0,
                name: "funLSD".to_string(),
                symbol: "fLSD".to_string(),
                decimals: 6,
                initial_balances: vec![],
                marketing: None,
            },
            liquidity_discount: Decimal::percent(4),
            tombstone_treshold: Decimal::percent(3),
            slashing_safety_margin: 10 * 60,
        };

        let sender = Addr::unchecked("addr0000");
        // We can just call .unwrap() to assert this was a success
        let env = mock_env();
        let info = message_info(&sender, &[]);
        // Verify the error is InvalidCommission
        assert!(matches!(
            instantiate(deps.as_mut(), env.clone(), info.clone(), msg).unwrap_err(),
            ContractError::InvalidCommission {},
        ));
        // Instantiate message with invalid validator weights
        let msg = InstantiateMsg {
            treasury: MockApi::default().addr_make("treasury").to_string(),
            commission: Decimal::percent(10),
            validators: vec![(
                MockApi::default().addr_make("val1").to_string(),
                Decimal::percent(50),
            )],
            owner: MockApi::default().addr_make("owner").to_string(),

            epoch_period: 3600u64,
            unbond_period: 3600u64,
            max_concurrent_unbondings: 7,
            cw20_init: TokenInitInfo {
                label: "label".to_string(),
                cw20_code_id: 0,
                name: "funLSD".to_string(),
                symbol: "fLSD".to_string(),
                decimals: 6,
                initial_balances: vec![],
                marketing: None,
            },
            liquidity_discount: Decimal::percent(4),
            tombstone_treshold: Decimal::percent(3),
            slashing_safety_margin: 10 * 60,
        };

        // Verify the error is InvalidCommission
        assert!(matches!(
            instantiate(deps.as_mut(), env.clone(), info.clone(), msg).unwrap_err(),
            ContractError::InvalidValidatorWeights {},
        ));

        // Instantiate message with a badd Liquidity Discount value
        let msg = InstantiateMsg {
            treasury: MockApi::default().addr_make("treasury").to_string(),
            commission: Decimal::percent(10),
            validators: vec![(
                MockApi::default().addr_make("val1").to_string(),
                Decimal::percent(100),
            )],
            owner: MockApi::default().addr_make("owner").to_string(),

            epoch_period: 3600u64,
            unbond_period: 3600u64,
            max_concurrent_unbondings: 7,
            cw20_init: TokenInitInfo {
                label: "label".to_string(),
                cw20_code_id: 0,
                name: "funLSD".to_string(),
                symbol: "fLSD".to_string(),
                decimals: 6,
                initial_balances: vec![],
                marketing: None,
            },
            liquidity_discount: Decimal::percent(100),
            tombstone_treshold: Decimal::percent(3),
            slashing_safety_margin: 10 * 60,
        };

        // Verify the error is InvalidCommission
        assert!(matches!(
            instantiate(deps.as_mut(), env, info, msg).unwrap_err(),
            ContractError::InvalidLiquidityDiscount {},
        ));
    }

    #[test]
    fn unbonding_burns_tokens() {
        let sender_addr = MockApi::default().addr_make("sender");
        let validator_addr = MockApi::default().addr_make("valid-val");
        let owner_addr = MockApi::default().addr_make("addr0000");

        let mut deps = mock_dependencies(&[]);

        // We can just call .unwrap() to assert this was a success
        let env = mock_env();

        register_validator(&mut deps.querier, &validator_addr.to_string());
        init(deps.as_mut(), &owner_addr.to_string());

        do_deposit(&mut deps, &sender_addr, 1700);
        let res = execute::unbond(
            deps.as_mut(),
            env,
            Addr::unchecked(""),
            100u128.into(),
            owner_addr.to_string(),
        )
        .unwrap();
        assert_eq!(
            res.messages[0].msg,
            CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: "".to_string(),
                msg: to_json_binary(&Cw20ExecuteMsg::Burn {
                    amount: 100u128.into()
                })
                .unwrap(),
                funds: vec![],
            })
        );
    }

    #[test]
    fn basic_claim_creation_works() {
        let sender = MockApi::default().addr_make("sender");
        let sender2 = MockApi::default().addr_make("sender2");
        let validator_addr = MockApi::default().addr_make("valid-val");
        let creator = MockApi::default().addr_make("creator");

        let mut deps = mock_dependencies(&[]);

        register_validator(&mut deps.querier, &validator_addr.to_string());

        init(deps.as_mut(), &creator.to_string());

        // initial deposits
        do_deposit(&mut deps, &sender, 1700);
        do_deposit(&mut deps, &sender2, 800);
        // create a claim
        execute::unbond(
            deps.as_mut(),
            mock_env(),
            Addr::unchecked(""),
            500u128.into(),
            sender.to_string(),
        )
        .unwrap();
        assert_eq!(
            1,
            CLAIMS
                .query_claims(deps.as_ref(), &sender)
                .unwrap()
                .claims
                .len()
        );

        // create a second claim
        execute::unbond(
            deps.as_mut(),
            mock_env(),
            Addr::unchecked(""),
            500u128.into(),
            sender.to_string(),
        )
        .unwrap();
        assert_eq!(
            2,
            CLAIMS
                .query_claims(deps.as_ref(), &sender)
                .unwrap()
                .claims
                .len()
        );
    }

    #[test]
    fn epoch_handling() {
        let mut deps = mock_dependencies(&[]);
        let mut env = mock_env();

        let msg = InstantiateMsg {
            treasury: MockApi::default().addr_make("treasury").to_string(),
            commission: Decimal::percent(10),
            validators: vec![(
                MockApi::default().addr_make("val1").to_string(),
                Decimal::percent(100),
            )],
            owner: MockApi::default().addr_make("owner").to_string(),

            epoch_period: 3600u64,
            unbond_period: 3600u64,
            max_concurrent_unbondings: 7,
            cw20_init: TokenInitInfo {
                label: "label".to_string(),
                cw20_code_id: 0,
                name: "funLSD".to_string(),
                symbol: "fLSD".to_string(),
                decimals: 6,
                initial_balances: vec![],
                marketing: None,
            },
            liquidity_discount: Decimal::percent(4),
            tombstone_treshold: Decimal::percent(3),
            slashing_safety_margin: 10 * 60,
        };

        let sender = Addr::unchecked("addr0000");

        let info = message_info(&sender, &[]);
        instantiate(deps.as_mut(), env.clone(), info, msg).unwrap();

        // update the epoch timer once
        env.block.time = env.block.time.plus_seconds(3600);
        super::execute::reinvest(deps.as_mut(), env.clone()).unwrap();

        // wait until just before the next epoch
        env.block.time = env.block.time.plus_seconds(3599);
        assert!(matches!(
            super::execute::reinvest(deps.as_mut(), env.clone()).unwrap_err(),
            ContractError::EpochNotReached { next_epoch: _ },
        ));

        // now right at the epoch
        env.block.time = env.block.time.plus_seconds(1);
        super::execute::reinvest(deps.as_mut(), env.clone()).unwrap();

        // skip a few epochs
        env.block.time = env.block.time.plus_seconds(3600 * 5 + 1);
        super::execute::reinvest(deps.as_mut(), env.clone()).unwrap();

        // next epoch should be sooner than epoch period, since it keeps the same rythm
        // and we triggered last epoch 1 second too late
        env.block.time = env.block.time.plus_seconds(3599);
        super::execute::reinvest(deps.as_mut(), env).unwrap();
    }
}
