use super::suite::{SuiteBuilder, NATIVE};

use cosmwasm_std::testing::MockApi;
use wyndex::asset::{Asset, AssetInfo};

use cosmwasm_std::{assert_approx_eq, coins, Decimal, Decimal256, Uint128};

use std::str::FromStr;

#[test]
fn bond_with_equal_pool_liquidity() {
    let user = &MockApi::default().addr_make("user");
    let admin = &MockApi::default().addr_make("admin");
    let mut suite = SuiteBuilder::new()
        .with_funds(user, (200_000_000u128, NATIVE))
        .with_funds(admin, (500_000_000u128, NATIVE))
        .with_lsd_funds(admin, 500_000_000u128)
        .build();

    let lsd_pool = suite.lsd_pool.clone();
    let lsd_token = suite.lsd_token.clone();

    suite
        .increase_allowance(admin, &lsd_token, &lsd_pool, 500_000_000u128)
        .unwrap();
    // pools are balanced, and beside the discount rate price should be close to target
    suite
        .provide_liquidity(
            admin,
            &lsd_pool,
            &[
                Asset {
                    info: AssetInfo::Token(lsd_token.to_string()),
                    amount: 500_000_000u128.into(),
                },
                Asset {
                    info: AssetInfo::Native(NATIVE.to_owned()),
                    amount: 500_000_000u128.into(),
                },
            ],
            &coins(500_000_000, NATIVE),
        )
        .unwrap();

    assert_eq!(suite.query_exchange_rate().unwrap(), Decimal::one());

    // price has 3% discount
    assert_eq!(
        suite.query_spot_price().unwrap(),
        Decimal256::from_str("1.029533").unwrap()
    );

    let expected_lsd_amount = suite.query_simulate(100_000_000u128).unwrap();

    // Given current amplification = 23 (so 11.5), almost 30% of liquidity is around
    // certain price range in this target rate
    suite.bond(user, (100_000_000u128, NATIVE)).unwrap();

    // Since spot price is still not equal to target, whole trade went through the pool
    // and no LSD tokens were bonded (issued)
    assert_eq!(
        suite.query_spot_price().unwrap(),
        Decimal256::from_str("1.010637").unwrap()
    );
    let issued_lsd = suite.query_lsd_supply().unwrap().issued;
    assert_eq!(issued_lsd, Uint128::zero());

    let lsd_balance = suite.query_cw20_balance(user, &lsd_token).unwrap();
    assert_eq!(lsd_balance, expected_lsd_amount, "0.0000001");

    let expected_lsd_amount = suite.query_simulate(100_000_000u128).unwrap();
    suite.bond(user, (100_000_000u128, NATIVE)).unwrap();

    // after second round of bond pool price is almost at target 1.0 (algorithm is working)
    assert_eq!(
        suite.query_spot_price().unwrap(),
        Decimal256::from_str("1.000315").unwrap()
    );
    // and since price was close to the target, some of the trade went through the bond
    let issued_lsd = suite.query_lsd_supply().unwrap().issued;
    assert_ne!(issued_lsd, Uint128::zero());

    let lsd_balance_second = suite.query_cw20_balance(user, &lsd_token).unwrap();
    // add previously received tokens to the expected amount to get actual balance
    assert_eq!(
        lsd_balance_second,
        expected_lsd_amount + lsd_balance,
        "0.0000001"
    );

    // Assert that user has more LSD tokens then the native ones he bonded.
    // It means that user got a better price and that algorithm works
    assert!(lsd_balance_second > 200_000_000u128.into());
}

#[test]
fn bond_with_uneven_pool_liquidity() {
    let user = &MockApi::default().addr_make("user");
    let admin = &MockApi::default().addr_make("admin");
    let mut suite = SuiteBuilder::new()
        .with_funds(user, (500_000_000u128, NATIVE))
        .with_funds(admin, (400_000_000u128, NATIVE))
        .with_lsd_funds(admin, 500_000_000u128)
        .build();

    let lsd_pool = suite.lsd_pool.clone();
    let lsd_token = suite.lsd_token.clone();

    suite
        .increase_allowance(admin, &lsd_token, &lsd_pool, 500_000_000u128)
        .unwrap();
    // in this test case, first bond provides as much liquidity as is missing for the pool
    // to be balanced
    suite
        .provide_liquidity(
            admin,
            &lsd_pool,
            &[
                Asset {
                    info: AssetInfo::Token(lsd_token.to_string()),
                    amount: 500_000_000u128.into(),
                },
                Asset {
                    info: AssetInfo::Native(NATIVE.to_owned()),
                    amount: 300_000_000u128.into(),
                },
            ],
            &coins(300_000_000, NATIVE),
        )
        .unwrap();

    assert_eq!(suite.query_exchange_rate().unwrap(), Decimal::one());

    assert_eq!(
        suite.query_spot_price().unwrap(),
        Decimal256::from_str("1.053529").unwrap()
    );

    // similar to test above, but this time bonding 200_000_000 is not enough to
    // get spot price close to the target rate
    suite.bond(user, (200_000_000u128, NATIVE)).unwrap();

    assert_eq!(
        suite.query_spot_price().unwrap(),
        Decimal256::from_str("1.004035").unwrap()
    );
    // because spot price wasn't close enough to the target, whole transaction
    // went through swap - no tokens were bonded
    let issued_lsd = suite.query_lsd_supply().unwrap().issued;
    assert_eq!(issued_lsd, Uint128::zero());

    // Another amount takes us already closer to the target rate, which means
    // that new LSD tokens are minted
    suite.bond(user, (50_000_000u128, NATIVE)).unwrap();

    assert_eq!(
        suite.query_spot_price().unwrap(),
        Decimal256::from_str("1.000121").unwrap()
    );

    let issued_lsd = suite.query_lsd_supply().unwrap().issued;
    assert_ne!(issued_lsd, Uint128::zero());

    // Assert that user has more LSD tokens then the native ones he bonded in total.
    // It means that user got a better price and that algorithm works
    let lsd_balance = suite.query_cw20_balance(user, &lsd_token).unwrap();
    assert!(lsd_balance > 250_000_000u128.into());
}
