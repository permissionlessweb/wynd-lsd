use std::{collections::HashMap, str::FromStr};

use crate::{state::SUPPLY, ContractError};

use super::suite::SuiteBuilder;

use crate::state::{unbonding_info_num_epochs, unbonding_info_total_entries, BONDED};
use cosmwasm_std::testing::MockApi;
use cosmwasm_std::{assert_approx_eq, coin, Decimal, Delegation, Uint128, Uint256};

const DAY: u64 = 24 * HOUR;
const HOUR: u64 = 60 * 60;

#[test]
fn proper_init() {
    let delegators = &[
        &MockApi::default().addr_make("delegator1"),
        &MockApi::default().addr_make("delegator2"),
    ];

    let suite = SuiteBuilder::new()
        .with_initial_balances(vec![
            (delegators[0], 2_000_000u128),
            (delegators[1], 2_000_000u128),
        ])
        .with_periods(DAY, 28 * DAY)
        .build();
    let lsd_token = suite.query_lsd_token().unwrap();
    // Verify the admin of the hub is the same as the LP token
    let admin_hub = suite.query_contract_admin(&suite.hub).unwrap();
    let admin_lp = suite.query_contract_admin(&lsd_token).unwrap();
    assert_eq!(admin_lp, admin_hub);
}

#[test]
fn basic_minting_case() {
    let delegators = &[
        &MockApi::default().addr_make("delegator1"),
        &MockApi::default().addr_make("delegator2"),
    ];

    let initial_staking_amount = 1_000_000u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![
            (delegators[0], 2_000_000u128),
            (delegators[1], 2_000_000u128),
        ])
        .with_periods(DAY, 28 * DAY)
        .build();

    // Simulate deposits for some of the users of all their stake
    suite.bond(delegators[0], initial_staking_amount).unwrap();
    suite.bond(delegators[1], initial_staking_amount).unwrap();

    let lsd = suite.query_lsd_token().unwrap();

    // Users now have the same amount of LSD tokens as they staked
    let balance = suite.query_cw20_balance(delegators[0], &lsd).unwrap();
    assert_eq!(balance, Uint256::new(initial_staking_amount));
    let balance = suite.query_cw20_balance(delegators[1], &lsd).unwrap();
    assert_eq!(balance, Uint256::new(initial_staking_amount));

    // wait until next epoch and do first reinvest to actually delegate
    suite.update_time(DAY);
    suite.reinvest().unwrap();

    // bonded amount should be staked to the validator now
    let delegations = suite.query_delegations().unwrap();
    assert_eq!(delegations.len(), 1);
    assert_eq!(
        delegations[0].amount.amount,
        Uint256::new(initial_staking_amount * 2)
    );

    assert_eq!(suite.query_tvl().unwrap().u128(), 2_000_000u128);
}

#[test]
fn change_valset_dust() {
    let delegator = &MockApi::default().addr_make("delegator");

    let amount = 3_333_333u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegator, amount)])
        .with_validators(vec![
            (
                &MockApi::default().addr_make("testvaloper1").into(),
                Decimal::percent(50),
            ),
            (
                &MockApi::default().addr_make("testvaloper2").into(),
                Decimal::percent(50),
            ),
        ])
        .with_registered_validators(vec![MockApi::default()
            .addr_make("testvaloper3")
            .to_string()])
        .with_periods(23 * HOUR, 28 * DAY)
        .build();

    // deposit some tokens to stake
    suite.bond(delegator, amount).unwrap();

    // wait until next epoch and do first reinvest to actually delegate
    suite.update_time(23 * HOUR);
    suite.reinvest().unwrap();

    // first validator gets the dust
    let delegations = suite.query_delegations().unwrap();
    assert_eq!(delegations[0].amount.amount, Uint256::new(1_666_667));
    assert_eq!(delegations[1].amount.amount, Uint256::new(1_666_666));

    // get the saved balances
    let supply = SUPPLY.query(&suite.app.wrap(), suite.hub.clone()).unwrap();
    assert_eq!(supply.total_bonded.u128(), amount);
    let bonded = BONDED.query(&suite.app.wrap(), suite.hub.clone()).unwrap();
    assert_eq!(
        HashMap::from([
            (
                MockApi::default().addr_make("testvaloper1").to_string(),
                1_666_667u128.into()
            ),
            (
                MockApi::default().addr_make("testvaloper2").to_string(),
                1_666_666u128.into()
            ),
        ]),
        bonded.into_iter().collect()
    );

    // wait until we can change the validator set
    suite.update_time(27 * DAY + HOUR);
    suite
        .set_validators(
            &MockApi::default().addr_make("owner"),
            vec![
                (
                    MockApi::default().addr_make("testvaloper2").to_string(),
                    Decimal::percent(50),
                ),
                (
                    MockApi::default().addr_make("testvaloper3").to_string(),
                    Decimal::percent(50),
                ),
            ],
        )
        .unwrap();

    // first validator still has some dust left
    let delegations = suite.query_delegations().unwrap();
    assert_eq!(delegations[0].amount.amount, Uint256::new(1));
    assert_eq!(delegations[1].amount.amount, Uint256::new(1_666_666));
    assert_eq!(delegations[2].amount.amount, Uint256::new(1_666_666));

    // get the saved balances
    let supply = SUPPLY.query(&suite.app.wrap(), suite.hub.clone()).unwrap();
    assert_eq!(supply.total_bonded.u128(), amount);
    let bonded = BONDED.query(&suite.app.wrap(), suite.hub.clone()).unwrap();
    assert_eq!(
        HashMap::from([
            (
                MockApi::default().addr_make("testvaloper1").to_string(),
                1u128.into()
            ),
            (
                MockApi::default().addr_make("testvaloper2").to_string(),
                1_666_666u128.into()
            ),
            (
                MockApi::default().addr_make("testvaloper3").to_string(),
                1_666_666u128.into()
            ),
        ]),
        bonded.into_iter().collect()
    );
}

#[test]
fn set_new_valset_more_validators() {
    let delegator = &MockApi::default().addr_make("delegator1");

    let staking_amount = 1_000_000u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegator, 1_000_000)])
        .with_validators(vec![
            (
                &MockApi::default().addr_make("testvaloper1").into(),
                Decimal::percent(50),
            ),
            (
                &MockApi::default().addr_make("testvaloper2").into(),
                Decimal::percent(50),
            ),
        ])
        .with_registered_validators(vec![
            MockApi::default().addr_make("testvaloper3").into(),
            MockApi::default().addr_make("testvaloper4").into(),
        ])
        .with_periods(DAY, 28 * DAY)
        .build();

    suite.bond(delegator, staking_amount).unwrap();
    suite.update_time(DAY);
    suite.reinvest().unwrap();

    // check example delegation if it matches the percentages
    let delegations = suite.query_delegations().unwrap();
    assert_eq!(delegations.len(), 2);

    let Delegation {
        delegator: _,
        validator,
        amount,
        ..
    } = delegations[0].clone();
    assert_eq!(validator, MockApi::default().addr_make("testvaloper1").to_string());
    assert_eq!(amount, coin(500_000, "FUN"));

    suite
        .set_validators(
            &MockApi::default().addr_make("owner"),
            vec![
                (
                    MockApi::default().addr_make("testvaloper1").to_string(),
                    Decimal::percent(25),
                ),
                (
                    MockApi::default().addr_make("testvaloper2").to_string(),
                    Decimal::percent(25),
                ),
                (
                    MockApi::default().addr_make("testvaloper3").to_string(),
                    Decimal::percent(25),
                ),
                (
                    MockApi::default().addr_make("testvaloper4").to_string(),
                    Decimal::percent(25),
                ),
            ],
        )
        .unwrap();

    let delegations = suite.query_delegations().unwrap();
    assert_eq!(delegations.len(), 4);

    let Delegation {
        delegator: _,
        validator,
        amount,
        ..
    } = delegations[0].clone();
    assert_eq!(validator, MockApi::default().addr_make("testvaloper1").to_string());
    assert_eq!(amount, coin(250_000, "FUN"));

    suite.update_time(DAY);
    suite.reinvest().unwrap();

    let delegations = suite.query_delegations().unwrap();
    assert_eq!(delegations.len(), 4);

    let Delegation {
        delegator: _,
        validator,
        amount,
        ..
    } = delegations[0].clone();
    assert_eq!(validator, MockApi::default().addr_make("testvaloper1").to_string());
    assert_eq!(amount, coin(250_494, "FUN"));

    let valset = suite.query_validator_set().unwrap();
    assert_eq!(
        valset,
        vec![
            (
                MockApi::default().addr_make("testvaloper1").to_string(),
                Decimal::percent(25)
            ),
            (
                MockApi::default().addr_make("testvaloper2").to_string(),
                Decimal::percent(25)
            ),
            (
                MockApi::default().addr_make("testvaloper3").to_string(),
                Decimal::percent(25)
            ),
            (
                MockApi::default().addr_make("testvaloper4").to_string(),
                Decimal::percent(25)
            ),
        ]
    );
}

#[test]
fn set_new_valset_less_validators() {
    let delegator = &MockApi::default().addr_make("delegator1");

    let staking_amount = 1_000_000u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegator, 1_000_000)])
        .with_validators(vec![
            (
                &MockApi::default().addr_make("testvaloper1").into(),
                Decimal::percent(33),
            ),
            (
                &MockApi::default().addr_make("testvaloper2").into(),
                Decimal::percent(33),
            ),
            (
                &MockApi::default().addr_make("testvaloper3").into(),
                Decimal::percent(34),
            ),
        ])
        .with_periods(DAY, 28 * DAY)
        .build();

    suite.bond(delegator, staking_amount).unwrap();
    suite.update_time(DAY);
    suite.reinvest().unwrap();

    // check example delegation if it matches the percentages
    let delegations = suite.query_delegations().unwrap();
    assert_eq!(delegations.len(), 3);

    let Delegation {
        delegator: _,
        validator,
        amount,
        ..
    } = delegations[0].clone();
    assert_eq!(validator, MockApi::default().addr_make("testvaloper1").to_string());
    assert_eq!(amount, coin(330_000, "FUN"));

    suite
        .set_validators(
            &MockApi::default().addr_make("owner"),
            vec![
                (
                    MockApi::default().addr_make("testvaloper2").to_string(),
                    Decimal::percent(50),
                ),
                (
                    MockApi::default().addr_make("testvaloper3").to_string(),
                    Decimal::percent(50),
                ),
            ],
        )
        .unwrap();

    let delegations = suite.query_delegations().unwrap();
    assert_eq!(delegations.len(), 2);

    let Delegation {
        delegator: _,
        validator,
        amount,
        ..
    } = delegations[0].clone();
    assert_eq!(validator, MockApi::default().addr_make("testvaloper2").to_string());
    assert_eq!(amount, coin(500_000, "FUN"));

    suite.update_time(DAY);
    suite.reinvest().unwrap();

    let valset = suite.query_validator_set().unwrap();
    assert_eq!(
        valset,
        vec![
            (
                MockApi::default().addr_make("testvaloper2").to_string(),
                Decimal::percent(50)
            ),
            (
                MockApi::default().addr_make("testvaloper3").to_string(),
                Decimal::percent(50)
            ),
        ]
    );

    let delegations = suite.query_delegations().unwrap();
    assert_eq!(delegations.len(), 2);

    let delegation = delegations[0].clone();
    assert_eq!(delegation.validator, MockApi::default().addr_make("testvaloper2").to_string());
    assert_eq!(delegation.amount, coin(500_989, "FUN"));
}

#[test]
fn deposit_reinvest() {
    let delegators = &[
        &MockApi::default().addr_make("delegator1"),
        &MockApi::default().addr_make("delegator2"),
    ];

    let initial_staking_amount = 1_000_000u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegators[0], 2_000_000), (delegators[1], 2_000_000)])
        .with_validators(vec![
            (
                &MockApi::default().addr_make("testvaloper1").into(),
                Decimal::percent(50),
            ),
            (
                &MockApi::default().addr_make("testvaloper2").into(),
                Decimal::percent(30),
            ),
            (
                &MockApi::default().addr_make("testvaloper3").into(),
                Decimal::percent(20),
            ),
        ])
        .with_periods(DAY, 28 * DAY)
        .build();

    let exchange_rate = suite.query_exchange_rate().unwrap();
    assert_eq!(
        exchange_rate,
        Decimal::one(),
        "exchange rate should start at 1:1"
    );

    suite.bond(delegators[0], initial_staking_amount).unwrap();

    let exchange_rate = suite.query_exchange_rate().unwrap();
    assert_eq!(
        exchange_rate,
        Decimal::one(),
        "exchange rate should stay the same after deposit"
    );

    // wait until next epoch and do first reinvest to actually delegate and start accumulating rewards
    suite.update_time(DAY);
    suite.reinvest().unwrap();

    // wait until rewards pile up
    suite.update_time(DAY);
    // reinvest again to claim rewards and update exchange rate
    suite.reinvest().unwrap();

    let exchange_rate = suite.query_exchange_rate().unwrap();
    assert!(
        exchange_rate > Decimal::one(),
        "exchange rate should have increased because of rewards"
    );

    let delegations = suite.query_delegations().unwrap();
    assert_eq!(delegations.len(), 3);
    let delegations: Uint256 = delegations.into_iter().map(|d| d.amount.amount).sum();
    assert!(
        delegations > initial_staking_amount.into(),
        "should be more because of rewards"
    );

    // other user deposits
    suite.bond(delegators[1], initial_staking_amount).unwrap();

    // should get less lsd tokens than the first user, since lsd tokens are worth more now
    let lsd = suite.query_lsd_token().unwrap();
    let balance = suite.query_cw20_balance(delegators[1], &lsd).unwrap();
    assert!(balance < 1_000_000u128.into());

    // wait and reinvest to delegate them
    suite.update_time(DAY);
    suite.reinvest().unwrap();

    // bonded amount should be staked to the validator now
    let delegations = suite.query_delegations().unwrap();
    assert_eq!(delegations.len(), 3);
    let delegations: Uint256 = delegations.into_iter().map(|d| d.amount.amount).sum();
    assert!(
        delegations > (initial_staking_amount * 2).into(),
        "should be more because of rewards"
    );
}

#[test]
fn target_value() {
    let delegators = &[
        &MockApi::default().addr_make("delegator1"),
        &MockApi::default().addr_make("delegator2"),
    ];

    let amount = 1_000_000u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegators[0], 2_000_000), (delegators[1], 2_000_000)])
        .with_liquidity_discount(Decimal::percent(6))
        .with_validators(vec![
            (
                &MockApi::default().addr_make("testvaloper1").into(),
                Decimal::percent(50),
            ),
            (
                &MockApi::default().addr_make("testvaloper2").into(),
                Decimal::percent(30),
            ),
            (
                &MockApi::default().addr_make("testvaloper3").into(),
                Decimal::percent(20),
            ),
        ])
        .with_periods(DAY, 28 * DAY)
        .build();

    let target_value = suite.query_target_value().unwrap();
    assert_eq!(target_value, Decimal::percent(94), "100% - discount = 94%");

    suite.bond(delegators[0], amount).unwrap();

    let target_value = suite.query_target_value().unwrap();
    assert_eq!(
        target_value,
        Decimal::percent(94),
        "should stay the same after deposit"
    );

    // wait until next epoch and do first reinvest to actually delegate and start accumulating rewards
    suite.update_time(DAY);
    suite.reinvest().unwrap();

    let target_value = suite.query_target_value().unwrap();
    assert_eq!(
        target_value,
        Decimal::percent(94),
        "should still be the same"
    );

    // change liquidity discount
    let err = suite
        .update_liquidity_discount(
            &MockApi::default().addr_make("notadmin"),
            Decimal::percent(5),
        )
        .unwrap_err();
    assert!(
        err.to_string()
            .contains(&ContractError::Unauthorized {}.to_string()),
        "expected Unauthorized error, got: {}",
        err
    );
    let err = suite
        .update_liquidity_discount(
            &MockApi::default().addr_make("owner"),
            Decimal::percent(100),
        )
        .unwrap_err();
    assert!(
        err.to_string()
            .contains(&ContractError::InvalidLiquidityDiscount {}.to_string()),
        "expected InvalidLiquidityDiscount error, got: {}",
        err
    );
    suite
        .update_liquidity_discount(&MockApi::default().addr_make("owner"), Decimal::percent(10))
        .unwrap();

    let target_value = suite.query_target_value().unwrap();
    assert_eq!(target_value, Decimal::percent(90), "100% - discount = 90%");

    // wait for rewards
    suite.update_time(DAY);
    // reinvest again to claim rewards and update exchange rate
    suite.reinvest().unwrap();

    let target_value = suite.query_target_value().unwrap();
    assert!(
        target_value > Decimal::percent(90),
        "should have increased because of rewards"
    );

    // other user deposits
    suite.bond(delegators[1], amount).unwrap();

    let target_value2 = suite.query_target_value().unwrap();
    assert_approx_eq!(
        target_value2.atomics(),
        target_value.atomics(),
        "0.0000005",
        "deposits should not affect target value"
    );

    // wait and reinvest to delegate them
    suite.update_time(DAY);
    suite.reinvest().unwrap();

    let target_value3 = suite.query_target_value().unwrap();
    assert!(
        target_value3 > target_value2,
        "should have increased because of rewards"
    );

    // first user unbonds
    suite
        .unbond(delegators[0], &suite.query_lsd_token().unwrap(), amount)
        .unwrap();

    let target_value4 = suite.query_target_value().unwrap();
    assert_approx_eq!(
        target_value4.atomics(),
        target_value3.atomics(),
        "0.000001",
        "unbonding should not affect target value"
    );

    let exchange_rate = suite.query_exchange_rate().unwrap();
    assert_eq!(exchange_rate * Decimal::percent(90), target_value4);
}

#[test]
fn commission() {
    let delegator = &MockApi::default().addr_make("delegator");
    let amount = 1_000_000u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegator, 2 * amount)])
        .with_validators(vec![
            (
                &MockApi::default().addr_make("testvaloper1").into(),
                Decimal::percent(10),
            ),
            (
                &MockApi::default().addr_make("testvaloper2").into(),
                Decimal::percent(20),
            ),
            (
                &MockApi::default().addr_make("testvaloper3").into(),
                Decimal::percent(70),
            ),
        ])
        .with_periods(DAY, 28 * DAY)
        .build();

    suite.bond(delegator, amount).unwrap();

    // reinvest to trigger delegation
    suite.update_time(DAY);
    suite.reinvest().unwrap();

    // bond some more just to make sure we don't get commission for it
    // this is not delegated yet, so we don't get rewards for it
    suite.bond(delegator, amount).unwrap();

    assert_eq!(
        suite
            .query_balance(&MockApi::default().addr_make("treasury"), "FUN")
            .unwrap(),
        Uint256::zero(),
        "should not have gotten commission yet"
    );

    // wait until rewards pile up
    suite.update_time(DAY);
    // reinvest again to claim rewards and get commission
    suite.reinvest().unwrap();

    // APR is 80% per year, validator takes 5% commission
    // 80% * 1_000_000 / 365 * 95% = 2081 rewards
    // 2081 * 5% = 104 commission
    assert_eq!(
        suite
            .query_balance(&MockApi::default().addr_make("treasury"), "FUN")
            .unwrap(),
       Uint256::new( 104u128 )
    );
}

#[test]
fn tiny_weights() {
    let delegator = &MockApi::default().addr_make("delegator");
    let initial_staking_amount = 1_000_000u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegator, 1_000_000)])
        .with_validators(vec![
            (
                &MockApi::default().addr_make("testvaloper1").into(),
                Decimal::from_str("0.000001").unwrap(),
            ),
            (
                &MockApi::default().addr_make("testvaloper2").into(),
                Decimal::from_str("0.00000001").unwrap(),
            ),
            (
                &MockApi::default().addr_make("testvaloper3").into(),
                Decimal::from_str("0.99999899").unwrap(),
            ),
        ])
        .with_periods(DAY, 28 * DAY)
        .build();

    suite.bond(delegator, initial_staking_amount).unwrap();

    suite.update_time(DAY);
    suite.reinvest().unwrap();

    let delegations = suite.query_delegations().unwrap();
    assert_eq!(
        delegations.len(),
        2,
        "only two because one validator gets 0 stake"
    );
    assert_eq!(
        delegations[0].amount.amount,
        Uint256::new(2),
        "should be 2 because of rounding"
    );
    assert_eq!(delegations[1].amount.amount, Uint256::new(999998));
}

#[test]
fn simple_bond_unbond_claim() {
    let delegators = &[
        &MockApi::default().addr_make("delegator1"),
        &MockApi::default().addr_make("delegator2"),
    ];

    let initial_staking_amount = 1_000_000u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![
            (delegators[0], 2_000_000u128),
            (delegators[1], 2_000_000u128),
        ])
        .build();

    // Simulate deposits for some of the users of all their stake
    suite.bond(delegators[0], initial_staking_amount).unwrap();
    suite.bond(delegators[1], initial_staking_amount).unwrap();

    let lsd = suite.query_lsd_token().unwrap();

    // Users now have the same amount of LSD tokens as they staked
    let lsd_token_balance = suite.query_cw20_balance(delegators[0], &lsd).unwrap();
    assert_eq!(
        lsd_token_balance,
        Uint256::new(initial_staking_amount)
    );

    let native_token_balance = suite.query_balance(delegators[0], "FUN").unwrap();

    // Unbond all of delegator[0]'s previously bonded tokens
    suite
        .unbond(delegators[0], &lsd, lsd_token_balance)
        .unwrap();

    // Query claims to verify one was made
    let claims = suite.query_claims(delegators[0].to_string()).unwrap();
    assert_eq!(claims.len(), 1);

    // Advance time enough to make the unbonding period pass
    // Next time unbonding happens is one epoch from now, so 23 hours from now
    suite.update_time(23 * HOUR + 28 * DAY);

    // Submit a claim
    suite.claim(delegators[0]).unwrap();
    // Verify delegator[0] has their native tokens back
    // Using assert_approx_eq! because the claim amount will not be exact when we implement any sort of exit tax or the exchange rate is off
    assert_approx_eq!(
        Uint128::try_from(suite.query_balance(delegators[0], "FUN").unwrap()).unwrap(),
        Uint128::try_from(native_token_balance + lsd_token_balance).unwrap(),
        "0.000000000001"
    );
}

#[test]
fn bond_unbond() {
    let delegator = &MockApi::default().addr_make("delegator");

    let amount = 1_000_003u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegator, amount)])
        .with_validators(vec![
            (
                &MockApi::default().addr_make("testvaloper1").into(),
                Decimal::percent(50),
            ),
            (
                &MockApi::default().addr_make("testvaloper2").into(),
                Decimal::percent(50),
            ),
        ])
        .with_periods(23 * HOUR, 28 * DAY)
        .build();

    let lsd = suite.query_lsd_token().unwrap();

    // deposit some tokens to stake
    suite.bond(delegator, amount).unwrap();

    // wait until next epoch and do first reinvest to actually delegate
    suite.update_time(23 * HOUR);
    suite.reinvest().unwrap();

    // wait one more epoch to get rewards
    suite.update_time(23 * HOUR);
    suite.reinvest().unwrap();

    // now unbond
    suite.unbond(delegator, &lsd, amount).unwrap();

    // wait for next unbonding time and call reinvest to undelegate
    suite.update_time(5 * 23 * HOUR);
    suite.reinvest().unwrap();

    // wait until unbonding period is over and claim
    suite.update_time(28 * DAY);
    suite.process_native_unbonding();
    suite.claim(delegator).unwrap();
}

#[test]
fn small_delegations() {
    let delegator = &MockApi::default().addr_make("delegator");

    let single_amount = 32u128;
    let delegations = 100u128;
    let total_amount = single_amount * delegations;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegator, total_amount)])
        .with_validators(vec![
            (
                &MockApi::default().addr_make("testvaloper1").into(),
                Decimal::percent(50),
            ),
            (
                &MockApi::default().addr_make("testvaloper2").into(),
                Decimal::percent(50),
            ),
        ])
        .with_periods(23 * HOUR, 28 * DAY)
        .build();

    let lsd = suite.query_lsd_token().unwrap();

    for _ in 0..delegations {
        // deposit some tokens to stake
        suite.bond(delegator, single_amount).unwrap();

        // wait until next epoch and do reinvest to actually delegate
        suite.update_time(23 * HOUR);
        suite.reinvest().unwrap();
    }

    // now unbond everything
    let lsd_amount = suite.query_cw20_balance(delegator, &lsd).unwrap();
    suite.unbond(delegator, &lsd, lsd_amount).unwrap();

    // trigger undelegation
    suite.update_time(23 * HOUR);
    suite.reinvest().unwrap();

    let claims = suite.query_claims(delegator.to_string()).unwrap();
    assert_eq!(claims.len(), 1);
    // wait until unbonding period is over and claim
    suite.update_time(claims[0].release_at.seconds() - suite.app.block_info().time.seconds());

    suite.process_native_unbonding();
    suite.claim(delegator).unwrap();
    assert!(suite.query_balance(delegator, "FUN").unwrap() > total_amount.into());
}

#[test]
fn actual_undelegation_flow() {
    let delegators = &[
        &MockApi::default().addr_make("delegator1"),
        &MockApi::default().addr_make("delegator2"),
    ];

    let amount = 1_000_000u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegators[0], amount), (delegators[1], amount)])
        .build();

    // Simulate deposits
    suite.bond(delegators[0], amount).unwrap();
    suite.bond(delegators[1], amount).unwrap();

    // Users now have the same amount of LSD tokens as they staked
    let lsd = suite.query_lsd_token().unwrap();
    let lsd_token_balance = suite.query_cw20_balance(delegators[0], &lsd).unwrap();

    // Wait until next epoch and reinvest to trigger delegation
    suite.update_time(23 * HOUR);
    suite.reinvest().unwrap();

    // Immediately unbond all of delegator[0]'s previously bonded tokens
    suite
        .unbond(delegators[0], &lsd, lsd_token_balance)
        .unwrap();

    // Query claims to verify one was made
    let claims = suite.query_claims(delegators[0].to_string()).unwrap();
    assert_eq!(claims.len(), 1);

    // Advance time by 4 epochs = 92 hours
    for _ in 0..4 {
        // Advance time to next epoch and reinvest
        suite.update_time(23 * HOUR);
        suite.reinvest().unwrap();
    }
    // Next epoch should trigger unbonding
    suite.update_time(23 * HOUR);
    suite.reinvest().unwrap();

    // Advance one unbonding period and claim delegator's tokens
    suite.update_time(28 * DAY);
    suite.process_native_unbonding();
    suite.claim(delegators[0]).unwrap();

    // Verify delegator[0] has their native tokens back
    assert_eq!(
        suite.query_balance(delegators[0], "FUN").unwrap(),
        Uint256::from(amount)
    );

    // Unbond delegator[1]
    suite
        .unbond(delegators[1], &lsd, lsd_token_balance)
        .unwrap();

    // Trigger actual undelegation
    suite.reinvest().unwrap();

    // Advance time by 4 epochs = 92 hours
    for _ in 0..4 {
        // Advance time to next epoch and reinvest
        suite.update_time(23 * HOUR);
        suite.reinvest().unwrap();
    }
    // Trigger unbonding
    suite.update_time(23 * HOUR);
    suite.reinvest().unwrap();

    // Advance one unbonding period and claim delegator's tokens
    suite.update_time(28 * DAY);
    suite.process_native_unbonding();
    suite.claim(delegators[1]).unwrap();

    // Verify delegator[1] has their native tokens back + rewards
    assert!(suite.query_balance(delegators[1], "FUN").unwrap() > amount.into());
}

#[test]
fn bond_unbond_simultaneously() {
    let delegators = &[
        &MockApi::default().addr_make("delegator1"),
        &MockApi::default().addr_make("delegator2"),
    ];

    let amount = 1_000_000u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegators[0], amount), (delegators[1], amount)])
        .build();

    // Deposit with one delegator
    suite.bond(delegators[0], amount).unwrap();

    // User now has the same amount of LSD tokens as they staked
    let lsd = suite.query_lsd_token().unwrap();
    let lsd_token_balance = suite.query_cw20_balance(delegators[0], &lsd).unwrap();

    // Wait until next epoch and reinvest to trigger delegation
    suite.update_time(23 * HOUR);
    suite.reinvest().unwrap();

    // Bond with another delegator
    suite.bond(delegators[1], amount).unwrap();
    // Unbond all of delegators[0]'s previously bonded tokens
    suite
        .unbond(delegators[0], &lsd, lsd_token_balance)
        .unwrap();

    // Unbonding gets triggered every 28 / 7 = 4 days = 24 hours * 4 = 96 hours,
    // so we need to wait 5 epochs + 1 unbonding period
    // we do not need to reinvest because balance from second bond should cover first bond
    suite.update_time(5 * 23 * HOUR + 28 * DAY);
    suite.reinvest().unwrap();

    // Claim delegator's tokens
    suite.claim(delegators[0]).unwrap();

    // Verify delegator[0] has their native tokens back
    assert_eq!(suite.query_balance(delegators[0], "FUN").unwrap(), Uint256::from(amount));
}

#[test]
fn unbond_epoch_handling() {
    const DAY: u64 = 24 * 60 * 60;
    let delegator = &MockApi::default().addr_make("delegator");
    let delegator2 = &MockApi::default().addr_make("delegator2");

    let amount = 1_000_000u128;
    let amount2 = 100_000u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegator, amount), (delegator2, amount2)])
        .with_validators(vec![
            (
                &MockApi::default().addr_make("testvaloper1").into(),
                Decimal::percent(55),
            ),
            (
                &MockApi::default().addr_make("testvaloper2").into(),
                Decimal::percent(35),
            ),
            (
                &MockApi::default().addr_make("testvaloper3").into(),
                Decimal::percent(10),
            ),
        ])
        .with_periods(23 * HOUR, 28 * DAY)
        .build();

    suite.bond(delegator, amount).unwrap();
    suite.bond(delegator2, amount2).unwrap();

    // wait for next epoch to trigger reinvest to actually delegate
    suite.update_time(23 * HOUR);
    suite.reinvest().unwrap();

    // unbond the delegator 2 tokens, taking effect next reinvest
    suite
        .unbond(delegator2, &suite.query_lsd_token().unwrap(), amount2)
        .unwrap();

    // at this point, the supply should be:
    let supply = SUPPLY.query(&suite.app.wrap(), suite.hub.clone()).unwrap();
    assert_eq!(supply.total_bonded.u128(), amount + amount2);
    let bonded = BONDED.query(&suite.app.wrap(), suite.hub.clone()).unwrap();
    assert_eq!(
        HashMap::from([
            (
                MockApi::default().addr_make("testvaloper1").to_string(),
                605_000u128.into()
            ),
            (
                MockApi::default().addr_make("testvaloper2").to_string(),
                385_000u128.into()
            ),
            (
                MockApi::default().addr_make("testvaloper3").to_string(),
                110_000u128.into()
            ),
        ]),
        bonded.into_iter().collect()
    );
    assert_eq!(supply.claims.u128(), amount2);
    assert_eq!(supply.total_unbonding.u128(), 0);
    let storage = suite.read_hub_storage();
    assert_eq!(unbonding_info_num_epochs(&storage), 0);
    assert_eq!(unbonding_info_total_entries(&storage).unwrap(), 0);

    // wait another day and trigger unbonding (first time has no wait)
    suite.update_time(23 * HOUR);
    suite.reinvest().unwrap();
    suite.process_native_unbonding();

    // at this point, the supply should be:
    let supply = SUPPLY.query(&suite.app.wrap(), suite.hub.clone()).unwrap();
    let rewards = 2085u128;
    assert_eq!(supply.total_bonded.u128(), amount + rewards);
    let bonded = BONDED.query(&suite.app.wrap(), suite.hub.clone()).unwrap();
    assert_eq!(
        HashMap::from([
            // 550_000 + rewards * 0.55 + dust
            (
                MockApi::default().addr_make("testvaloper1").to_string(),
                551_146u128.into()
            ),
            // 350_000 + rewards * 0.35 (+1)
            (
                MockApi::default().addr_make("testvaloper2").to_string(),
                350_730u128.into()
            ),
            // 100_000 + rewards * 0.10
            (
                MockApi::default().addr_make("testvaloper3").to_string(),
                100_209u128.into()
            ),
        ]),
        bonded.into_iter().collect()
    );
    assert_eq!(supply.claims.u128(), amount2);
    assert_eq!(supply.total_unbonding.u128(), amount2 - rewards);
    let storage = suite.read_hub_storage();
    assert_eq!(unbonding_info_num_epochs(&storage), 1);
    assert_eq!(unbonding_info_total_entries(&storage).unwrap(), 3);

    // another unbonding
    suite
        .unbond(delegator, &suite.query_lsd_token().unwrap(), amount)
        .unwrap();

    // but this time it won't trigger, as we need to wait unbonding_period / 7
    suite.update_time(23 * HOUR);
    suite.reinvest().unwrap();
    suite.process_native_unbonding();

    // assert the same
    let supply = SUPPLY.query(&suite.app.wrap(), suite.hub.clone()).unwrap();
    assert_eq!(supply.total_bonded.u128(), amount + rewards);
    let bonded = BONDED.query(&suite.app.wrap(), suite.hub.clone()).unwrap();
    assert_eq!(
        HashMap::from([
            // 550_000 + rewards * 0.55 + dust
            (
                MockApi::default().addr_make("testvaloper1").to_string(),
                551_146u128.into()
            ),
            // 350_000 + rewards * 0.35 (+1)
            (
                MockApi::default().addr_make("testvaloper2").to_string(),
                350_730u128.into()
            ),
            // 100_000 + rewards * 0.10
            (
                MockApi::default().addr_make("testvaloper3").to_string(),
                100_209u128.into()
            ),
        ]),
        bonded.into_iter().collect()
    );
    assert_eq!(supply.claims.u128(), amount + amount2 + rewards);
    assert_eq!(supply.total_unbonding.u128(), amount2 - rewards);
    let storage = suite.read_hub_storage();
    assert_eq!(unbonding_info_num_epochs(&storage), 1);
    assert_eq!(unbonding_info_total_entries(&storage).unwrap(), 3);

    // wait 4 more epochs and trigger unbonding
    suite.update_time(4 * 23 * HOUR);
    suite.reinvest().unwrap();
    suite.process_native_unbonding();

    // this time it should have unbonded, claim is still there until payed out
    let supply = SUPPLY.query(&suite.app.wrap(), suite.hub.clone()).unwrap();
    // between unbonding and reinvesting/undelegating, there are some rewards that accrue.
    // these get reinvested...
    let post_unbond_rewards = 9496u128;
    assert_eq!(supply.total_bonded.u128(), post_unbond_rewards);
    assert_eq!(supply.claims.u128(), amount + amount2 + rewards);
    assert_eq!(
        Uint256::new(supply.total_unbonding.u128())
            + suite.query_balance(&suite.hub, "FUN").unwrap(),
        Uint256::new(amount + amount2 + rewards)
    );
    let storage = suite.read_hub_storage();
    assert_eq!(unbonding_info_num_epochs(&storage), 2);
    assert_eq!(unbonding_info_total_entries(&storage).unwrap(), 6);
}
