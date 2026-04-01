#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    ensure, to_json_binary, Addr, Binary, Decimal, Deps, DepsMut, Empty, Env, MessageInfo,
    Response, StdResult, WasmMsg,
};
use cw2::set_contract_version;
// use cw_utils::ensure_from_older_version;

use cw_placeholder::contract::CONTRACT_NAME as PLACEHOLDER_CONTRACT_NAME;
use wynd_lsd_hub::msg::ExecuteMsg as HubExecuteMsg;

use semver::Version;

use crate::error::ContractError;
use crate::msg::{AdapterQueryMsg, InstantiateMsg, MigrateMsg};
use crate::state::{Config, CONFIG};

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:gauge-adapter";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    ensure!(
        msg.max_commission > Decimal::zero() && msg.max_commission <= Decimal::one(),
        ContractError::InvalidMaxCommission {}
    );

    let config = Config {
        hub: deps.api.addr_validate(&msg.hub)?,
        max_commission: msg.max_commission,
    };
    CONFIG.save(deps.storage, &config)?;

    Ok(Response::new())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    _deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    _msg: Empty,
) -> Result<Response, ContractError> {
    Ok(Response::new())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, _env: Env, msg: AdapterQueryMsg) -> StdResult<Binary> {
    match msg {
        AdapterQueryMsg::Config {} => to_json_binary(&CONFIG.load(deps.storage)?),
        AdapterQueryMsg::AllOptions {} => to_json_binary(&query::all_options(deps)?),
        AdapterQueryMsg::CheckOption { option } => {
            to_json_binary(&query::check_option(deps, option)?)
        }
        AdapterQueryMsg::SampleGaugeMsgs { selected } => {
            to_json_binary(&query::sample_gauge_msgs(deps, selected)?)
        }
    }
}

mod query {
    use cosmwasm_std::Decimal;

    use crate::{
        msg::{AllOptionsResponse, CheckOptionResponse, SampleGaugeMsgsResponse},
        state::CONFIG,
    };

    use super::*;

    pub fn all_options(deps: Deps) -> StdResult<AllOptionsResponse> {
        let max_commission = CONFIG.load(deps.storage)?.max_commission;

        Ok(AllOptionsResponse {
            options: deps
                .querier
                .query_all_validators()?
                .into_iter()
                .filter(|v| v.commission <= max_commission)
                .map(|v| v.address)
                .collect(),
        })
    }

    pub fn check_option(deps: Deps, option: String) -> StdResult<CheckOptionResponse> {
        Ok(CheckOptionResponse {
            valid: deps.querier.query_validator(option)?.is_some(),
        })
    }

    pub fn sample_gauge_msgs(
        deps: Deps,
        new_validators: Vec<(String, Decimal)>,
    ) -> StdResult<SampleGaugeMsgsResponse> {
        let Config {
            hub,
            max_commission: _,
        } = CONFIG.load(deps.storage)?;
        Ok(SampleGaugeMsgsResponse {
            execute: vec![WasmMsg::Execute {
                contract_addr: hub.to_string(),
                msg: to_json_binary(&HubExecuteMsg::SetValidators { new_validators })?,
                funds: vec![],
            }
            .into()],
        })
    }
}

pub mod migration {
    use cosmwasm_schema::cw_serde;

    #[cw_serde]
    pub struct OldConfig {
        pub hub: String,
    }
}

/// Manages the contract migration.
#[cfg_attr(not(feature = "library"), entry_point)]
pub fn migrate(deps: DepsMut, env: Env, msg: MigrateMsg) -> Result<Response, ContractError> {
    match msg {
        MigrateMsg::Init(msg) => {
            // Enforce previous contract name was crates.io:cw-placeholder
            let ver = cw2::get_contract_version(deps.storage)?;
            if ver.contract != PLACEHOLDER_CONTRACT_NAME {
                return Err(ContractError::NotPlaceholder);
            }

            // Gather contract info to pass admin
            let contract_info = deps
                .querier
                .query_wasm_contract_info(env.contract.address.clone())?;
            let sender = contract_info.admin.unwrap();

            instantiate(
                deps,
                env,
                MessageInfo {
                    sender,
                    funds: vec![],
                },
                msg,
            )
            .unwrap();
        }
        MigrateMsg::Update { max_commission } => {
            cw2::ensure_from_older_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
        }
    }
    Ok(Response::new())
}

#[cfg(test)]
mod tests {
    use cosmwasm_std::{
        testing::{mock_dependencies, mock_env,  },
        CosmosMsg, Decimal, WasmMsg,
    };

    use super::*;

    #[test]
    fn proper_initialization() {
        let mut deps = mock_dependencies();
        let msg = InstantiateMsg {
            hub: "hub".to_string(),
            max_commission: Decimal::percent(30),
        };
        instantiate(deps.as_mut(), mock_env(), mock_info("user", &[]), msg).unwrap();

        // check if the config is stored
        let config = CONFIG.load(deps.as_ref().storage).unwrap();
        assert_eq!(config.hub, "hub");
        assert_eq!(config.max_commission, Decimal::percent(30));
    }

    #[test]
    fn invalid_max_commission() {
        let mut deps = mock_dependencies();
        let msg = InstantiateMsg {
            hub: "hub".to_string(),
            max_commission: Decimal::zero(),
        };

        let err = instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("user", &[]),
            msg.clone(),
        )
        .unwrap_err();
        assert_eq!(err, ContractError::InvalidMaxCommission {});

        let msg = InstantiateMsg {
            max_commission: Decimal::percent(101),
            ..msg
        };
        let err = instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("user", &[]),
            msg.clone(),
        )
        .unwrap_err();
        assert_eq!(err, ContractError::InvalidMaxCommission {});

        let msg = InstantiateMsg {
            max_commission: Decimal::one(),
            ..msg
        };
        instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("user", &[]),
            msg.clone(),
        )
        .unwrap();
        let msg = InstantiateMsg {
            max_commission: Decimal::percent(1),
            ..msg
        };
        instantiate(deps.as_mut(), mock_env(), mock_info("user", &[]), msg).unwrap();
    }

    #[test]
    fn basic_sample() {
        let mut deps = mock_dependencies();

        instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("user", &[]),
            InstantiateMsg {
                hub: "hub".to_string(),
                max_commission: Decimal::percent(30),
            },
        )
        .unwrap();

        let selected = vec![
            (
                "junovaloper1t8ehvswxjfn3ejzkjtntcyrqwvmvuknzmvtaaa".to_string(),
                Decimal::permille(416),
            ),
            (
                "junovaloper1ka8v934kgrw6679fs9cuu0kesyl0ljjy2kdtrl".to_string(),
                Decimal::permille(333),
            ),
            (
                "junovaloper196ax4vc0lwpxndu9dyhvca7jhxp70rmcqcnylw".to_string(),
                Decimal::permille(250),
            ),
        ];
        let res = query::sample_gauge_msgs(deps.as_ref(), selected).unwrap();
        assert_eq!(res.execute.len(), 1);
        assert_eq!(
            res.execute[0],
            CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: "hub".to_string(),
                msg: to_json_binary(&HubExecuteMsg::SetValidators {
                    new_validators: vec![
                        (
                            "junovaloper1t8ehvswxjfn3ejzkjtntcyrqwvmvuknzmvtaaa".to_string(),
                            Decimal::permille(416),
                        ),
                        (
                            "junovaloper1ka8v934kgrw6679fs9cuu0kesyl0ljjy2kdtrl".to_string(),
                            Decimal::permille(333),
                        ),
                        (
                            "junovaloper196ax4vc0lwpxndu9dyhvca7jhxp70rmcqcnylw".to_string(),
                            Decimal::permille(250),
                        ),
                    ]
                })
                .unwrap(),
                funds: vec![],
            })
        );
    }
}
