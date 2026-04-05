use anyhow::Result as AnyResult;

use cosmwasm_std::{
    coin, coins,
    testing::{mock_env, MockApi},
    to_json_binary, Addr, Coin, Decimal, StdResult, Uint256, Validator,
};
use cw20::{BalanceResponse, Cw20Coin, Cw20ExecuteMsg, Cw20QueryMsg};
use cw_multi_test::{App, AppResponse, ContractWrapper, Executor, StakingInfo};

use wynd_lsd_hub::msg::{
    ConfigResponse as LsdHubConfigResponse, ExchangeRateResponse,
    InstantiateMsg as HubInstantiateMsg, QueryMsg as LsdHubQueryMsg, Supply, SupplyResponse,
    TokenInitInfo,
};
use wyndex::{
    asset::{Asset, AssetInfo},
    factory::{
        DefaultStakeConfig, ExecuteMsg as FactoryExecuteMsg,
        InstantiateMsg as FactoryInstantiateMsg, PairConfig, PairType, QueryMsg as FactoryQueryMsg,
    },
    fee_config::FeeConfig,
    pair::{ExecuteMsg as PairExecuteMsg, QueryMsg as PairQueryMsg},
    pair::{LsdInfo, PairInfo, SpotPriceResponse, StablePoolParams},
};

use crate::msg::{ExecuteMsg, InstantiateMsg, QueryMsg, SimulateResponse};

pub const NATIVE: &str = "ujuno";

fn store_bond_router(app: &mut App) -> u64 {
    let contract = Box::new(
        ContractWrapper::new_with_empty(
            crate::contract::execute,
            crate::contract::instantiate,
            crate::contract::query,
        )
        .with_reply_empty(crate::contract::reply),
    );
    app.store_code(contract)
}

fn store_lsd_hub(app: &mut App) -> u64 {
    let contract = Box::new(
        ContractWrapper::new_with_empty(
            wynd_lsd_hub::contract::execute,
            wynd_lsd_hub::contract::instantiate,
            wynd_lsd_hub::contract::query,
        )
        .with_reply_empty(wynd_lsd_hub::contract::reply),
    );
    app.store_code(contract)
}

fn store_cw20(app: &mut App) -> u64 {
    let contract = Box::new(ContractWrapper::new(
        cw20_base::contract::execute,
        cw20_base::contract::instantiate,
        cw20_base::contract::query,
    ));
    app.store_code(contract)
}

fn store_factory(app: &mut App) -> u64 {
    let contract = Box::new(
        ContractWrapper::new_with_empty(
            wyndex_factory::contract::execute,
            wyndex_factory::contract::instantiate,
            wyndex_factory::contract::query,
        )
        .with_reply_empty(wyndex_factory::contract::reply),
    );
    app.store_code(contract)
}

fn store_lsd_pair(app: &mut App) -> u64 {
    let contract = Box::new(
        ContractWrapper::new(
            wyndex_pair_lsd::contract::execute,
            wyndex_pair_lsd::contract::instantiate,
            wyndex_pair_lsd::contract::query,
        )
        .with_reply_empty(wyndex_pair_lsd::contract::reply),
    );
    app.store_code(contract)
}

fn store_staking(app: &mut App) -> u64 {
    let contract = Box::new(ContractWrapper::new(
        wyndex_stake::contract::execute,
        wyndex_stake::contract::instantiate,
        wyndex_stake::contract::query,
    ));
    app.store_code(contract)
}

#[derive(Debug)]
pub struct SuiteBuilder {
    funds: Vec<(Addr, Vec<Coin>)>,
    lsd_funds: Vec<Cw20Coin>,
}

impl SuiteBuilder {
    pub fn new() -> Self {
        Self {
            funds: vec![],
            lsd_funds: vec![],
        }
    }

    pub fn with_funds(mut self, addr: &Addr, funds: (u128, &str)) -> Self {
        self.funds.push((addr.clone(), coins(funds.0, funds.1)));
        self
    }

    pub fn with_lsd_funds(mut self, addr: &Addr, amount: u128) -> Self {
        self.lsd_funds.push(Cw20Coin {
            address: addr.into(),
            amount: amount.into(),
        });
        self
    }

    #[track_caller]
    pub fn build(self) -> Suite {
        let mut app = App::default();
        let owner = MockApi::default().addr_make("owner");

        let funds = self.funds;
        app.init_modules(|router, api, storage| -> StdResult<()> {
            router.staking.setup(
                storage,
                StakingInfo {
                    bonded_denom: NATIVE.to_string(),
                    ..Default::default()
                },
            )?;
            router.staking.add_validator(
                api,
                storage,
                &mock_env().block,
                Validator::create(
                    MockApi::default()
                        .addr_make("junovaloper196ax4vc0lwpxndu9dyhvca7jhxp70rmcqcnylw")
                        .to_string(),
                    Decimal::percent(5),
                    Decimal::one(),
                    Decimal::one(),
                ),
            )?;

            for (addr, coin) in funds {
                router.bank.init_balance(storage, &addr, coin)?;
            }
            Ok(())
        })
        .unwrap();

        let cw20_code_id = store_cw20(&mut app);

        let factory_code_id = store_factory(&mut app);
        let lsd_pair_code_id = store_lsd_pair(&mut app);
        let staking_code_id = store_staking(&mut app);
        let factory = app
            .instantiate_contract(
                factory_code_id,
                owner.clone(),
                &FactoryInstantiateMsg {
                    pair_configs: vec![PairConfig {
                        code_id: lsd_pair_code_id,
                        pair_type: PairType::Lsd {},
                        fee_config: FeeConfig {
                            total_fee_bps: 4,
                            protocol_fee_bps: 5000,
                        },
                        is_disabled: false,
                    }],
                    token_code_id: cw20_code_id,
                    fee_address: None,
                    owner: owner.to_string(),
                    max_referral_commission: Decimal::percent(10).into(),
                    default_stake_config: DefaultStakeConfig {
                        staking_code_id,
                        tokens_per_power: Uint256::one(),
                        min_bond: Uint256::one(),
                        unbonding_periods: vec![3600],
                        max_distributions: 1,
                        converter: None,
                    },
                    trading_starts: None,
                },
                &[],
                "Wyndex Factory",
                None,
            )
            .unwrap();

        let lsd_hub_code_id = store_lsd_hub(&mut app);
        let lsd_funds = self.lsd_funds;
        let lsd_hub = app
            .instantiate_contract(
                lsd_hub_code_id,
                owner.clone(),
                &HubInstantiateMsg {
                    treasury: MockApi::default().addr_make("treasury").to_string(),
                    owner: owner.to_string(),
                    commission: Decimal::percent(9),
                    validators: vec![(
                        MockApi::default()
                            .addr_make("junovaloper196ax4vc0lwpxndu9dyhvca7jhxp70rmcqcnylw")
                            .to_string(),
                        Decimal::one(),
                    )],
                    cw20_init: TokenInitInfo {
                        cw20_code_id,
                        label: "wyJUNO".to_string(),
                        name: "Staked Juno".to_string(),
                        symbol: "WYJUNO".to_string(),
                        decimals: 6,
                        initial_balances: lsd_funds,
                        marketing: None,
                    },
                    epoch_period: 21 * 24 * 3600,
                    unbond_period: 21 * 24 * 3600,
                    max_concurrent_unbondings: 7,
                    liquidity_discount: Decimal::percent(3),
                    slashing_safety_margin: 10,
                    tombstone_treshold: Decimal::percent(10),
                },
                &[],
                "Wyndex LSD Hub",
                Some(owner.to_string()),
            )
            .unwrap();

        // get wyJUNO token address
        let lsd_token = app
            .wrap()
            .query_wasm_smart::<LsdHubConfigResponse>(lsd_hub.clone(), &LsdHubQueryMsg::Config {})
            .unwrap()
            .token_contract;

        // instantiate LSD pool
        app.execute_contract(
            owner.clone(),
            factory.clone(),
            &FactoryExecuteMsg::CreatePair {
                pair_type: PairType::Lsd {},
                asset_infos: vec![
                    AssetInfo::Token(lsd_token.to_string()),
                    AssetInfo::Native(NATIVE.to_owned()),
                ],
                init_params: Some(
                    to_json_binary(&StablePoolParams {
                        amp: 23,
                        owner: Some(owner.to_string()),
                        lsd: Some(LsdInfo {
                            asset: AssetInfo::Token(lsd_token.to_string()),
                            hub: lsd_hub.to_string(),
                            target_rate_epoch: 86400,
                        }),
                    })
                    .unwrap(),
                ),
                staking_config: Default::default(),
                total_fee_bps: None,
            },
            &[],
        )
        .unwrap();
        let lsd_pool = app
            .wrap()
            .query_wasm_smart::<PairInfo>(
                factory,
                &FactoryQueryMsg::Pair {
                    asset_infos: vec![
                        AssetInfo::Token(lsd_token.to_string()),
                        AssetInfo::Native(NATIVE.to_owned()),
                    ],
                },
            )
            .unwrap()
            .contract_addr;

        let bond_router_code_id = store_bond_router(&mut app);
        let bond_router = app
            .instantiate_contract(
                bond_router_code_id,
                owner.clone(),
                &InstantiateMsg {
                    hub: lsd_hub.to_string(),
                    pair: lsd_pool.to_string(),
                },
                &[],
                "Bond router",
                Some(owner.to_string()),
            )
            .unwrap();

        Suite {
            owner,
            app,
            lsd_hub,
            lsd_pool,
            lsd_token,
            bond_router,
        }
    }
}

pub struct Suite {
    pub owner: Addr,
    pub app: App,
    pub lsd_hub: Addr,
    pub lsd_pool: Addr,
    pub lsd_token: Addr,
    pub bond_router: Addr,
}

impl Suite {
    #[allow(unused)]
    pub fn next_block(&mut self, time: u64) {
        self.app.update_block(|block| {
            block.time = block.time.plus_seconds(time);
            block.height += 1
        });
    }

    pub fn bond(&mut self, sender: &Addr, funds: (u128, &str)) -> StdResult<AppResponse> {
        self.app.execute_contract(
            sender.clone(),
            self.bond_router.clone(),
            &ExecuteMsg::Bond {},
            &[coin(funds.0, funds.1)],
        )
    }

    pub fn increase_allowance(
        &mut self,
        owner: &Addr,
        contract: &Addr,
        spender: &Addr,
        amount: u128,
    ) -> StdResult<AppResponse> {
        self.app.execute_contract(
            owner.clone(),
            contract.clone(),
            &Cw20ExecuteMsg::IncreaseAllowance {
                spender: spender.to_string(),
                amount: amount.into(),
                expires: None,
            },
            &[],
        )
    }

    pub fn provide_liquidity(
        &mut self,
        owner: &Addr,
        pair: &Addr,
        assets: &[Asset],
        send_funds: &[Coin],
    ) -> StdResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(owner),
            pair.clone(),
            &PairExecuteMsg::ProvideLiquidity {
                assets: assets.to_vec(),
                slippage_tolerance: None,
                receiver: None,
            },
            send_funds,
        )
    }

    // simulate bond tx query in bond router contract
    pub fn query_simulate(&self, bond: u128) -> StdResult<Uint256> {
        Ok(self
            .app
            .wrap()
            .query_wasm_smart::<SimulateResponse>(
                self.bond_router.clone(),
                &QueryMsg::Simulate { bond: bond.into() },
            )?
            .lsd_val)
    }

    pub fn query_exchange_rate(&self) -> StdResult<Decimal> {
        Ok(self
            .app
            .wrap()
            .query_wasm_smart::<ExchangeRateResponse>(
                self.lsd_hub.clone(),
                &LsdHubQueryMsg::ExchangeRate {},
            )?
            .exchange_rate)
    }

    pub fn query_lsd_supply(&self) -> StdResult<Supply> {
        Ok(self
            .app
            .wrap()
            .query_wasm_smart::<SupplyResponse>(self.lsd_hub.clone(), &LsdHubQueryMsg::Supply {})?
            .supply)
    }

    pub fn query_spot_price(&self) -> StdResult<cosmwasm_std::Decimal256> {
        Ok(self
            .app
            .wrap()
            .query_wasm_smart::<SpotPriceResponse>(
                self.lsd_pool.clone(),
                &PairQueryMsg::SpotPrice {
                    offer: AssetInfo::Native(NATIVE.to_string()),
                    ask: AssetInfo::Token(self.lsd_token.to_string()),
                },
            )?
            .price)
    }

    pub fn query_cw20_balance(&self, sender: &Addr, address: &Addr) -> StdResult<Uint256> {
        Ok(self
            .app
            .wrap()
            .query_wasm_smart::<BalanceResponse>(
                address,
                &Cw20QueryMsg::Balance {
                    address: sender.to_string(),
                },
            )?
            .balance
            .into())
    }
}
