//! Default-deny external call filter for an explicitly composed research Runtime.
//! Only the two Balances transfer variants are permitted. Origins are checked by
//! Balances itself; Contains cannot authenticate a signer. Root/unfiltered and
//! internal currency calls bypass this boundary and remain separately trusted.
//! The consumer must use the exact outer variant name `Balances`; unknown/new
//! pallets and nested dispatch wrappers default to denied.
#[macro_export]
macro_rules! define_closed_monetary_calls {
    ($visibility:vis struct $name:ident for $runtime_call:ident) => {
        $visibility struct $name;
        impl frame_support::traits::Contains<$runtime_call> for $name {
            fn contains(call: &$runtime_call) -> bool {
                matches!(call,
                    $runtime_call::Balances(pallet_balances::Call::transfer_allow_death { .. }) |
                    $runtime_call::Balances(pallet_balances::Call::transfer_keep_alive { .. }))
            }
        }
    };
}
