use crate::asp::ListVtxo;
use crate::wallet::BoardingWallet;
use crate::wallet::OnchainWallet;
use ark_core::asp::VtxoOutPoint;
use ark_core::default_vtxo::DefaultVtxo;
use ark_core::ArkAddress;
use bitcoin::key::Keypair;
use bitcoin::key::Secp256k1;
use bitcoin::secp256k1::All;
use bitcoin::Address;
use bitcoin::Amount;
use bitcoin::OutPoint;
use bitcoin::Transaction;
use bitcoin::Txid;
use futures::Future;
use std::sync::Arc;
use std::time::Duration;

#[allow(warnings)]
#[allow(clippy::all)]
mod generated {
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub mod std {
        #[path = ""]
        pub mod ark {
            #[path = "ark.v1.rs"]
            pub mod v1;
        }
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub use std::*;

    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    pub mod nostd {
        #[path = ""]
        pub mod ark {
            #[path = "ark.v1.rs"]
            pub mod v1;
        }
    }

    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    pub use nostd::*;
}

pub mod asp;
pub mod error;
pub mod round;
pub mod wallet;

mod coin_select;
mod send_vtxo;
mod unilateral_exit;
mod utils;

pub use error::Error;

pub struct OfflineClient<B, W> {
    asp_client: asp::Client,
    pub name: String,
    pub kp: Keypair,
    blockchain: Arc<B>,
    secp: Secp256k1<All>,
    wallet: Arc<W>,
}

pub struct Client<B, W> {
    inner: OfflineClient<B, W>,
    pub asp_info: asp::Info,
}

#[derive(Clone, Copy, Debug)]
pub struct ExplorerUtxo {
    pub outpoint: OutPoint,
    pub amount: Amount,
    pub confirmation_blocktime: Option<u64>,
}

pub trait Blockchain {
    fn find_outpoints(
        &self,
        address: &Address,
    ) -> impl Future<Output = Result<Vec<ExplorerUtxo>, Error>> + Send;

    fn find_tx(
        &self,
        txid: &Txid,
    ) -> impl Future<Output = Result<Option<Transaction>, Error>> + Send;

    fn broadcast(&self, tx: &Transaction) -> impl Future<Output = Result<(), Error>> + Send;
}

impl<B, W> OfflineClient<B, W>
where
    B: Blockchain,
    W: BoardingWallet + OnchainWallet,
{
    pub fn new(
        name: String,
        kp: Keypair,
        blockchain: Arc<B>,
        wallet: Arc<W>,
        asp_url: String,
    ) -> Self {
        let secp = Secp256k1::new();

        let asp_client = asp::Client::new(asp_url);

        Self {
            asp_client,
            name,
            kp,
            blockchain,
            secp,
            wallet,
        }
    }

    pub async fn connect(mut self) -> Result<Client<B, W>, Error> {
        self.asp_client.connect().await?;
        let asp_info = self.asp_client.get_info().await?;

        Ok(Client {
            inner: self,
            asp_info,
        })
    }
}

impl<B, W> Client<B, W>
where
    B: Blockchain,
    W: BoardingWallet + OnchainWallet,
{
    // At the moment we are always generating the same address.
    pub fn get_offchain_address(&self) -> (ArkAddress, DefaultVtxo) {
        let asp_info = &self.asp_info;

        let (asp, _) = asp_info.pk.inner.x_only_public_key();
        let (owner, _) = self.inner.kp.public_key().x_only_public_key();

        let default_vtxo = DefaultVtxo::new(
            self.secp(),
            asp,
            owner,
            asp_info.unilateral_exit_delay,
            asp_info.network,
        );

        let ark_address = default_vtxo.to_ark_address();

        (ark_address, default_vtxo)
    }

    pub fn get_offchain_addresses(&self) -> Vec<(ArkAddress, DefaultVtxo)> {
        let address = self.get_offchain_address();

        vec![address]
    }

    pub async fn list_vtxos(&self) -> Result<Vec<ListVtxo>, Error> {
        let addresses = self.get_offchain_addresses();

        let mut vtxos = vec![];
        for (address, _) in addresses.into_iter() {
            let list = self.asp_client().list_vtxos(address).await?;
            vtxos.push(list);
        }

        Ok(vtxos)
    }

    pub async fn spendable_vtxos(&self) -> Result<Vec<(Vec<VtxoOutPoint>, DefaultVtxo)>, Error> {
        let addresses = self.get_offchain_addresses();

        let now = std::time::UNIX_EPOCH
            .elapsed()
            .map_err(Error::coin_select)?;

        let mut spendable = vec![];
        for (address, vtxo) in addresses.into_iter() {
            let vtxos = self.asp_client().list_vtxos(address).await?;
            let explorer_utxos = self.blockchain().find_outpoints(vtxo.address()).await?;

            let mut vtxo_outpoints = Vec::new();
            for vtxo_outpoint in vtxos.spendable {
                if let Some(outpoint) = vtxo_outpoint.outpoint {
                    match explorer_utxos
                        .iter()
                        .find(|explorer_utxo| explorer_utxo.outpoint == outpoint)
                    {
                        // Include VTXOs that have been confirmed on the blockchain, but whose
                        // exit path is still _inactive_.
                        Some(ExplorerUtxo {
                            confirmation_blocktime: Some(confirmation_blocktime),
                            ..
                        }) if !vtxo.can_be_claimed_unilaterally_by_owner(
                            now,
                            Duration::from_secs(*confirmation_blocktime),
                        ) =>
                        {
                            vtxo_outpoints.push(vtxo_outpoint);
                        }
                        // The VTXO has not been confirmed on the blockchain yet. Therefore, it
                        // cannot have expired.
                        _ => {
                            vtxo_outpoints.push(vtxo_outpoint);
                        }
                    }
                }
            }

            spendable.push((vtxo_outpoints, vtxo));
        }

        Ok(spendable)
    }

    pub async fn offchain_balance(&self) -> Result<Amount, Error> {
        let list = self.spendable_vtxos().await?;
        let sum = list
            .iter()
            .flat_map(|(vtxos, _)| vtxos)
            .fold(Amount::ZERO, |acc, x| acc + x.amount);

        Ok(sum)
    }

    // TODO: GetTransactionHistory.

    fn asp_client(&self) -> asp::Client {
        self.inner.asp_client.clone()
    }

    fn kp(&self) -> &Keypair {
        &self.inner.kp
    }

    fn secp(&self) -> &Secp256k1<All> {
        &self.inner.secp
    }

    fn blockchain(&self) -> &B {
        &self.inner.blockchain
    }
}
