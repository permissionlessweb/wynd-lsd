use cosmwasm_std::testing::MockApi;
use cosmwasm_std::Decimal;

use super::suite::SuiteBuilder;

#[test]
fn updating_validators_works() {
    let mut suite = SuiteBuilder::new().build();

    let selected = vec![
        (
            MockApi::default()
                .addr_make("junovaloper196ax4vc0lwpxndu9dyhvca7jhxp70rmcqcnylw")
                .to_string(),
            Decimal::percent(50),
        ),
        (
            "junovaloper1y0us8xvsvfvqkk9c6nt5cfyu5au5tww2wsdcwk".to_string(),
            Decimal::percent(20),
        ),
    ];
    // sample messages
    let messages = suite.sample_gauge_msgs(selected.clone());

    // should be able to update validators
    suite
        .app
        .execute_multi(suite.owner.clone(), messages)
        .unwrap();

    // check if the validators are updated
    let validators = suite.query_validator_set().unwrap();
    assert_eq!(validators, selected);
}
