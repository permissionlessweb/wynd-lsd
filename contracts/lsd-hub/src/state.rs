use std::ops::{Deref, DerefMut};

use crate::ContractError;
use cosmwasm_schema::cw_serde;
use cosmwasm_std::{
    Addr, Decimal, Deps, Env, Order, OverflowError, StdError, StdResult, Storage, Uint128,
};
use cw_storage_plus::{Bound, Item, Map};

use crate::claim::Claims;

#[cw_serde]
pub struct Config {
    /// Owner of the contract, is allowed to set valset
    pub owner: Addr,
    pub token_contract: Addr,
    /// The address that receives the commission
    pub treasury: Addr,
    /// The proportion of the staking rewards that goes to the treasury
    pub commission: Decimal,

    /// The frequency in which reinvest can be called
    pub epoch_period: u64,
    /// The staking module's unbonding time, in seconds
    pub unbond_period: u64,
    /// The maximum number of unbonding queue entries per validator at any time
    pub max_concurrent_unbondings: u64,

    /// This is the next time (in seconds) at which `reinvest` can be called
    pub next_epoch: u64,
    /// This is the next time (in seconds) at which unbondings can take place in `reinvest`
    pub next_unbond: u64,

    /// The minimum relative difference between the stored and queried delegations needed to consider a validator as tombstoned.
    pub tombstone_treshold: Decimal,
    /// The safety margin (in seconds) around unbondings where we don't allow slashing detection
    /// in order to not confuse unbonding with slashing.
    /// If there are any unbondings this many seconds in the future or past, we don't allow slashing detection.
    pub slashing_safety_margin: u64,

    /// The expected discount applied to the underlying value of the staking token in the `TargetValue` query.
    /// The idea here is that no one will want to buy the staking token at exactly the price of the underlying,
    /// because they are locked and can potentially be slashed. So we apply a discount to the price.
    pub liquidity_discount: Decimal,
}

impl Config {
    /// Progresses to the next reinvest epoch after the given timestamp, and returns that timestamp.
    /// Returns error if epoch has not passes.
    pub fn next_epoch_after(&mut self, env: &Env) -> Result<u64, ContractError> {
        let timestamp = env.block.time.seconds();
        if timestamp < self.next_epoch {
            Err(ContractError::EpochNotReached {
                next_epoch: self.next_epoch,
            })
        } else {
            // calculate the next epoch, making sure it keeps the same rythm even if we don't call immediately
            // and works even if we skip a few epochs
            let epochs_until_then = (timestamp - self.next_epoch) / self.epoch_period;
            self.next_epoch += (epochs_until_then + 1) * self.epoch_period;
            Ok(self.next_epoch)
        }
    }

    /// Progresses to the next unbonding epoch after the given timestamp, and returns that timestamp.
    /// Returns error if epoch has not passes.
    pub fn next_unbond_after(&mut self, env: &Env) -> Result<u64, ContractError> {
        let timestamp = env.block.time.seconds();
        if timestamp < self.next_unbond {
            Err(ContractError::EpochNotReached {
                next_epoch: self.next_unbond,
            })
        } else {
            // calculate the next epoch, making sure it keeps the same rythm even if we don't call immediately
            // and works even if we skip a few epochs
            let epoch_period = self.unbond_epoch();
            let epochs_until_then = (timestamp - self.next_unbond) / epoch_period;
            self.next_unbond += (epochs_until_then + 1) * epoch_period;
            Ok(self.next_unbond)
        }
    }

    pub fn unbond_epoch(&self) -> u64 {
        div_ceil(self.unbond_period, self.max_concurrent_unbondings)
    }
}

/// Investment info is fixed at instantiation, and is used to control the function of the contract
#[cw_serde]
pub struct StakeInfo {
    /// All tokens are bonded to these validators with the given weights
    pub validators: Vec<(String, Decimal)>,
}

#[cw_serde]
pub struct Unbonding {
    // renamed to save some space
    /// The amount of tokens we are unbonding
    #[serde(rename = "a")]
    pub amount: Uint128,
    /// The validator we are unbonding from
    #[serde(rename = "v")]
    pub validator: String,
}

/// how many tokens are currently bonded to each validator.
/// this does not include those in process of undelegating.
pub const BONDED: Item<Vec<(String, Uint128)>> = Item::new("bonded");

/// Store all pending unbondings, indexed by the expiration (when it will be ready).
/// We unbond in large groups, so expect a few entries each with many validators
pub const UNBONDING: Map<u64, Vec<Unbonding>> = Map::new("unbonding");

/// Deletes all unbonding items that are mature and return the number of tokens that have
/// completed unbonding
pub fn clean_unbonding(storage: &mut dyn Storage, env: &Env) -> StdResult<Uint128> {
    let time = env.block.time.seconds();

    // clean up all old ones
    let mature = UNBONDING
        .range(
            storage,
            None,
            Some(Bound::inclusive(time)),
            Order::Ascending,
        )
        .collect::<StdResult<Vec<_>>>()?;
    for (key, _) in &mature {
        UNBONDING.remove(storage, *key);
    }

    // sum up what we got
    let freed = mature
        .into_iter()
        .map(|(_, bonds)| bonds.into_iter().map(|u| u.amount).sum::<Uint128>())
        .sum();
    Ok(freed)
}

/// Like clean_unbonding, but designed for readonly queries. Just counts how many unbonding
/// items are mature but doesn't delete
pub fn count_unbonding(storage: &dyn Storage, env: &Env) -> StdResult<Uint128> {
    let time = env.block.time.seconds();
    let mature = UNBONDING
        .range(
            storage,
            None,
            Some(Bound::inclusive(time)),
            Order::Ascending,
        )
        .map(|r| {
            let (_, bonds) = r?;
            Ok(bonds.into_iter().map(|u| u.amount).sum::<Uint128>())
        })
        .collect::<StdResult<Vec<_>>>()?;
    let freed = mature.into_iter().sum();
    Ok(freed)
}

pub fn unbondings_expiring_between(
    storage: &dyn Storage,
    start: u64,
    end: u64,
) -> impl Iterator<Item = StdResult<(u64, Vec<Unbonding>)>> + '_ {
    UNBONDING.range(
        storage,
        Some(Bound::exclusive(start)),
        Some(Bound::exclusive(end)),
        Order::Ascending,
    )
}

/// Only for tests. How many different unbonding epochs are there.
pub fn unbonding_info_num_epochs(storage: &dyn Storage) -> u64 {
    UNBONDING
        .range(storage, None, None, Order::Ascending)
        .count() as u64
}

/// Only for tests. How many different unbonding entries there are over all epochs.
pub fn unbonding_info_total_entries(storage: &dyn Storage) -> StdResult<u64> {
    let mature = UNBONDING
        .range(storage, None, None, Order::Ascending)
        .map(|r| {
            let (_, bonds) = r?;
            Ok(bonds.len() as u64)
        })
        .collect::<StdResult<Vec<_>>>()?;
    let freed = mature.into_iter().sum();
    Ok(freed)
}

/// Supply is dynamic and tracks the current supply of tokens in various states
/// Locked value = bonded + unbonding - claims + deps.querier.balance()
/// Promised shares = issued
/// Ratio = Locked / promised
/// All actions besides reinvest should keep ratio
#[cw_serde]
#[derive(Default)]
pub struct Supply {
    /// This is the denomination we can stake (and only one we accept for payments)
    pub bond_denom: String,
    /// issued is how many derivative tokens this contract has issued
    pub issued: Uint128,
    /// total_bonded how many native tokens exist bonded to the validators in total
    pub total_bonded: Uint128,
    /// claims is how many tokens need to be reserved paying back those who unbonded
    pub claims: Uint128,
    /// the total amount of tokens that are currently unbonding
    /// this should always be equal to `supply.unbonding.into_iter().map(|u| u.amount).sum()`
    pub total_unbonding: Uint128,
}

impl Supply {
    pub fn new(bond_denom: String) -> Self {
        Self {
            bond_denom,
            ..Default::default()
        }
    }

    /// Removes the amount from claims
    pub fn claim(&mut self, amount: Uint128) -> Result<(), OverflowError> {
        self.claims = self.claims.checked_sub(amount)?;
        Ok(())
    }

    // returns the current bank balance of this contract
    pub fn balance(&self, deps: Deps, env: &Env) -> Result<Uint128, StdError> {
        let coin = deps
            .querier
            .query_balance(&env.contract.address, &self.bond_denom)?;
        Ok(coin.amount.try_into().unwrap())
    }

    pub fn cleanup_unbonding(
        mut self,
        storage: &mut dyn Storage,
        env: &Env,
    ) -> StdResult<CleanedSupply> {
        let freed = clean_unbonding(storage, env)?;
        self.total_unbonding -= freed;
        Ok(CleanedSupply(self))
    }
}

/// Wrapper around [`Supply`] that ensures old unbonding queue entries are cleaned up before updating the delegations.
pub struct CleanedSupply(Supply);

impl Deref for CleanedSupply {
    type Target = Supply;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl DerefMut for CleanedSupply {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl CleanedSupply {
    /// Removes all expired unbonding entries
    pub fn load(storage: &mut dyn Storage, env: &Env) -> StdResult<Self> {
        let mut supply = SUPPLY.load(storage)?;
        let freed = clean_unbonding(storage, env)?;
        supply.total_unbonding -= freed;
        Ok(CleanedSupply(supply))
    }

    /// Updates unbonding count but doesn't delete state (meant for queries)
    pub fn load_for_query(storage: &dyn Storage, env: &Env) -> StdResult<Self> {
        let mut supply = SUPPLY.load(storage)?;
        let freed = count_unbonding(storage, env)?;
        supply.total_unbonding -= freed;
        Ok(CleanedSupply(supply))
    }

    /// Returns the ratio of TVL / Outstanding shares.
    /// This should be maintained on deposits and withdrawals and only
    /// modify (increase) on reinvest.
    /// You must pass in the current balance of the contract (Bank balance)
    pub fn tokens_per_share(&self, balance: Uint128) -> Decimal {
        // ensure that we return 1 at the beginning (when no ratio has been set)
        if self.issued.is_zero() {
            Decimal::one()
        } else {
            Decimal::from_ratio(self.assets(balance), self.issued)
        }
    }

    /// This is 1/tokens_per_share, implemented here to reduce rounding
    pub fn shares_per_token(&self, balance: Uint128) -> Decimal {
        let assets = self.assets(balance);
        // ensure that we return 1 at the beginning (when no ratio has been set)
        if assets.is_zero() {
            Decimal::one()
        } else {
            Decimal::from_ratio(self.issued, assets)
        }
    }

    /// Returns the total amount of native tokens that are backing all of the lsd tokens.
    #[inline]
    fn assets(&self, balance: Uint128) -> Uint128 {
        self.total_bonded + self.total_unbonding + balance - self.claims
    }

    /// Removes the given `amount` from the issued tokens and adds the corresponding native amount to claims.
    /// Also returns the native claim amount
    /// The amount parameter is denominated in lsd tokens.
    /// Note that this only updates the supply. Make sure to create a claim for the user as well.
    pub fn unbond(&mut self, amount: Uint128, balance: Uint128) -> Uint128 {
        let native = amount.mul_floor(self.tokens_per_share(balance));
        self.issued -= amount;
        self.claims += native;

        native
    }
}

#[cw_serde]
pub struct TmpState {
    #[serde(rename = "b")]
    pub balance: Uint128,
}

#[cw_serde]
pub struct Slashing {
    pub start: u64,
    pub end: u64,
    pub multiplier: Decimal,
}

pub const SUPPLY: Item<Supply> = Item::new("supply");
pub const CONFIG: Item<Config> = Item::new("config");
pub const STAKE_INFO: Item<StakeInfo> = Item::new("stake_info");
/// This item is used to store some temporary state between the message initiating the reinvest process
/// and the reply we get after withdrawing the rewards.
pub const TMP_STATE: Item<TmpState> = Item::new("tmp_state");
pub const CLAIMS: Claims = Claims::new("claims");
pub const SLASHINGS: Item<Vec<Slashing>> = Item::new("slashings");

/// Divides `numerator` by `denominator` and rounds up the result.
/// This is needed because [`std`]'s implementation is currently unstable.
/// ```rust
/// use wynd_lsd_hub::state::div_ceil;
/// let numerator = 5;
/// let denominator = 2;
/// assert_eq!(div_ceil(numerator, denominator), 3);
///
/// ```
/// ```rust
/// use wynd_lsd_hub::state::div_ceil;
/// let numerator = 5;
/// let denominator = 3;
/// assert_eq!(div_ceil(numerator, denominator), 2);
/// ```
/// ```rust
/// use wynd_lsd_hub::state::div_ceil;
/// let numerator = 6;
/// let denominator = 7;
/// assert_eq!(div_ceil(numerator, denominator), 1);
/// ```
pub fn div_ceil(numerator: u64, denominator: u64) -> u64 {
    let d = numerator / denominator;
    let r = numerator % denominator;
    if r > 0 {
        d + 1
    } else {
        d
    }
}
