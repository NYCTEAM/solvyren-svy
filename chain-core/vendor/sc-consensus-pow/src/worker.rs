// This file is part of Substrate.

// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use futures::{
	lock::Mutex as AsyncMutex,
	prelude::*,
	task::{Context, Poll},
};
use futures_timer::Delay;
use log::*;
use parking_lot::Mutex as ParkingMutex;
use sc_client_api::ImportNotifications;
use sc_consensus::{
	BlockImportParams, BoxBlockImport, ImportResult, StateAction, StorageChanges,
};
use sp_consensus::{BlockOrigin, Proposal};
use sp_runtime::{
	generic::BlockId,
	traits::{Block as BlockT, Header as HeaderT},
	DigestItem,
};
use std::{
	pin::Pin,
	sync::{
		atomic::{AtomicUsize, Ordering},
		Arc,
	},
	time::Duration,
};

use crate::{PowAlgorithm, PowIntermediate, Seal, INTERMEDIATE_KEY, LOG_TARGET, POW_ENGINE_ID};

/// Mining metadata. This is the information needed to start an actual mining loop.
#[derive(Clone, Eq, PartialEq)]
pub struct MiningMetadata<H, D> {
	/// Currently known best hash which the pre-hash is built on.
	pub best_hash: H,
	/// Mining pre-hash.
	pub pre_hash: H,
	/// Pre-runtime digest item.
	pub pre_runtime: Option<Vec<u8>>,
	/// Mining target difficulty.
	pub difficulty: D,
}

/// A build of mining, containing the metadata and the block proposal.
pub struct MiningBuild<Block: BlockT, Algorithm: PowAlgorithm<Block>, Proof> {
	/// Mining metadata.
	pub metadata: MiningMetadata<Block::Hash, Algorithm::Difficulty>,
	/// Mining proposal.
	pub proposal: Proposal<Block, Proof>,
}

/// Version of the mining worker.
#[derive(Eq, PartialEq, Clone, Copy)]
pub struct Version(usize);

/// Result of a version-bound mining submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitOutcome {
	/// The captured build was imported successfully.
	Imported,
	/// The captured build was replaced before submission; no import was attempted.
	Stale,
	/// The supplied seal failed local verification; no import was attempted.
	Rejected,
	/// Import was attempted but returned an error, so callers must reconcile durably.
	Unknown,
}

/// Mining worker that exposes structs to query the current mining build and submit mined blocks.
pub struct MiningHandle<
	Block: BlockT,
	Algorithm: PowAlgorithm<Block>,
	L: sc_consensus::JustificationSyncLink<Block>,
	Proof,
> {
	version: Arc<AtomicUsize>,
	algorithm: Arc<Algorithm>,
	justification_sync_link: Arc<L>,
	build: Arc<ParkingMutex<Option<MiningBuild<Block, Algorithm, Proof>>>>,
	block_import: Arc<AsyncMutex<BoxBlockImport<Block>>>,
}

impl<Block, Algorithm, L, Proof> MiningHandle<Block, Algorithm, L, Proof>
where
	Block: BlockT,
	Algorithm: PowAlgorithm<Block>,
	Algorithm::Difficulty: 'static + Send,
	L: sc_consensus::JustificationSyncLink<Block>,
{
	fn increment_version(&self) {
		self.version.fetch_add(1, Ordering::SeqCst);
	}

	pub(crate) fn new(
		algorithm: Algorithm,
		block_import: BoxBlockImport<Block>,
		justification_sync_link: L,
	) -> Self {
		Self {
			version: Arc::new(AtomicUsize::new(0)),
			algorithm: Arc::new(algorithm),
			justification_sync_link: Arc::new(justification_sync_link),
			build: Arc::new(ParkingMutex::new(None)),
			block_import: Arc::new(AsyncMutex::new(block_import)),
		}
	}

	pub(crate) fn on_major_syncing(&self) {
		let mut build = self.build.lock();
		*build = None;
		self.increment_version();
	}

	pub(crate) fn on_build(&self, value: MiningBuild<Block, Algorithm, Proof>) {
		let mut build = self.build.lock();
		*build = Some(value);
		self.increment_version();
	}

	/// Get the version of the mining worker.
	///
	/// This returns type `Version` which can only compare equality. If `Version` is unchanged, then
	/// it can be certain that `best_hash` and `metadata` were not changed.
	pub fn version(&self) -> Version {
		Version(self.version.load(Ordering::SeqCst))
	}

	/// Get the current best hash. `None` if the worker has just started or the client is doing
	/// major syncing.
	pub fn best_hash(&self) -> Option<Block::Hash> {
		self.build.lock().as_ref().map(|b| b.metadata.best_hash)
	}

	/// Get a copy of the current mining metadata, if available.
	pub fn metadata(&self) -> Option<MiningMetadata<Block::Hash, Algorithm::Difficulty>> {
		self.build.lock().as_ref().map(|b| b.metadata.clone())
	}

	/// Atomically snapshot the current build generation and its metadata.
	///
	/// Miners must retain the returned `Version` and pass it to [`Self::submit_at`].
	/// Reading `version()` and `metadata()` separately can observe different builds when a new
	/// proposal replaces the current one between those calls.
	pub fn snapshot(
		&self,
	) -> Option<(Version, MiningMetadata<Block::Hash, Algorithm::Difficulty>)> {
		let build = self.build.lock();
		build.as_ref().map(|build| {
			(
				Version(self.version.load(Ordering::SeqCst)),
				build.metadata.clone(),
			)
		})
	}

	/// Submit a mined seal. The seal will be validated again. Returns true if the submission is
	/// successful.
	pub async fn submit(&self, seal: Seal) -> bool {
		self.submit_at(self.version(), seal).await
	}

	/// Submit only if the current build still has the supplied version.
	/// A stale submission never consumes a newer build. Once taken, a build is
	/// consumed even if verification or import fails.
	pub async fn submit_at(&self, expected: Version, seal: Seal) -> bool {
		self.submit_at_detailed(expected, seal).await == SubmitOutcome::Imported
	}

	/// Submit a version-bound seal while retaining whether an import was attempted.
	pub async fn submit_at_detailed(&self, expected: Version, seal: Seal) -> SubmitOutcome {
		let build = {
			let mut slot = self.build.lock();
			if self.version.load(Ordering::SeqCst) != expected.0 {
				return SubmitOutcome::Stale
			}
			let value = slot.take();
			if value.is_some() {
				self.increment_version();
			}
			value
		};
		let build = if let Some(build) = build {
			build
		} else {
			warn!(target: LOG_TARGET, "Unable to import mined block: build does not exist");
			return SubmitOutcome::Stale
		};
		{
			let metadata = &build.metadata;
			match self.algorithm.verify(
				&BlockId::Hash(metadata.best_hash),
				&metadata.pre_hash,
				metadata.pre_runtime.as_ref().map(|v| &v[..]),
				&seal,
				metadata.difficulty,
			) {
				Ok(true) => (),
				Ok(false) => {
					warn!(target: LOG_TARGET, "Unable to import mined block: seal is invalid",);
					return SubmitOutcome::Rejected
				},
				Err(err) => {
					warn!(target: LOG_TARGET, "Unable to import mined block: {}", err,);
					return SubmitOutcome::Rejected
				},
			}
		}

		let seal = DigestItem::Seal(POW_ENGINE_ID, seal);
		let (header, body) = build.proposal.block.deconstruct();

		let mut import_block = BlockImportParams::new(BlockOrigin::Own, header);
		import_block.post_digests.push(seal);
		import_block.body = Some(body);
		import_block.state_action =
			StateAction::ApplyChanges(StorageChanges::Changes(build.proposal.storage_changes));

		let intermediate = PowIntermediate::<Algorithm::Difficulty> {
			difficulty: Some(build.metadata.difficulty),
		};
		import_block.insert_intermediate(INTERMEDIATE_KEY, intermediate);

		let header = import_block.post_header();
		let block_import = self.block_import.lock().await;

		match block_import.import_block(import_block).await {
			Ok(ImportResult::Imported(aux)) if !aux.header_only => {
				let result = ImportResult::Imported(aux);
				result.handle_justification(
					&header.hash(),
					*header.number(),
					&self.justification_sync_link,
				);

				info!(
					target: LOG_TARGET,
					"✅ Successfully mined block on top of: {}", build.metadata.best_hash
				);
				SubmitOutcome::Imported
			},
			Ok(ImportResult::KnownBad) => {
				warn!(target: LOG_TARGET, "Unable to import mined block: block or parent is known bad");
				SubmitOutcome::Rejected
			},
			Ok(ImportResult::Imported(_)) => {
				warn!(target: LOG_TARGET, "Unable to confirm complete mined block import: header-only result");
				SubmitOutcome::Unknown
			},
			Ok(ImportResult::AlreadyInChain) => {
				warn!(target: LOG_TARGET, "Unable to confirm complete mined block import: already in chain");
				SubmitOutcome::Unknown
			},
			Ok(ImportResult::UnknownParent) => {
				warn!(target: LOG_TARGET, "Unable to import mined block: parent is unknown");
				SubmitOutcome::Unknown
			},
			Ok(ImportResult::MissingState) => {
				warn!(target: LOG_TARGET, "Unable to import mined block: parent state is missing");
				SubmitOutcome::Unknown
			},
			Err(err) => {
				warn!(target: LOG_TARGET, "Unable to import mined block: {}", err,);
				SubmitOutcome::Unknown
			},
		}
	}
}

impl<Block, Algorithm, L, Proof> Clone for MiningHandle<Block, Algorithm, L, Proof>
where
	Block: BlockT,
	Algorithm: PowAlgorithm<Block>,
	L: sc_consensus::JustificationSyncLink<Block>,
{
	fn clone(&self) -> Self {
		Self {
			version: self.version.clone(),
			algorithm: self.algorithm.clone(),
			justification_sync_link: self.justification_sync_link.clone(),
			build: self.build.clone(),
			block_import: self.block_import.clone(),
		}
	}
}

/// A stream that waits for a block import or timeout.
pub struct UntilImportedOrTimeout<Block: BlockT> {
	import_notifications: ImportNotifications<Block>,
	timeout: Duration,
	inner_delay: Option<Delay>,
}

impl<Block: BlockT> UntilImportedOrTimeout<Block> {
	/// Create a new stream using the given import notification and timeout duration.
	pub fn new(import_notifications: ImportNotifications<Block>, timeout: Duration) -> Self {
		Self { import_notifications, timeout, inner_delay: None }
	}
}

impl<Block: BlockT> Stream for UntilImportedOrTimeout<Block> {
	type Item = ();

	fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<()>> {
		let mut fire = false;

		loop {
			match Stream::poll_next(Pin::new(&mut self.import_notifications), cx) {
				Poll::Pending => break,
				Poll::Ready(Some(_)) => {
					fire = true;
				},
				Poll::Ready(None) => return Poll::Ready(None),
			}
		}

		let timeout = self.timeout;
		let inner_delay = self.inner_delay.get_or_insert_with(|| Delay::new(timeout));

		match Future::poll(Pin::new(inner_delay), cx) {
			Poll::Pending => (),
			Poll::Ready(()) => {
				fire = true;
			},
		}

		if fire {
			self.inner_delay = None;
			Poll::Ready(Some(()))
		} else {
			Poll::Pending
		}
	}
}
