use std::{collections::HashMap, str::FromStr};

use cosmwasm_std::testing::MockApi;
use cosmwasm_std::{Decimal, Uint256};

use crate::multitest::suite::SuiteBuilder;
use crate::state::BONDED;

const HOUR: u64 = 60 * 60;
const DAY: u64 = 24 * HOUR;
const EPOCH: u64 = 23 * HOUR;

fn test_empty_rewards(validators: Vec<(&String, Decimal)>, empty_validators: &[&str]) {
    let delegator = &MockApi::default().addr_make("delegator");

    let amount = 1_000_000u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegator, amount)])
        .with_validators(validators.clone())
        .with_periods(EPOCH, 28 * DAY)
        .build();

    // Deposit some tokens to stake
    suite.bond(delegator, amount).unwrap();

    // wait until next epoch and do first reinvest to actually delegate
    suite.update_time(EPOCH);
    suite.reinvest().unwrap();

    // bonded amount should be staked to the validators now
    let delegations = suite.query_delegations().unwrap();
    assert_eq!(delegations.len(), validators.len());
    for empty_val in empty_validators {
        assert_eq!(
            delegations
                .iter()
                .find(|d| d.validator == *empty_val)
                .unwrap()
                .amount
                .amount,
            Uint256::one(),
            "validator {} should have tiny stake",
            empty_val
        );
    }

    // wait until next epoch to trigger reinvest
    suite.update_time(DAY);
    // make sure we get no rewards from the validator
    let full_delegations = suite.query_full_delegations().unwrap();
    assert_eq!(full_delegations.len(), 2);
    // these validators should have no rewards
    for empty_val in empty_validators {
        assert!(full_delegations
            .iter()
            .find(|d| d.validator == *empty_val)
            .unwrap()
            .accumulated_rewards
            .is_empty());
    }
    // this should work without error and delegate all rewards (no more rewards left)
    suite.reinvest().unwrap();
    assert_eq!(
        suite
            .query_full_delegations()
            .unwrap()
            .into_iter()
            .flat_map(|d| d.accumulated_rewards)
            .map(|c| c.amount)
            .sum::<Uint256>(),
        Uint256::zero()
    );
    assert_eq!(
        suite.query_balance(&suite.hub, "FUN").unwrap(),
        Uint256::zero()
    );
}

#[test]
fn reinvest_failing_withdraw() {
    // we set a very low weight to one of the validators to make the stake so small that we don't get any rewards
    test_empty_rewards(
        vec![
            (
                &MockApi::default().addr_make("testvaloper1").to_string(),
                Decimal::from_str("0.000001").unwrap(),
            ),
            (
                &MockApi::default().addr_make("testvaloper2").to_string(),
                Decimal::from_str("0.999999").unwrap(),
            ),
        ],
        &[&MockApi::default().addr_make("testvaloper1").to_string()],
    );
    // now the other way around (to test that it also works if the last message fails)
    test_empty_rewards(
        vec![
            (
                &MockApi::default().addr_make("testvaloper1").to_string(),
                Decimal::from_str("0.999999").unwrap(),
            ),
            (
                &MockApi::default().addr_make("testvaloper2").to_string(),
                Decimal::from_str("0.000001").unwrap(),
            ),
        ],
        &[&MockApi::default().addr_make("testvaloper2").to_string()],
    );
}

#[test]
fn bonded_updated_correctly() {
    let delegator = &MockApi::default().addr_make("delegator");

    let amount = 1_000_000u128;
    let mut suite = SuiteBuilder::new()
        .with_initial_balances(vec![(delegator, amount)])
        .with_validators(vec![
            (
                &MockApi::default().addr_make("testvaloper1").to_string(),
                Decimal::percent(50),
            ),
            (
                &MockApi::default().addr_make("testvaloper2").to_string(),
                Decimal::percent(50),
            ),
        ])
        .with_periods(EPOCH, 28 * DAY)
        .build();

    // Deposit some tokens to stake
    suite.bond(delegator, amount).unwrap();

    // wait until next epoch and do first reinvest to actually delegate
    suite.update_time(EPOCH);
    suite.reinvest().unwrap();

    let bonded = BONDED.query(&suite.app.wrap(), suite.hub.clone()).unwrap();
    assert_eq!(
        HashMap::from([
            (
                MockApi::default().addr_make("testvaloper1").to_string(),
                (amount / 2).into()
            ),
            (MockApi::default().addr_make("testvaloper2").to_string(), (amount / 2).into())
        ]),
        bonded.into_iter().collect()
    );

    // unbond half of the tokens
    suite
        .unbond(delegator, &suite.query_lsd_token().unwrap(), amount / 2)
        .unwrap();

    // wait until next unbonding epoch and do reinvest to actually undelegate
    suite.update_time(5 * EPOCH);
    suite.reinvest().unwrap();

    // 80% APR, 5% validator commission, 5% treasury commission
    // => rewards per year: 0.8 * 0.95 * 0.95 * amount = 0.722 * amount
    // let rewards = 5 * EPOCH as u128 * amount * 722 / (365 * DAY as u128 * 1000);
    let rewards = 9478u128;

    let bonded = BONDED.query(&suite.app.wrap(), suite.hub.clone()).unwrap();
    assert_eq!(
        HashMap::from([
            (
                MockApi::default().addr_make("testvaloper1").to_string(),
                (amount / 4 + rewards / 2).into()
            ),
            (
                MockApi::default().addr_make("testvaloper2").to_string(),
                (amount / 4 + rewards / 2).into()
            )
        ]),
        bonded.into_iter().collect()
    );

    let exchange_rate = suite.query_exchange_rate().unwrap();
    assert_eq!(
        exchange_rate,
        Decimal::from_ratio(amount / 2 + rewards, amount / 2),
        "half of the tokens are unbonded, but rewards and other half should still be there"
    );
}
