// Copyright 2024 RISC Zero, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::fmt::Debug;

use anyhow::Context;
use ethers_core::types::Transaction as EthersTransaction;
use log::{info, warn};
use risc0_zkvm::{compute_image_id, Receipt};
use serde::{Deserialize, Serialize};
use zeth_lib::{
    builder::BlockBuilderStrategy,
    consts::ChainSpec,
    host::{cache_file_path, preflight::Preflight, verify::Verifier},
    input::BlockBuildInput,
    output::BlockBuildOutput,
};

use crate::{
    cli::Cli,
    operations::{execute, maybe_prove, verify_bonsai_receipt},
};

pub async fn build_chain_blocks<N: BlockBuilderStrategy>(
    cli: Cli,
    rpc_url: Option<String>,
    chain_spec: ChainSpec,
    guest_elf: &[u8],
) -> anyhow::Result<Option<(String, Receipt)>>
where
    N::TxEssence: 'static + Send + TryFrom<EthersTransaction> + Serialize + Deserialize<'static>,
    <N::TxEssence as TryFrom<EthersTransaction>>::Error: Debug,
{
    let core_args = cli.core_args().clone();
    // Fetch all of the initial data
    let rpc_cache = core_args.cache.as_ref().map(|dir| {
        cache_file_path(
            dir,
            &core_args.network.to_string(),
            core_args.block_number,
            "json.gz",
        )
    });

    let init_spec = chain_spec.clone();
    let preflight_result = tokio::task::spawn_blocking(move || {
        N::run_preflight(init_spec, rpc_cache, rpc_url, core_args.block_number)
    })
    .await?;
    let preflight_data = preflight_result.context("preflight failed")?;

    // Create the guest input from [Init]
    let input: BlockBuildInput<N::TxEssence> = preflight_data
        .clone()
        .try_into()
        .context("invalid preflight data")?;

    // Verify that the transactions run correctly
    info!("Running from memory ...");
    let output = N::build_from(&chain_spec, input.clone()).context("Error while building block")?;

    match &output {
        BlockBuildOutput::SUCCESS {
            new_block_hash,
            new_block_head,
            new_block_state,
        } => {
            info!("Verifying final state using provider data ...");
            preflight_data.verify_block(new_block_head, new_block_state)?;

            info!("Final block hash derived successfully. {}", new_block_hash);
        }
        BlockBuildOutput::FAILURE { .. } => {
            warn!("Proving bad block construction!")
        }
    }

    let compressed_output = output.with_state_compressed();
    let result = match &cli {
        Cli::Build(..) => None,
        Cli::Run(run_args) => {
            execute(
                &input,
                run_args.exec_args.execution_po2,
                run_args.exec_args.profile,
                guest_elf,
                &compressed_output,
                &cli.execution_label(),
            );
            None
        }
        Cli::Prove(..) => {
            maybe_prove(
                &cli,
                &input,
                guest_elf,
                &compressed_output,
                Default::default(),
            )
            .await
        }
        Cli::Verify(verify_args) => Some(
            verify_bonsai_receipt(
                compute_image_id(guest_elf)?,
                &compressed_output,
                verify_args.bonsai_receipt_uuid.clone(),
                4,
            )
            .await?,
        ),
        Cli::OpInfo(..) => {
            unreachable!()
        }
    };

    Ok(result)
}
