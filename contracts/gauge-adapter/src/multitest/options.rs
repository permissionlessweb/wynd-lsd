use cosmwasm_std::testing::MockApi;
use cosmwasm_std::Addr;

use crate::multitest::suite::SuiteBuilder;

#[test]
fn option_queries() {
    let a = MockApi::default().addr_make("junovaloper196ax4vc0lwpxndu9dyhvca7jhxp70rmcqcnylw");
    let aa = MockApi::default().addr_make("junovaloper1t8ehvswxjfn3ejzkjtntcyrqwvmvuknzmvtaaa");
    let b = MockApi::default().addr_make("junovaloper1y0us8xvsvfvqkk9c6nt5cfyu5au5tww2wsdcwk");
    let validators = vec![
        (aa.as_str(), "1.0"),
        (a.as_str(), "1.0"),
        (b.as_str(), "1.0"),
    ];
    let suite = SuiteBuilder::new()
        .with_chain_validators(validators.clone())
        .build();

    // get all options
    let options = suite.query_all_options().unwrap();
    assert_eq!(
        options,
        validators
            .iter()
            .map(|v| v.0.to_string())
            .collect::<Vec<_>>()
    );

    // check option validity
    for v in validators {
        assert!(suite.query_check_option(v.0.to_string()).unwrap());
    }
    assert!(!suite
        .query_check_option(Addr::unchecked("invalid").to_string())
        .unwrap());
}

#[test]
fn query_validators_with_commission_cap() {
    let a = MockApi::default().addr_make("junovaloper1t8ehvswxjfn3ejzkjtntcyrqwvmvuknzmvtaaa");
    let b = MockApi::default().addr_make("junovaloper196ax4vc0lwpxndu9dyhvca7jhxp70rmcqcnylw");
    let c = MockApi::default().addr_make("junovaloper196ax4vc0lwpxndu9dyhvca7jhxpsdawytdfsgf");
    let d = MockApi::default().addr_make("junovaloper196ax4vc0lwpxndu9dyhvca7jhxp70rdadsadsa");
    let e = MockApi::default().addr_make("junovaloper1y0us8xvsvfvqkk9c6nt5cfyu5au5tww2wsdcwk");
    let validators = vec![
        (a.as_str(), "1.0"),
        (b.as_str(), "0.2"),
        (c.as_str(), "0.3"),
        (d.as_str(), "0.8"),
        (e.as_str(), "1.0"),
    ];
    let suite = SuiteBuilder::new()
        .with_chain_validators(validators.clone())
        .with_max_allowed_commission("0.3")
        .build();

    // get all options
    let options = suite.query_all_options().unwrap();
    assert_eq!(
        options,
        vec![
            MockApi::default()
                .addr_make("junovaloper196ax4vc0lwpxndu9dyhvca7jhxp70rmcqcnylw")
                .as_str(),
            MockApi::default()
                .addr_make("junovaloper196ax4vc0lwpxndu9dyhvca7jhxpsdawytdfsgf")
                .as_str(),
        ]
    );
}
