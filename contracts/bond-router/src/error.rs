use cosmwasm_std::StdError;
use cw_utils::PaymentError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),

    #[error("{0}")]
    Payment(#[from] PaymentError),

    #[error("Invalid address as lsd_hub")]
    NotLsdHub,

    #[error("Recevied unexpected reply id: {0}")]
    InvalidReplyId(u64),
}
