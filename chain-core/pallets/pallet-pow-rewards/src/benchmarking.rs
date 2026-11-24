//! Benchmarks for the mandatory reward-finalization path.

use super::*;
use frame_benchmarking::v2::*;
use frame_support::traits::{Currency, Get};

#[benchmarks]
mod benchmarks {
    use super::*;

    /// Diagnostic control for the benchmark host's database tracker. This key
    /// exists only in disposable benchmark externalities and is wiped after
    /// every iteration.
    #[benchmark]
    fn direct_storage_control() {
        const KEY: &[u8] = b"eagw:benchmark:direct-storage-control";
        frame_support::storage::unhashed::put_raw(KEY, b"before");

        #[block]
        {
            assert_eq!(
                frame_support::storage::unhashed::get_raw(KEY).as_deref(),
                Some(&b"before"[..])
            );
            frame_support::storage::unhashed::put_raw(KEY, b"after");
        }

        assert_eq!(
            frame_support::storage::unhashed::get_raw(KEY).as_deref(),
            Some(&b"after"[..])
        );
    }

    /// Same control inside a committed FRAME storage transaction. Comparing
    /// this result with `direct_storage_control` distinguishes host tracking
    /// defects from transaction-overlay behavior without changing production
    /// reward logic.
    #[benchmark]
    fn transactional_storage_control() {
        const KEY: &[u8] = b"eagw:benchmark:transaction-storage-control";
        frame_support::storage::unhashed::put_raw(KEY, b"before");

        #[block]
        {
            frame_support::storage::with_transaction(|| {
                assert_eq!(
                    frame_support::storage::unhashed::get_raw(KEY).as_deref(),
                    Some(&b"before"[..])
                );
                frame_support::storage::unhashed::put_raw(KEY, b"after");
                frame_support::storage::TransactionOutcome::Commit(Ok::<
                    (),
                    sp_runtime::DispatchError,
                >(()))
            })
            .expect("diagnostic transaction must commit");
        }

        assert_eq!(
            frame_support::storage::unhashed::get_raw(KEY).as_deref(),
            Some(&b"after"[..])
        );
    }

    #[benchmark]
    fn issue_current_block() {
        // Do not use `whitelisted_caller()` here: FRAME excludes that account's
        // System storage key from database accounting, which would undercount
        // the first reward-account write performed by the real hook.
        let beneficiary: T::AccountId = account("reward-beneficiary", 0, 0);
        assert!(beneficiary != T::AiPool::get());
        T::BenchmarkHelper::setup(beneficiary.clone())
            .expect("the runtime must prepare its real reward source and schedule");
        let before = Pallet::<T>::ledger().expect("reward ledger must exist");
        let issuance_before = T::Currency::total_issuance();

        #[block]
        {
            Pallet::<T>::issue_current_block().expect("benchmark reward must succeed");
        }

        let after = Pallet::<T>::ledger().expect("reward ledger must remain present");
        assert_eq!(after.last_height, before.last_height + 1);
        assert!(after.lifetime_issued > before.lifetime_issued);
        assert!(T::Currency::total_issuance() > issuance_before);
    }

    /// Reachable steady-state path after the same miner and the AI pool have
    /// both been credited by a preceding valid block.
    #[benchmark]
    fn issue_current_block_late_nonzero() {
        let beneficiary: T::AccountId = account("late-reward-beneficiary", 0, 0);
        assert!(beneficiary != T::AiPool::get());
        // period index 2070: exponent 2047 exercises eleven set bits while
        // the experimental 10^12 fixed-point schedule still has a reward.
        T::BenchmarkHelper::setup_high_height(beneficiary, 2_070 * 219_000 + 1)
            .expect("late nonzero fixture must be configured");
        let before = Pallet::<T>::ledger().expect("reward ledger exists");
        #[block]
        {
            Pallet::<T>::issue_current_block().expect("late nonzero reward must succeed");
        }
        let after = Pallet::<T>::ledger().unwrap();
        assert_eq!(after.last_height, before.last_height + 1);
        assert!(after.lifetime_issued > before.lifetime_issued);
    }

    #[benchmark]
    fn issue_current_block_late_zero() {
        let beneficiary: T::AccountId = account("zero-reward-beneficiary", 0, 0);
        // period index 16406: exponent 16383 exercises fourteen set bits.
        T::BenchmarkHelper::setup_high_height(beneficiary, 16_406 * 219_000 + 1)
            .expect("late zero fixture must be configured");
        let before = Pallet::<T>::ledger().expect("reward ledger exists");
        #[block]
        {
            Pallet::<T>::issue_current_block().expect("late zero reward must succeed");
        }
        let after = Pallet::<T>::ledger().unwrap();
        assert_eq!(after.last_height, before.last_height + 1);
        assert_eq!(after.lifetime_issued, before.lifetime_issued);
    }

    #[benchmark]
    fn issue_current_block_existing_accounts() {
        let beneficiary: T::AccountId = account("reward-existing-beneficiary", 0, 0);
        assert!(beneficiary != T::AiPool::get());
        T::BenchmarkHelper::setup_existing_accounts(beneficiary.clone())
            .expect("the runtime must prepare a valid preceding reward block");
        let before = Pallet::<T>::ledger().expect("reward ledger must exist");
        let issuance_before = T::Currency::total_issuance();

        #[block]
        {
            Pallet::<T>::issue_current_block().expect("existing-account reward must succeed");
        }

        let after = Pallet::<T>::ledger().expect("reward ledger must remain present");
        assert_eq!(after.last_height, before.last_height + 1);
        assert!(after.lifetime_issued > before.lifetime_issued);
        assert!(T::Currency::total_issuance() > issuance_before);
    }

    /// Existing miner account that must take Balances' automatic legacy
    /// account upgrade before the reward can be credited. The AI pool remains
    /// an ordinary existing account, so the measurement isolates the one
    /// additional protocol branch instead of combining unrelated setup.
    #[benchmark]
    fn issue_current_block_legacy_beneficiary() {
        let beneficiary: T::AccountId = account("reward-legacy-beneficiary", 0, 0);
        assert!(beneficiary != T::AiPool::get());
        T::BenchmarkHelper::setup_legacy_beneficiary(beneficiary.clone())
            .expect("the runtime must prepare a legacy beneficiary reward block");
        let before = Pallet::<T>::ledger().expect("reward ledger must exist");
        let issuance_before = T::Currency::total_issuance();

        #[block]
        {
            Pallet::<T>::issue_current_block().expect("legacy-beneficiary reward must succeed");
        }

        let after = Pallet::<T>::ledger().expect("reward ledger must remain present");
        assert_eq!(after.last_height, before.last_height + 1);
        assert!(after.lifetime_issued > before.lifetime_issued);
        assert!(T::Currency::total_issuance() > issuance_before);
    }

    /// The protocol permits a miner to name the configured AI pool as its
    /// beneficiary. Both credits then touch the same account, which is a
    /// distinct reachable storage path and must be measured independently.
    #[benchmark]
    fn issue_current_block_shared_account() {
        let beneficiary = T::AiPool::get();
        T::BenchmarkHelper::setup(beneficiary.clone())
            .expect("the runtime must prepare its real reward source and schedule");
        let before = Pallet::<T>::ledger().expect("reward ledger must exist");
        let issuance_before = T::Currency::total_issuance();

        #[block]
        {
            Pallet::<T>::issue_current_block().expect("shared-account reward must succeed");
        }

        let after = Pallet::<T>::ledger().expect("reward ledger must remain present");
        assert_eq!(after.last_height, before.last_height + 1);
        assert!(after.lifetime_issued > before.lifetime_issued);
        assert!(T::Currency::total_issuance() > issuance_before);
    }
}
