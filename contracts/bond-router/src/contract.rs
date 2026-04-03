#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    to_json_binary, Binary, Coin, Decimal, Deps, DepsMut, Env, Fraction, MessageInfo, Reply,
    Response, StdResult, SubMsg, Uint128, Uint256, WasmMsg,
};
use cw2::set_contract_version;
use cw20::{BalanceResponse, Cw20ExecuteMsg, Cw20QueryMsg};
use cw_utils::must_pay;
use wyndex::asset::{Asset, AssetInfo};

use wynd_lsd_hub::msg::{
    ConfigResponse as HubConfigResponse, ExchangeRateResponse, ExecuteMsg as HubExecuteMsg,
    QueryMsg as HubQueryMsg, SupplyResponse as HubSupplyResponse,
};
use wyndex::pair::{
    ExecuteMsg as PairExecuteMsg, QueryMsg as PairQueryMsg, SimulationResponse,
    SpotPricePredictionResponse,
};

use crate::error::ContractError;
use crate::msg::{ConfigResponse, ExecuteMsg, InstantiateMsg, QueryMsg, SimulateResponse};
use crate::state::{Config, CONFIG, REPLY_INFO};

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:bond-router";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

// ids for reply handlers
const REPLY_BOND_ID: u64 = 1;

// Number of iterations to use to predict spot price.
// TODO: check gas usage and see if we tune up or down
const ITERATIONS: u8 = 10;

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    let hub = deps.api.addr_validate(&msg.hub)?;
    let pair = deps.api.addr_validate(&msg.pair)?;

    // get static info from the lsd hub
    let cfg: HubConfigResponse = deps
        .querier
        .query_wasm_smart(&hub, &HubQueryMsg::Config {})
        .map_err(|_| ContractError::NotLsdHub)?;
    let sup: HubSupplyResponse = deps
        .querier
        .query_wasm_smart(&hub, &HubQueryMsg::Supply {})
        .map_err(|_| ContractError::NotLsdHub)?;
    let lsd_token = cfg.token_contract;
    let bond_denom = sup.supply.bond_denom;

    // save config
    let config = Config {
        hub,
        pair,
        lsd_token,
        bond_denom,
    };
    CONFIG.save(deps.storage, &config)?;

    Ok(Response::new())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Bond {} => execute_bond(deps, info),
    }
}

/// Algorithm:
///   1. Ensure we are sent some of the proper tokens in funds
///   2. Check the current exchange rate for bonding
///   3. Check how many tokens can be swapped up to that rate on the pool
///   4. Create messages swapping those tokens (if any) and bonding remaining tokens (if any)
///   5. Temp store the sender to get rewards
///   6. Reply::on_success for last message, sending all lsd_token to this temp.sender
pub fn execute_bond(deps: DepsMut, info: MessageInfo) -> Result<Response, ContractError> {
    let cfg = CONFIG.load(deps.storage)?;
    let mut pay = must_pay(&info, &cfg.bond_denom)?;

    let ExchangeRateResponse { exchange_rate } = deps
        .querier
        .query_wasm_smart(&cfg.hub, &HubQueryMsg::ExchangeRate {})?;

    let SpotPricePredictionResponse { trade } = deps.querier.query_wasm_smart(
        &cfg.pair,
        &PairQueryMsg::SpotPricePrediction {
            offer: AssetInfo::Native(cfg.bond_denom.clone()),
            ask: AssetInfo::Token(cfg.lsd_token.to_string()),
            max_trade: Uint256::try_from(pay).unwrap(),
            target_price: exchange_rate.into(),
            iterations: ITERATIONS,
        },
    )?;

    let mut res = Response::new().add_attribute("execute", "bond");

    // if there is something to swap, swap it
    if let Some(to_swap) = trade {
        let msg = WasmMsg::Execute {
            contract_addr: cfg.pair.into_string(),
            msg: to_json_binary(&PairExecuteMsg::Swap {
                offer_asset: Asset {
                    info: AssetInfo::Native(cfg.bond_denom.clone()),
                    amount: to_swap,
                },
                ask_asset_info: Some(AssetInfo::Token(cfg.lsd_token.into_string())),
                belief_price: None,
                max_spread: Some(Decimal::percent(50).into()),
                // send back directly to original sender, so no reply needed
                to: Some(info.sender.to_string()),
                referral_address: None,
                referral_commission: None,
            })?,
            funds: vec![Coin {
                denom: cfg.bond_denom.clone(),
                amount: Uint256::from(to_swap),
            }],
        };
        res = res
            .add_message(msg)
            .add_attribute("swap", "true")
            .add_attribute("amount", to_swap);

        // update remaining pay for bonding
        pay -= Uint256::from(to_swap);
    }

    // anything left should be bonded, this
    if !pay.is_zero() {
        // just bond
        let msg = WasmMsg::Execute {
            contract_addr: cfg.hub.into_string(),
            msg: to_json_binary(&HubExecuteMsg::Bond {})?,
            funds: vec![Coin {
                denom: cfg.bond_denom,
                amount: pay,
            }],
        };
        // we need to handle the reply here to send it back
        res = res
            .add_submessage(SubMsg::reply_on_success(msg, REPLY_BOND_ID))
            .add_attribute("bond", "true")
            .add_attribute("amount", pay);

        // store some state for the reply block
        REPLY_INFO.save(deps.storage, &info.sender)?;
    }

    // TODO: add some events here?
    Ok(res)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn reply(deps: DepsMut, env: Env, reply: Reply) -> Result<Response, ContractError> {
    match reply.id {
        // only on success and we just query current state, ignore response data
        REPLY_BOND_ID => reply_bond_callback(deps, env),
        _ => Err(ContractError::InvalidReplyId(reply.id)),
    }
}

pub fn reply_bond_callback(deps: DepsMut, env: Env) -> Result<Response, ContractError> {
    // figure out who to send back to
    let recipient = REPLY_INFO.load(deps.storage)?.to_string();

    // figure out how much we received
    let cfg = CONFIG.load(deps.storage)?;
    let amount = deps
        .querier
        .query_wasm_smart::<BalanceResponse>(
            cfg.lsd_token.clone(),
            &Cw20QueryMsg::Balance {
                address: env.contract.address.to_string(),
            },
        )?
        .balance;

    let mut response = Response::new();
    if !amount.is_zero() {
        // send it back
        response = response.add_message(WasmMsg::Execute {
            contract_addr: cfg.lsd_token.to_string(),
            msg: to_json_binary(&Cw20ExecuteMsg::Transfer { recipient, amount })?,
            funds: vec![],
        });
    }

    Ok(response)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => {
            let cfg: ConfigResponse = CONFIG.load(deps.storage)?.into();
            to_json_binary(&cfg)
        }
        QueryMsg::Simulate { bond } => to_json_binary(&query_simulate(deps, bond)?),
    }
}

pub fn query_simulate(deps: Deps, bond: Uint128) -> StdResult<SimulateResponse> {
    let cfg = CONFIG.load(deps.storage)?;

    let ExchangeRateResponse { exchange_rate } = deps
        .querier
        .query_wasm_smart(&cfg.hub, &HubQueryMsg::ExchangeRate {})?;

    let SpotPricePredictionResponse { trade } = deps.querier.query_wasm_smart(
        &cfg.pair,
        &PairQueryMsg::SpotPricePrediction {
            offer: AssetInfo::Native(cfg.bond_denom.clone()),
            ask: AssetInfo::Token(cfg.lsd_token.to_string()),
            max_trade: Uint256::try_from(bond).unwrap(),
            target_price: exchange_rate.into(),
            iterations: ITERATIONS,
        },
    )?;

    // how many lsd we get from bonding
    let bond: Uint256 = Uint256::new(bond.u128()) - trade.unwrap_or_default();
    // let mut lsd_val = bond / exchange_rate;
    let mut lsd_val: Uint256 =
        bond.multiply_ratio(exchange_rate.denominator(), exchange_rate.numerator());

    if let Some(trade) = trade {
        // simulate swap to see how much would be there
        let res: SimulationResponse = deps.querier.query_wasm_smart(
            &cfg.pair,
            &PairQueryMsg::Simulation {
                offer_asset: Asset {
                    info: AssetInfo::Native(cfg.bond_denom.clone()),
                    amount: trade,
                },
                ask_asset_info: Some(AssetInfo::Token(cfg.lsd_token.to_string())),
                referral: false,
                referral_commission: None,
            },
        )?;

        // add this to what we get from bonding
        lsd_val += res.return_amount;
    }

    Ok(SimulateResponse { lsd_val })
}
