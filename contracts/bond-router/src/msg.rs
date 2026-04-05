use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::{Uint128, Uint256};

#[cw_serde]
pub struct InstantiateMsg {
    /// Address of the lsd-hub contract
    pub hub: String,
    /// Address of the staking swap pool to trade on
    pub pair: String,
}

#[cw_serde]
pub enum ExecuteMsg {
    /// Set staking Asset to bond to mint wyAsset
    Bond {},
}

#[cw_serde]
#[derive(QueryResponses)]
pub enum QueryMsg {
    #[returns(ConfigResponse)]
    Config {},
    #[returns(SimulateResponse)]
    Simulate { bond: Uint128 },
}

#[cw_serde]
pub struct ConfigResponse {
    /// Address of the lsd-hub contract
    pub hub: String,
    /// Address of the staking swap pool to trade on
    pub pair: String,
    /// This is the denomination we can stake (and only one we accept for payments)
    pub bond_denom: String,
    /// This is the lsd token the users wishes to receive
    pub lsd_token: String,
}

impl From<crate::state::Config> for ConfigResponse {
    fn from(cfg: crate::state::Config) -> Self {
        ConfigResponse {
            hub: cfg.hub.into_string(),
            pair: cfg.pair.into_string(),
            bond_denom: cfg.bond_denom,
            lsd_token: cfg.lsd_token.into_string(),
        }
    }
}

#[cw_serde]
pub struct SimulateResponse {
    pub lsd_val: Uint256,
}
