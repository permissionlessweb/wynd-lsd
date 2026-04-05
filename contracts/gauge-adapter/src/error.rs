use cosmwasm_std::StdError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("Can only init upgrade from cw-placeholder")]
    NotPlaceholder,

    #[error("Invalid max_commission; must be higher then 0.0 and smaller or equal then 1.0")]
    InvalidMaxCommission {},
}
