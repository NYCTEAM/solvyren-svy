use crate::{Balance, BalancesCall, Runtime, RuntimeCall, RuntimeOrigin, TransactionPayment};
use codec::{Decode, DecodeWithMemTracking, Encode};
use frame_support::{
    dispatch::{DispatchInfo, Pays, PostDispatchInfo},
    weights::Weight,
};
use pallet_transaction_payment::{
    ChargeTransactionPayment as Standard, FeeDetails, InclusionFee, OnChargeTransaction, Pre, Val,
};
use scale_info::TypeInfo;
use sp_runtime::{
    traits::TransactionExtension,
    transaction_validity::{TransactionSource, TransactionValidityError, ValidTransaction},
    DispatchResult,
};

pub const TRANSFER_FEE: Balance = 5_000_000;

pub fn is_transfer(call: &RuntimeCall) -> bool {
    matches!(
        call,
        RuntimeCall::Balances(
            BalancesCall::transfer_keep_alive { .. } | BalancesCall::transfer_allow_death { .. }
        )
    )
}

pub fn transfer_fee_details() -> FeeDetails<Balance> {
    FeeDetails {
        inclusion_fee: Some(InclusionFee {
            base_fee: TRANSFER_FEE,
            len_fee: 0,
            adjusted_weight_fee: 0,
        }),
        tip: 0,
    }
}

type Charger = <Runtime as pallet_transaction_payment::Config>::OnChargeTransaction;

#[derive(Encode, Decode, DecodeWithMemTracking, Clone, Eq, PartialEq, TypeInfo, Debug)]
pub struct TransferPayment(#[codec(compact)] Balance);

impl From<Balance> for TransferPayment {
    fn from(tip: Balance) -> Self {
        Self(tip)
    }
}

impl TransactionExtension<RuntimeCall> for TransferPayment {
    const IDENTIFIER: &'static str = "ChargeTransactionPayment";
    type Implicit = ();
    type Val = Val<Runtime>;
    type Pre = (bool, Pre<Runtime>);

    fn weight(&self, call: &RuntimeCall) -> Weight {
        Standard::<Runtime>::from(self.0).weight(call)
    }

    fn validate(
        &self,
        origin: RuntimeOrigin,
        call: &RuntimeCall,
        info: &DispatchInfo,
        len: usize,
        implicit: (),
        implication: &impl sp_runtime::traits::Implication,
        source: TransactionSource,
    ) -> Result<(ValidTransaction, Self::Val, RuntimeOrigin), TransactionValidityError> {
        if is_transfer(call) {
            if let Ok(who) = frame_system::ensure_signed(origin.clone()) {
                let fee = TRANSFER_FEE.saturating_add(self.0);
                <Charger as OnChargeTransaction<Runtime>>::can_withdraw_fee(
                    &who, call, info, fee, self.0,
                )?;
                let priority = Standard::<Runtime>::get_priority(info, len, self.0, fee);
                return Ok((
                    ValidTransaction {
                        priority,
                        ..Default::default()
                    },
                    Val::Charge {
                        tip: self.0,
                        who,
                        fee,
                    },
                    origin,
                ));
            }
        }
        Standard::<Runtime>::from(self.0).validate(
            origin,
            call,
            info,
            len,
            implicit,
            implication,
            source,
        )
    }

    fn prepare(
        self,
        val: Self::Val,
        origin: &RuntimeOrigin,
        call: &RuntimeCall,
        info: &DispatchInfo,
        len: usize,
    ) -> Result<Self::Pre, TransactionValidityError> {
        Ok((
            is_transfer(call),
            Standard::<Runtime>::from(self.0).prepare(val, origin, call, info, len)?,
        ))
    }

    fn post_dispatch_details(
        (fixed, pre): Self::Pre,
        info: &DispatchInfo,
        post: &PostDispatchInfo,
        len: usize,
        result: &DispatchResult,
    ) -> Result<Weight, TransactionValidityError> {
        match (fixed, pre) {
            (
                true,
                Pre::Charge {
                    who,
                    tip,
                    imbalance,
                },
            ) => {
                let actual = if post.pays_fee(info) == Pays::Yes {
                    TRANSFER_FEE.saturating_add(tip)
                } else {
                    tip
                };
                <Charger as OnChargeTransaction<Runtime>>::correct_and_deposit_fee(
                    &who, info, post, actual, tip, imbalance,
                )?;
                TransactionPayment::deposit_fee_paid_event(who, actual, tip);
                Ok(Weight::zero())
            }
            (_, pre) => Standard::<Runtime>::post_dispatch_details(pre, info, post, len, result),
        }
    }
}
