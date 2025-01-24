use crate::script::csv_sig_script;
use bitcoin::key::Secp256k1;
use bitcoin::secp256k1::All;
use bitcoin::taproot::LeafVersion;
use bitcoin::taproot::TaprootBuilder;
use bitcoin::taproot::TaprootSpendInfo;
use bitcoin::ScriptBuf;
use bitcoin::XOnlyPublicKey;

/// The script of an _internal_ node of the VTXO tree. By internal node we mean a non-leaf node.
///
/// This script allows the ASP to sweep the entire output after `round_lifetime_seconds` have passed
/// from the time the output was included in a block.
pub struct VtxoTreeInternalNodeScript {
    script: ScriptBuf,
}

impl VtxoTreeInternalNodeScript {
    pub fn new(round_lifetime: bitcoin::Sequence, asp: XOnlyPublicKey) -> Self {
        let script = csv_sig_script(round_lifetime, asp);

        Self { script }
    }

    pub fn sweep_spend_leaf(
        &self,
        secp: &Secp256k1<All>,
        aggregate_pk: XOnlyPublicKey,
    ) -> TaprootSpendInfo {
        TaprootBuilder::new()
            .add_leaf_with_ver(0, self.script.clone(), LeafVersion::TapScript)
            .expect("valid sweep leaf")
            .finalize(secp, aggregate_pk)
            .expect("can be finalized")
    }
}
