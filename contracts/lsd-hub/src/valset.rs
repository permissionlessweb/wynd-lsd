use std::{cmp::Ordering, collections::BTreeMap};

use crate::state::Supply;
use cosmwasm_std::{coin, Decimal, StakingMsg, StdResult, Uint128};

pub struct ValsetChange {
    pub messages: Vec<StakingMsg>,
    pub new_balances: Vec<(String, Uint128)>,
}
/// Calculates the necessary redelegation messages to change the validator set,
/// given the old balances and the target validator set.
///
/// Please note that this assumes the old balances to be the actual staked tokens *without* undelegations.
pub fn valset_change_redelegation_messages<'a>(
    supply: &Supply,
    old_balances: impl Iterator<Item = (&'a String, Uint128)>,
    new_valset: impl Iterator<Item = (&'a String, Decimal)>,
) -> StdResult<ValsetChange> {
    let mut msgs: Vec<StakingMsg> = vec![];

    let mut delegate_from: Vec<(&String, Uint128)> = vec![];
    let mut delegate_to: Vec<(&String, Uint128)> = vec![];

    // collect sets into BTreeMap for faster lookup (and predictable order when collecting)
    let mut balances: BTreeMap<_, _> = old_balances.collect();
    // Map this to amounts here (as we only use as amount below)
    let new_valset: BTreeMap<_, Uint128> = new_valset
        .into_iter()
        .map(|(addr, weight)| (addr, supply.total_bonded.mul_floor(weight)))
        .collect();

    debug_assert_eq!(
        supply.total_bonded,
        balances.values().copied().sum::<Uint128>()
    );

    for (old_validator, old_amount) in balances.clone() {
        // if old validator in new validator list, compute difference
        match new_valset.get(old_validator) {
            // Weights has changed, calculate the difference
            Some(new_amount) => {
                match old_amount.cmp(new_amount) {
                    Ordering::Greater => {
                        // if old amount is greater than new amount, delegate from old validator
                        delegate_from.push((old_validator, old_amount - new_amount));
                    }
                    Ordering::Less => {
                        // if old amount is less than new amount, delegate to old validator
                        delegate_to.push((old_validator, new_amount - old_amount));
                    }
                    _ => {} // Weights are the same, do nothing
                }
            }
            None => {
                // Validator is not present in the new list, undelegate everything
                delegate_from.push((old_validator, old_amount));
            }
        }
    }

    // add new validators that aren't present in the old list
    for (new_validator, new_amount) in &new_valset {
        if !balances.contains_key(new_validator) {
            delegate_to.push((new_validator, *new_amount))
        }
    }

    // sorting from highest to lowest to (probably) reduce the number of messages
    delegate_from.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    delegate_to.sort_unstable_by(|a, b| b.1.cmp(&a.1));

    // now i have two lists of validators to delegate from and to
    // i need to compute the amount to delegate from each validator
    for (addr_to, amount_to) in delegate_to.iter_mut() {
        for (addr_from, amount_from) in delegate_from.iter_mut() {
            // take as much as possible, but not more than needed
            let amount = std::cmp::min(*amount_from, *amount_to);
            if amount.is_zero() {
                continue;
            }
            let msg = redelegate_msg(
                addr_from.clone(),
                addr_to.clone(),
                amount.u128(),
                supply.bond_denom.to_string(),
            );
            msgs.push(msg);

            *amount_from -= amount;
            *amount_to -= amount;

            // also update supply
            *balances
                .get_mut(*addr_from)
                .expect("delegate_from contained address with no stake") -= amount;
            *balances.entry(*addr_to).or_insert(Uint128::zero()) += amount;
        }
    }

    Ok(ValsetChange {
        messages: msgs,
        new_balances: balances
            .into_iter()
            .filter(|(_, b)| !b.is_zero())
            .map(|(val, bal)| (val.clone(), bal))
            .collect(),
    })
}

fn redelegate_msg(
    from: impl Into<String>,
    to: impl Into<String>,
    amount: u128,
    denom: impl Into<String>,
) -> StakingMsg {
    StakingMsg::Redelegate {
        src_validator: from.into(),
        dst_validator: to.into(),
        amount: coin(amount, denom.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use cosmwasm_std::Uint128;

    use crate::state::Supply;

    use super::*;

    #[test]
    fn basic_test() {
        let valsets = vec![
            vec![
                ("a".to_string(), Decimal::percent(25)),
                ("b".to_string(), Decimal::percent(25)),
                ("c".to_string(), Decimal::percent(50)),
            ],
            vec![
                ("a".to_string(), Decimal::percent(20)),
                ("b".to_string(), Decimal::percent(35)),
                ("d".to_string(), Decimal::percent(45)),
            ],
            vec![
                ("f".to_string(), Decimal::percent(10)),
                ("g".to_string(), Decimal::percent(20)),
                ("h".to_string(), Decimal::percent(30)),
                ("i".to_string(), Decimal::percent(40)),
            ],
            vec![
                ("a".to_string(), Decimal::percent(67)),
                ("b".to_string(), Decimal::percent(33)),
            ],
        ];

        let supply = Supply {
            bond_denom: "FUN".to_string(),
            issued: 1000u128.into(),
            total_bonded: 1000u128.into(),
            claims: Uint128::zero(),
            total_unbonding: Uint128::zero(),
        };

        let mut balances = vec![
            ("a".to_string(), 500u128.into()),
            ("b".to_string(), 500u128.into()),
        ]
        .into_iter()
        .collect();
        // go through all valsets and simulate the changes from one to the next
        for new_valset in valsets.iter() {
            balances = simulate_valset(&supply, balances, new_valset.clone());
        }

        // simulate change from last valset to itself
        let new_balances =
            simulate_valset(&supply, balances.clone(), valsets.last().unwrap().clone());
        assert_eq!(balances, new_balances, "should be a noop");
    }

    #[test]
    fn rounding() {
        let supply = Supply {
            bond_denom: "FUN".to_string(),
            issued: 4444u128.into(),
            total_bonded: 4444u128.into(),
            claims: Uint128::zero(),
            total_unbonding: Uint128::zero(),
        };

        let initial_balances: HashMap<_, _> = vec![
            ("a".to_string(), supply.total_bonded / Uint128::from(2u128)),
            ("b".to_string(), supply.total_bonded / Uint128::from(2u128)),
        ]
        .into_iter()
        .collect();

        let new_valset = vec![
            ("a".to_string(), Decimal::from_ratio(1u128, 3u128)),
            ("b".to_string(), Decimal::from_ratio(1u128, 3u128)),
            ("c".to_string(), Decimal::from_ratio(1u128, 3u128)),
        ];

        simulate_valset(&supply, initial_balances.clone(), new_valset.clone());

        let new_valset2 = vec![
            ("a".to_string(), Decimal::from_ratio(1u128, 6u128)),
            ("b".to_string(), Decimal::from_ratio(1u128, 6u128)),
            ("c".to_string(), Decimal::from_ratio(1u128, 6u128)),
            ("d".to_string(), Decimal::from_ratio(1u128, 6u128)),
            ("e".to_string(), Decimal::from_ratio(1u128, 6u128)),
            ("f".to_string(), Decimal::from_ratio(1u128, 6u128)),
        ];

        simulate_valset(&supply, initial_balances, new_valset2.clone());
        simulate_valset(
            &supply,
            valset_to_balances(&supply, new_valset.clone()),
            new_valset2.clone(),
        );

        // from new_valset to itself
        let supply = Supply {
            bond_denom: "FUN".to_string(),
            issued: 1000u128.into(),
            total_bonded: 1000u128.into(),
            claims: Uint128::zero(),
            total_unbonding: Uint128::zero(),
        };
        simulate_valset(
            &supply,
            valset_to_balances(&supply, new_valset.clone()),
            new_valset,
        );
        simulate_valset(
            &supply,
            valset_to_balances(&supply, new_valset2.clone()),
            new_valset2.clone(),
        );

        let new_valset3 = vec![
            ("c".to_string(), Decimal::percent(25)),
            ("d".to_string(), Decimal::percent(25)),
            ("e".to_string(), Decimal::percent(25)),
            ("f".to_string(), Decimal::percent(25)),
        ];
        simulate_valset(
            &supply,
            valset_to_balances(&supply, new_valset2),
            new_valset3,
        );
    }

    #[test]
    fn actual_balances_example() {
        // mock supply of 1000 staked tokens
        let supply = Supply {
            bond_denom: "FUN".to_string(),
            issued: 1000u128.into(),
            total_bonded: 1000u128.into(),
            claims: Uint128::zero(),
            total_unbonding: Uint128::zero(),
        };

        // mock initial split of 500 tokens each
        let balances = vec![
            ("a".to_string(), 500u128.into()),
            ("b".to_string(), 500u128.into()),
        ]
        .into_iter()
        .collect();

        let new_valset = vec![
            ("a".to_string(), Decimal::percent(15)),
            ("c".to_string(), Decimal::percent(50)),
            ("d".to_string(), Decimal::percent(35)),
        ];

        let balances = simulate_valset(&supply, balances, new_valset);
        assert_eq!(
            balances,
            vec![
                ("a".to_string(), 150u128.into()),
                ("c".to_string(), 500u128.into()),
                ("d".to_string(), 350u128.into()),
            ]
            .into_iter()
            .collect()
        );

        // now one that does not divide evenly
        let new_valset = vec![
            ("a".to_string(), Decimal::from_ratio(1u128, 3u128)),
            ("b".to_string(), Decimal::from_ratio(1u128, 3u128)),
            ("c".to_string(), Decimal::from_ratio(1u128, 3u128)),
        ];
        let balances = simulate_valset(&supply, balances, new_valset);
        assert_eq!(
            balances,
            vec![
                ("a".to_string(), 333u128.into()),
                ("b".to_string(), 333u128.into()),
                ("c".to_string(), 334u128.into()),
            ]
            .into_iter()
            .collect()
        );
    }

    fn valset_to_balances(
        supply: &Supply,
        valset: Vec<(String, Decimal)>,
    ) -> HashMap<String, Uint128> {
        let mut balances: HashMap<_, _> = valset
            .into_iter()
            .map(|(addr, weight)| (addr, supply.total_bonded.mul_floor(weight)))
            .collect();

        let sum = balances.iter().map(|(_, v)| v).sum::<Uint128>();
        if sum < supply.total_bonded {
            // add the remainder to one of the validators
            let lucky_validator = balances.keys().next().unwrap().clone();
            *balances.get_mut(&lucky_validator).unwrap() += supply.total_bonded - sum;
        }

        balances
    }

    /// Validates the following invariants:
    /// - total balance is equal to bonded
    /// - each validator has at least `floor(total bonded * weight)` balance
    fn validate_valset(
        supply: &Supply,
        valset: Vec<(String, Decimal)>,
        balances: HashMap<String, Uint128>,
    ) {
        for (addr, weight) in valset {
            let balance = balances.get(&addr).unwrap();
            let expected = supply.total_bonded.mul_floor(weight);
            assert!(
                balance >= &expected,
                "balance for {} should be at least {}, but was only {}",
                addr,
                expected,
                balance
            );
        }

        assert_eq!(
            balances.values().sum::<Uint128>(),
            supply.total_bonded,
            "total balance should be equal to total bonded"
        );
    }

    fn simulate_valset(
        supply: &Supply,
        old_balances: HashMap<String, Uint128>,
        new_valset: Vec<(String, Decimal)>,
    ) -> HashMap<String, Uint128> {
        let ValsetChange {
            messages,
            new_balances,
        } = valset_change_redelegation_messages(
            supply,
            old_balances.iter().map(|(k, v)| (k, *v)),
            new_valset.iter().map(|(k, v)| (k, *v)),
        )
        .unwrap();

        let mut balances: HashMap<_, _> = old_balances.into_iter().collect();

        for message in messages {
            match message {
                StakingMsg::Redelegate {
                    src_validator,
                    dst_validator,
                    amount,
                } => {
                    assert_eq!(amount.denom, supply.bond_denom, "denom sanity check");
                    assert!(!amount.amount.is_zero(), "amount sanity check");

                    let redelegate_amount = Uint128::try_from(amount.amount).unwrap();
                    // update balance of both validators
                    let src_balance = balances
                        .get_mut(&src_validator)
                        .expect("src_validator has to exist in the balances map");
                    *src_balance = src_balance
                        .checked_sub(redelegate_amount)
                        .unwrap_or_else(|_| {
                            panic!(
                                "overflowed when redelegating {} from {} to {}",
                                amount.amount, src_validator, dst_validator
                            )
                        });
                    if src_balance.is_zero() {
                        balances.remove(&src_validator);
                    }
                    // dst_validator might not exist yet
                    let dst_balance = balances.entry(dst_validator).or_insert(Uint128::zero());
                    *dst_balance += redelegate_amount;
                }
                _ => panic!("unexpected message"),
            }
        }

        // after simulating all messages, the balances should be valid for the new valset (modulo rounding errors)
        validate_valset(supply, new_valset, balances.clone());
        // also make sure that the new balances are equal to the ones returned by the function
        assert_eq!(balances, new_balances.into_iter().collect());
        balances
    }
}
