use crate::RateLimitRule;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateStateError {
    InsufficientBalance,
    AmountTooLarge { amount: u64 },
    InvalidRule { max_capacity: i64 },
    InternalInvariantViolation,
}

#[derive(Clone)]
pub(crate) struct RateState {
    balance: i64,
    last_refill_micros: u64,
}

impl RateState {
    pub(crate) fn from_rule(now: u64, rule: &RateLimitRule) -> Result<Self, RateStateError> {
        if rule.max_capacity < 0 {
            return Err(RateStateError::InvalidRule {
                max_capacity: rule.max_capacity,
            });
        }
        Ok(Self::new(rule.max_capacity, now))
    }

    pub(crate) fn new(balance: i64, last_refill_micros: u64) -> Self {
        Self {
            balance,
            last_refill_micros,
        }
    }

    pub(crate) fn balance(&self) -> i64 {
        self.balance
    }

    /// Computes the projected balance after refill at `now`, capped by `rule.max_capacity`.
    ///
    /// # Arguments
    /// * `now` - Current timestamp in microseconds.
    /// * `rule` - Rate limit rule providing refill rate and maximum capacity.
    ///
    /// # Returns
    /// The projected post-refill balance.
    ///
    /// # Errors
    /// Returns `RateStateError::InvalidRule` if `rule.max_capacity` is negative.
    fn calculate_refill(&self, now: u64, rule: &RateLimitRule) -> Result<i64, RateStateError> {
        if rule.max_capacity < 0 {
            return Err(RateStateError::InvalidRule {
                max_capacity: rule.max_capacity,
            });
        }
        let elapsed = now.saturating_sub(self.last_refill_micros);
        let added = rule.rate_per_micro.saturating_mul(elapsed);
        let added_i64 = i64::try_from(added).unwrap_or(i64::MAX);
        Ok(self
            .balance
            .saturating_add(added_i64)
            .min(rule.max_capacity))
    }

    /// Validates that `amount` can be withdrawn after applying a simulated refill at `now`.
    ///
    /// # Arguments
    /// * `amount` - Amount to test for withdrawal.
    /// * `now` - Current timestamp in microseconds.
    /// * `rule` - Rate limit rule providing refill rate and maximum capacity.
    ///
    /// # Returns
    /// `Ok(())` if the projected post-refill balance can cover `amount`.
    ///
    /// # Errors
    /// Returns:
    /// - `RateStateError::InsufficientBalance` if the projected post-refill balance is below `amount`.
    /// - `RateStateError::AmountTooLarge` if `amount` exceeds `i64::MAX`.
    /// - `RateStateError::InvalidRule` if `rule.max_capacity` is negative.
    pub(crate) fn check_withdraw_after_refill(
        &self,
        amount: u64,
        now: u64,
        rule: &RateLimitRule,
    ) -> Result<(), RateStateError> {
        let amount_i64 =
            i64::try_from(amount).map_err(|_| RateStateError::AmountTooLarge { amount })?;
        let projected_balance = self.calculate_refill(now, rule)?;
        if projected_balance < amount_i64 {
            return Err(RateStateError::InsufficientBalance);
        }
        Ok(())
    }

    /// Applies the execution-phase state transition for a previously validated operation.
    ///
    /// This method is intentionally mutation-only and does not perform access validation.
    /// It is meant to run after a separate check phase has already confirmed that the
    /// projected post-refill balance can cover `amount`.
    ///
    /// Execution order is:
    /// 1. Compute the post-refill balance at `now` using `calculate_refill`.
    /// 2. Persist that refilled balance.
    /// 3. Debit `amount` from the refilled balance.
    ///
    /// This keeps refill/debit math centralized while allowing a two-phase flow where
    /// validation and mutation are separated.
    ///
    /// # Arguments
    /// * `amount` - Amount to debit.
    /// * `now` - Current timestamp in microseconds used for refill calculation.
    /// * `rule` - Rate limit rule that defines refill rate and maximum capacity.
    ///
    /// # Errors
    /// Returns:
    /// - `RateStateError::InsufficientBalance` if debiting `amount` would make balance negative.
    /// - `RateStateError::AmountTooLarge` if `amount` exceeds `i64::MAX`.
    /// - `RateStateError::InvalidRule` if `rule.max_capacity` is negative.
    pub(crate) fn withdraw_after_refill(
        &mut self,
        amount: u64,
        now: u64,
        rule: &RateLimitRule,
    ) -> Result<(), RateStateError> {
        let amount_i64 =
            i64::try_from(amount).map_err(|_| RateStateError::AmountTooLarge { amount })?;
        let refilled_balance = self.calculate_refill(now, rule)?;
        if refilled_balance < amount_i64 {
            return Err(RateStateError::InsufficientBalance);
        }
        self.balance = refilled_balance.saturating_sub(amount_i64);
        Ok(())
    }

    /// Credits `amount` back to the balance and clamps to `rule.max_capacity`.
    ///
    /// # Arguments
    /// * `amount` - Amount to refund.
    /// * `rule` - Rate limit rule that defines maximum capacity.
    ///
    /// # Errors
    /// Returns:
    /// - `RateStateError::AmountTooLarge` if `amount` exceeds `i64::MAX`.
    /// - `RateStateError::InvalidRule` if `rule.max_capacity` is negative.
    pub(crate) fn refund(
        &mut self,
        amount: u64,
        rule: &RateLimitRule,
    ) -> Result<(), RateStateError> {
        if rule.max_capacity < 0 {
            return Err(RateStateError::InvalidRule {
                max_capacity: rule.max_capacity,
            });
        }
        let amount_i64 =
            i64::try_from(amount).map_err(|_| RateStateError::AmountTooLarge { amount })?;
        self.balance = self
            .balance
            .saturating_add(amount_i64)
            .min(rule.max_capacity);
        Ok(())
    }

}
