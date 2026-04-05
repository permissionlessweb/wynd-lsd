use cosmwasm_std::testing::{MockApi, MockQuerier, MockStorage, MOCK_CONTRACT_ADDR};
use cosmwasm_std::{
    from_json, to_json_binary, Addr, Coin, ContractInfoResponse, Empty, OwnedDeps, Querier,
    QuerierResult, QueryRequest, SystemError, SystemResult, Uint128, WasmQuery,
};
use std::collections::HashMap;

use cw20::{BalanceResponse, Cw20QueryMsg, TokenInfoResponse};

/// mock_dependencies is a drop-in replacement for cosmwasm_std::testing::mock_dependencies.
/// This uses the Wyndex CustomQuerier.
pub fn mock_dependencies(
    contract_balance: &[Coin],
) -> OwnedDeps<MockStorage, MockApi, WasmMockQuerier> {
    let custom_querier: WasmMockQuerier =
        WasmMockQuerier::new(MockQuerier::new(&[(MOCK_CONTRACT_ADDR, contract_balance)]));

    OwnedDeps {
        storage: MockStorage::default(),
        api: MockApi::default(),
        querier: custom_querier,
        custom_query_type: Default::default(),
    }
}

pub struct WasmMockQuerier {
    pub base: MockQuerier<Empty>,
    token_querier: TokenQuerier,
}

#[derive(Clone, Default)]
pub struct TokenQuerier {
    // This lets us iterate over all pairs that match the first string
    balances: HashMap<String, HashMap<String, Uint128>>,
}

impl TokenQuerier {}

impl Querier for WasmMockQuerier {
    fn raw_query(&self, bin_request: &[u8]) -> QuerierResult {
        // MockQuerier doesn't support Custom, so we ignore it completely
        let request: QueryRequest<Empty> = match from_json(bin_request) {
            Ok(v) => v,
            Err(e) => {
                return SystemResult::Err(SystemError::InvalidRequest {
                    error: format!("Parsing query request: {}", e),
                    request: bin_request.into(),
                })
            }
        };
        self.handle_query(&request)
    }
}

impl WasmMockQuerier {
    pub fn handle_query(&self, request: &QueryRequest<Empty>) -> QuerierResult {
        match &request {
            QueryRequest::Wasm(WasmQuery::ContractInfo { contract_addr }) => {
                if contract_addr == "cosmos2contract" {
                    let contract_info = ContractInfoResponse::new(
                        1,
                        MockApi::default().addr_make("admin"),
                        Some(MockApi::default().addr_make("admin")),
                        false,
                        None,
                        None,
                    );
                    SystemResult::Ok(to_json_binary(&contract_info).into())
                } else {
                    panic!("DO NOT ENTER HERE");
                }
            }
            QueryRequest::Wasm(WasmQuery::Smart { contract_addr, msg }) => {
                match from_json(msg).unwrap() {
                    Cw20QueryMsg::TokenInfo {} => {
                        let balances: &HashMap<String, Uint128> =
                            match self.token_querier.balances.get(contract_addr) {
                                Some(balances) => balances,
                                None => {
                                    return SystemResult::Err(SystemError::Unknown {});
                                }
                            };

                        let mut total_supply = Uint128::zero();

                        for balance in balances {
                            total_supply += *balance.1;
                        }

                        SystemResult::Ok(
                            to_json_binary(&TokenInfoResponse {
                                name: "mAPPL".to_string(),
                                symbol: "mAPPL".to_string(),
                                decimals: 6,
                                total_supply: total_supply.into(),
                            })
                            .into(),
                        )
                    }
                    Cw20QueryMsg::Balance { address } => {
                        let balances: &HashMap<String, Uint128> =
                            match self.token_querier.balances.get(contract_addr) {
                                Some(balances) => balances,
                                None => {
                                    return SystemResult::Err(SystemError::Unknown {});
                                }
                            };

                        let balance = match balances.get(&address) {
                            Some(v) => v,
                            None => {
                                return SystemResult::Err(SystemError::Unknown {});
                            }
                        };

                        SystemResult::Ok(
                            to_json_binary(&BalanceResponse {
                                balance: (*balance).into(),
                            })
                            .into(),
                        )
                    }
                    _ => panic!("DO NOT ENTER HERE"),
                }
            }
            QueryRequest::Wasm(WasmQuery::Raw { contract_addr, .. }) => {
                if contract_addr == "factory" {
                    SystemResult::Ok(to_json_binary(&Vec::<Addr>::new()).into())
                } else {
                    panic!("DO NOT ENTER HERE");
                }
            }
            _ => self.base.handle_query(request),
        }
    }
}

impl WasmMockQuerier {
    pub fn new(base: MockQuerier<Empty>) -> Self {
        WasmMockQuerier {
            base,
            token_querier: TokenQuerier::default(),
        }
    }
}
