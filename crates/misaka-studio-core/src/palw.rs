//! The PALW execution classes — the list a person consults before mining.
//!
//! On the MISAKA network a block is won by verified LLM inference, and *which* model you run is a
//! chain-registered **class**: an execution graph the whole network can re-derive, with an artifact
//! every panel seat checks byte-for-byte. "Can I mine, and with what?" therefore has a precise
//! answer per class, and this module is that answer as data — the UX equivalent of the model list,
//! but for participation.
//!
//! **The table is per network**, because the registry is. testnet-12 (launched 2026-09-25/26 JST
//! from release `0e8ec984e`) is the public network; its genesis card registers the floor and two
//! held rows of the same Qwen2.5-1.5B-Instruct weights (`docs/testnet12-join-mining.md` §6 and
//! `docs/testnet-12-regenesis-2026-09-23.md` "genesis の行" in the misakas repository, whose roots
//! `class_manifest_const_v1.rs` reads from the committed `.palwmanifest` sidecars):
//!
//! | class | artifact | chain model id |
//! |---|---|---|
//! | `PALW-BASE-0` | none — derived from a seed on every node | — |
//! | `PALW-QWEN25-A16-8K` | `qwen25-1.5b-a16-8k.palwart`, 1.68 GiB | `Qwen/Qwen2.5-1.5B/graph-v7@8192` |
//! | `PALW-QWEN25-A16-2M` | `qwen25-1.5b-a16-2m.palwart`, 2.7 GiB | `Qwen/Qwen2.5-1.5B/graph-v7@2097152` |
//!
//! The 512-position dense row and the Qwen3.6 hybrid are **not** testnet-12 classes: the held
//! hybrid map fails at prefill position 15 for Qwen3.6-35B-A3B, so no genesis row carries it, and
//! the 512 dense row was testnet-11's. [`TESTNET11_CLASSES`] keeps testnet-11's Relaunch 5f table
//! for a Studio still pointed there, and so that the files those classes use are still recognised
//! as artifacts (and kept out of an inference engine) whichever network is selected.
//!
//! Past ADR-0137 **share is not an input**. A block buys one unit of work from any model
//! (`CCU_m / W`). testnet-12's model rows declare the minimum grantable share (1‰) and never an
//! allocation; live share is the fraction of Final work a reader computes. `share_permille` below is
//! the genesis declaration, kept so a card can name the row.
//!
//! # What this table is, and is not
//!
//! It is a **pinned snapshot of a genesis registry**, kept here so the Studio can show the list —
//! with artifact identities a download can be verified against — before any node is running. It
//! is not the source of truth: the chain is, and a node's own startup check (`the node checks this
//! itself and refuses a mismatch`) is what finally gates production. A class registered after
//! genesis is the chain's to report; this table is a release-note edit, exactly like the
//! quantization table.
//!
//! Every hash here is copied from the runbooks and the consensus constants verbatim, with its
//! provenance named, so a mismatch is attributable.

use crate::settings::NodeNetwork;
use serde::{Deserialize, Serialize};

/// How a class's artifact comes to exist on a machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PalwArtifactSource {
    /// No file at all: the artifact is derived from a seed by every node. The floor class — the
    /// reason a machine with no GPU and no download can still produce blocks.
    DerivedFromSeed,
    /// A file published for download, verifiable against a pinned digest before use.
    Download {
        /// The name the file takes on disk — the basename of `repo_path`, and what a scan of the
        /// models directory looks for.
        filename: &'static str,
        /// Path within the repository. Not the same string as `filename` the moment an artifact
        /// lives in a subdirectory, and conflating the two downloads a 404.
        repo_path: &'static str,
        /// SHA-256 of the file itself — what the download manager verifies.
        sha256: &'static str,
        size_bytes: u64,
        /// Hugging Face repository holding it.
        hf_repo: &'static str,
        /// Rebuilding the artifact from the public weights is the alternative route, and the one
        /// that trusts nobody; named so the UI can offer both.
        convert_command: &'static str,
    },
    /// Must be converted locally from public weights — no direct download is published.
    ConvertLocally {
        /// The name the converted file takes on disk. Matched exactly, not by extension: two
        /// classes can share `.palwart` and must not see each other's file as installed.
        filename: &'static str,
        approx_size_bytes: u64,
        /// The exact size and SHA-256 of the output, where the conversion is pinned: a deterministic
        /// conversion of pinned weights lands on the same bytes on every machine, and testnet-12's
        /// 8k row publishes both. `None` where no one has measured the output yet — presence is then
        /// judged by the filename alone and the root check is the node's.
        exact: Option<PalwConvertedPin>,
        /// The public weights the conversion reads.
        source_repo: &'static str,
        convert_command: &'static str,
    },
}

/// What a pinned conversion must produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PalwConvertedPin {
    pub size_bytes: u64,
    pub sha256: &'static str,
}

impl PalwArtifactSource {
    /// The extension this artifact carries on disk, or `None` for a class that has no file.
    ///
    /// Read off the class table rather than written out again. Three places have to agree about
    /// what an artifact looks like — the model scan, the network tab's artifact scan, and the gate
    /// that keeps one out of an inference engine — and a list spelled three times is a list that a
    /// new class updates in one of them.
    pub fn file_extension(&self) -> Option<&'static str> {
        let filename = match self {
            PalwArtifactSource::DerivedFromSeed => return None,
            PalwArtifactSource::ConvertLocally { filename, .. } | PalwArtifactSource::Download { filename, .. } => *filename,
        };
        // `rfind`, not `find`: every artifact filename in the table carries a version dot
        // (`qwen2.5-…`), and the first dot would name `.5-1` as the extension.
        filename.rfind('.').map(|dot| &filename[dot..])
    }
}

/// Whether a file in the models directory is a PALW execution artifact — something a class is
/// produced with — rather than a model an inference engine can load.
///
/// The two live in one directory on purpose, so this is the question that separates them. It is
/// asked by name, not by content: a scan that opened every file to read a magic number would read
/// 34 GiB of artifact to list a directory.
///
/// A partially downloaded artifact (`qwen36.palwq36.part`) is deliberately **not** one: it is not
/// an artifact until the download finishes, and offering half a file is offering a failure.
pub fn is_artifact_filename(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    known_classes().filter_map(|class| class.artifact.file_extension()).any(|ext| lower.ends_with(ext))
}

/// Every class any table here knows, the public network's first.
///
/// For the questions that are about a FILE rather than a network — is this an artifact, which
/// class's tokenizer goes with it. A `qwen36.palwq36` left in the models directory is still an
/// artifact on testnet-12, and handing it to `llama-server` is the same abort it always was.
fn known_classes() -> impl Iterator<Item = &'static PalwClassSpec> {
    TESTNET12_CLASSES.iter().chain(TESTNET11_CLASSES.iter())
}

/// The class table of `network`.
///
/// A local devnet or simnet mints its own registry; the public network's table is the closest
/// thing this build knows to what such a chain carries, and the Network tab says the table is the
/// snapshot, not the node's (the node's own dump replaces it once one is connected).
pub fn classes_for(network: NodeNetwork) -> &'static [PalwClassSpec] {
    match network {
        NodeNetwork::Testnet11 => TESTNET11_CLASSES,
        NodeNetwork::Testnet12 | NodeNetwork::Devnet | NodeNetwork::Simnet => TESTNET12_CLASSES,
    }
}

/// One chain-registered execution class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PalwClassSpec {
    /// The name operators know it by.
    pub name: &'static str,
    pub description: &'static str,
    /// Genesis-table leftover (Relaunch 5f permille). Past ADR-0137 this is **not** a lottery
    /// input and **not** an epoch budget: live share is the fraction of Final work a reader
    /// computes (`share_m = Σ Finals of m of W / Σ all Finals of W`). Zero for a post-genesis
    /// class; the chain reports the live number, not this field.
    pub share_permille: u16,
    /// The class id (`shape_profile_id()` over the execution graph), 128 hex chars where the
    /// docs publish it in full; a documented prefix otherwise. Display, never verification —
    /// verification is the artifact root, and the node performs it.
    pub class_id_hex: &'static str,
    /// Whether `class_id_hex` is the complete id or a documented prefix.
    pub class_id_complete: bool,
    /// The artifact root the chain registers (`Base0ArtifactV1::artifact_digest()`), 128 hex.
    /// What `--root-only` must print for the artifact to be the registered class.
    pub artifact_root_hex: &'static str,
    pub artifact: PalwArtifactSource,
    /// The floor: default when no class is named. Past ADR-0137 it fills whatever cadence the
    /// models leave and is paid nothing for it — residual liveness, not an emission grant.
    pub is_base: bool,
    /// **The context the class was registered at**, in tokens: the `n_ctx` inside its profile.
    ///
    /// A registration choice, not a property of the weights — the node says so in as many words
    /// ("a class is not a function of an artifact: `n_ctx`, `tile_len` and `n_threads` are
    /// registration choices that no weight file contains"). One model can therefore be registered
    /// at 12, 512 or 2M positions as separate classes, and the number decides what a job can hold.
    /// Pinned beside the id it belongs to; the node does not yet publish it over RPC.
    pub context_tokens: u32,
    /// The tokenizer the class's ids mean, where one is published beside the artifact.
    ///
    /// The Studio counts a prompt with it, so that a 512-token class is budgeted in the tokens the
    /// worker will count and not in an estimate of them. Pinned by SHA-256 like the artifact: the
    /// published A16 artifact declares no tokenizer commitment of its own (the 64 bytes after its
    /// rotary table are zero — read 2026-09-17), so the digest here is the only binding there is.
    pub tokenizer: Option<PalwTokenizerPin>,
    /// **The memory one node needs to serve the class**, where it has been measured: a full-seat
    /// replay or an attempt, whichever is larger. `0` where the artifact's own size is the only
    /// figure — the memory note then compares the artifact with the machine, as it always did.
    ///
    /// Separate from the artifact size because at 8k they part company: a 1.68 GiB artifact needs
    /// ≈ 3.37 GiB for a full-seat replay (artifact + 1.67 GiB of trace scratch) and an 8k producer
    /// peaked at 3.67 GiB RSS in the drill (`docs/testnet12-join-mining.md` §6).
    pub min_memory_bytes: u64,
}

/// A class's `tokenizer.json`, published in the class's repository and pinned by digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PalwTokenizerPin {
    pub hf_repo: &'static str,
    pub repo_path: &'static str,
    pub sha256: &'static str,
    pub size_bytes: u64,
}

impl PalwClassSpec {
    /// The name the class's tokenizer takes on disk: beside the artifact, prefixed with the
    /// artifact's stem, so two classes' tokenizers in one models directory do not collide.
    pub fn tokenizer_filename(&self) -> Option<String> {
        self.tokenizer?;
        match &self.artifact {
            PalwArtifactSource::Download { filename, .. } | PalwArtifactSource::ConvertLocally { filename, .. } => {
                let stem = filename.rfind('.').map(|dot| &filename[..dot]).unwrap_or(filename);
                Some(format!("{stem}.tokenizer.json"))
            }
            _ => None,
        }
    }
}

/// The class whose published artifact has this file name, if any.
pub fn class_for_artifact_filename(file_name: &str) -> Option<&'static PalwClassSpec> {
    known_classes().find(|class| match class.artifact {
        PalwArtifactSource::Download { filename, .. } | PalwArtifactSource::ConvertLocally { filename, .. } => filename == file_name,
        PalwArtifactSource::DerivedFromSeed => false,
    })
}

/// GiB, binary.
const GIB: u64 = 1 << 30;

/// **How long a block's pay sits before it can be spent, in DAA**, on the public testnets.
///
/// A coinbase output matures one block after acceptance and then waits out the settlement window
/// the network runs (600 DAA), so a reward is spendable at `block DAA + 601`. testnet-12 keeps the
/// 600: its DNS finality is in Bootstrap until validators are funded, and until then "a coinbase
/// matures on the 600-DAA fallback alone (about 20 hours)" (`docs/testnet12-join-mining.md` §4).
/// testnet-11 ran the same window. The node does not publish it over RPC — it is a consensus
/// parameter — so it is pinned here beside the class table, with the same rule: if the network
/// changes it, this is the line that must change. It is used only to SAY when a reward becomes
/// spendable; nothing in the app spends.
pub const COINBASE_MATURITY_DAA: u64 = 601;

/// The Qwen2.5-1.5B-Instruct tokenizer, as the A16 artifacts' repository publishes it.
///
/// Byte-identical to `Qwen/Qwen2.5-1.5B-Instruct`'s own `tokenizer.json` (compared 2026-09-17).
/// Every dense row converts those weights, so every dense row counts in these ids.
const QWEN25_TOKENIZER: PalwTokenizerPin = PalwTokenizerPin {
    hf_repo: "Misakachain/Qwen2.5-1.5B-PALW-A16-runtime",
    repo_path: "tokenizer.json",
    sha256: "c0382117ea329cdf097041132f6d735924b697924d6f6fc3945713e96ce87539",
    size_bytes: 7_031_645,
};

/// The floor, as every network since testnet-11 Relaunch 5f registers it.
///
/// testnet-12 derives the same artifact in-process (`palw_rc_base0_artifact_root_v1`) and pins the
/// same root (`PALW_RC_GENESIS_ARTIFACT_ROOT`, `bcf2d9eb…`), so the id is the same too.
const PALW_BASE_0: PalwClassSpec = PalwClassSpec {
    name: "PALW-BASE-0",
    description: "The deterministic integer floor. Its artifact is derived from a seed on every node — no GGUF, \
                  no download, no GPU. Past ADR-0137 it fills whatever cadence the model classes leave and is \
                  paid nothing for it. The default class when none is named.",
    share_permille: 22,
    // The id and root the public node reports through `getPalwProducerFacts`;
    // `palw-class ledger` prints the same.
    class_id_hex: "f1c5635c6e47e96e7af864789c94523335dc56584af297cb8cc19021c228b897bee1a50145597e45f8ca2727349bf4aa352a98cc05274b7f059a176642f623c8",
    class_id_complete: true,
    artifact_root_hex: "bcf2d9eb7357bd6c267df2df6588393ca71c67d7c802903ca7031948303c793dcb78bfe26488f52d0393be08e0cc0777b080e2dce9355d3576036b734545b8df",
    artifact: PalwArtifactSource::DerivedFromSeed,
    is_base: true,
    // `PALW_RC_BASE0_GEOMETRY.n_ctx`: sized by the court's cost ceiling, not by capability.
    context_tokens: 12,
    tokenizer: None,
    min_memory_bytes: 0,
};

/// The testnet-12 genesis classes (release `0e8ec984e`, genesis `a27f8f44…`).
///
/// Order is the order a newcomer should read them in: the one that needs nothing first, then the
/// row a laptop can actually serve, then the one it cannot.
pub const TESTNET12_CLASSES: &[PalwClassSpec] = &[
    PALW_BASE_0,
    PalwClassSpec {
        name: "PALW-QWEN25-A16-8K",
        description: "Qwen2.5-1.5B-Instruct, W8A16, at 8,192 tokens — the held-context row testnet-12 actually runs, \
                      chain model id Qwen/Qwen2.5-1.5B/graph-v7@8192. An 8k attempt prefills 1,023 positions in about \
                      290 s; a full-seat replay needs ≈ 3.37 GiB. The published artifact is the conversion of the \
                      public Instruct weights (qwen25-convert --n-ctx 8192); the conversion is deterministic, so \
                      rebuilding it yourself lands on the same bytes and the same registered inventory root, which is \
                      the only reason downloading it is safe.",
        // A genesis model row declares the minimum grantable share and never an allocation
        // (`t12_regenesis.rs`: "block rights come from verified work").
        share_permille: 1,
        // `class_manifest_const_v1.rs` (misakas): `class_id_of_class(QWEN25_A16_8K_MANIFEST_V1, 1)`.
        class_id_hex: "ebf44d0aa09ff7d1310a7855ab4005c275cdce557e32c269b0f3a984ea80ca73\
                       ad1ea0c9b1c0539c8ae04abb5fe24399e67e05bb0895a3dee82253e772246d01",
        class_id_complete: true,
        // The INVENTORY root the genesis registers, read from the committed sidecar — not the
        // flat artifact digest `f4af38d9…` the same sidecar also names.
        artifact_root_hex: "88096dc177826d880c1c5fca4ec93cffe5ab51af108ed169a8e03cd4726308f9\
                            1263f79f81904b043327bfa277e3558b1656f14259f6dd33603a9f91871aae20",
        artifact: PalwArtifactSource::Download {
            filename: "qwen25-1.5b-a16-8k.palwart",
            // Published 2026-09-26 beside the 512 and 2M files, copied from the fleet's own file
            // (ibm `/root/palw-class/`), whose SHA-256 is the one the deploy kit pins.
            repo_path: "palw-runtime/qwen25-1.5b-a16-8k.palwart",
            // `contrib/t12-deploy-kit/fleet.env.example` (misakas): ART_8K_SHA256 / ART_8K_BYTES,
            // pinned to the build by `t12_deploy_kit_constants.rs`.
            sha256: "b73600cfeef3f54fd6e9f6a831c504588aa3b20ea2506ae824799206201e8ac8",
            size_bytes: 1_799_359_436,
            hf_repo: "Misakachain/Qwen2.5-1.5B-PALW-A16-runtime",
            // The join guide's two steps: the conversion, then the sidecar the node reads the
            // root from (without it the node derives the root itself at startup).
            convert_command: "qwen25-convert /path/to/Qwen2.5-1.5B-Instruct --a16 --n-ctx 8192 --out qwen25-1.5b-a16-8k.palwart \
                              && palw-class manifest --network testnet-12 qwen25-1.5b-a16-8k.palwart",
        },
        is_base: false,
        context_tokens: 8_192,
        tokenizer: Some(QWEN25_TOKENIZER),
        // The drill's producer peak (3.67 GiB RSS); a seat needs 3.37 GiB, and the join guide asks
        // for a 3.5 GiB share at the least.
        min_memory_bytes: 3_940_634_542,
    },
    PalwClassSpec {
        name: "PALW-QWEN25-A16-2M",
        description: "Qwen2.5-1.5B-Instruct, W8A16, at 2,097,152 tokens — the held-context row (ADR-0103), chain \
                      model id Qwen/Qwen2.5-1.5B/graph-v7@2097152. Same Instruct weights as the 8k row; a different \
                      graph and a 2M rotary table. An attempt holds ≈ 11.6 GiB and takes about a week of CPU on a \
                      fleet host, and the row is closed to economic Final at launch — listed so the registry reads \
                      whole, not as something a desktop mines.",
        share_permille: 1,
        // `class_id_of_class(QWEN25_A16_2M_MANIFEST_V1, 1)`.
        class_id_hex: "74c67e63d9c03daa05880c5d8a47b354ca20e952b1a2d49c107abe14f890a9c5\
                       0790371bb715c7cea33ae8ac9213a3a63da409070cb2c98b8e861598db902f7a",
        class_id_complete: true,
        // The inventory root testnet-12 registers (`inventory_root_of_class`). The container
        // digest `b5baca63…` is what the FIRST t12 deployment pinned by mistake — root and digest
        // are different values of the same file, and only the root opens the class.
        artifact_root_hex: "f63af2c46b3816f6a16c168a130d28ec95b0ca5107da76d4abe24e4d9396e65c\
                            80d6d83014c47855e2394af20ce3333a59359fd3eb06090fdc7bfd502f75c7c2",
        artifact: PalwArtifactSource::Download {
            filename: "qwen25-1.5b-a16-2m.palwart",
            repo_path: "palw-runtime/qwen25-1.5b-a16-2m.palwart",
            // docs/qwen25-a16-2m-held-artifact.md (misakas) — file SHA-256, not the container digest.
            sha256: "35d41da6272010035894d76bbd0c17edfb76f20a6b6625063e639ccd06739bf3",
            size_bytes: 2_868_906_956,
            hf_repo: "Misakachain/Qwen2.5-1.5B-PALW-A16-runtime",
            convert_command: "qwen25-convert /path/to/Qwen2.5-1.5B-Instruct --a16 --n-ctx 2097152 --out qwen25-1.5b-a16-2m.palwart",
        },
        is_base: false,
        context_tokens: 2_097_152,
        tokenizer: Some(QWEN25_TOKENIZER),
        // ~11.6 GiB an attempt under A16-KV-i16 (kaspad's own `--palw-host-memory-share` help).
        min_memory_bytes: 12_455_405_158,
    },
];

/// The testnet-11 Relaunch 5f classes (genesis `ad30b5cb…`), kept for a Studio still pointed at
/// testnet-11 and for recognising those classes' files. A build of current misakas `main` no longer
/// joins testnet-11; its operators build `1f98d3bf4`.
pub const TESTNET11_CLASSES: &[PalwClassSpec] = &[
    PALW_BASE_0,
    PalwClassSpec {
        name: "PALW-QWEN25-A16",
        description: "Qwen2.5-1.5B-Instruct, W8A16 static-PTQ — the dense tier, registered on Relaunch 5f as the \
                      chain model id Qwen/Qwen2.5-1.5B/graph-v5@512 (a 512-token context, canonical job prefill 63 / \
                      decode 2). The published artifact is the conversion of the public Instruct weights; rebuilding \
                      it yourself lands on the same inventory root, which is the only reason downloading it is safe.",
        share_permille: 489,
        class_id_hex: "4277d84f7d91528cc04aa366d51ee1c2e4f7902c4f6b16a213dead1c7e227977db732f18ed6183db3d944d44726ebd3feff7b15c48f9dba11cd526684f35f1b7",
        class_id_complete: true,
        // The chain pins the artifact's INVENTORY root (what `getPalwProducerFacts.artifactRoot`
        // reports), not the file's artifact digest (`c00faa48…`, printed in the repository card).
        // Both name the same 1,795,427,276-byte file.
        artifact_root_hex: "1a7457f100d9fb0f3406d882b4b5bcd7e2ebcccd54edc5268a08c3a85bc6c8d3adacdf345cde3cb72ffe8ed7fe7a2f729d10f00821f94b1e8562e4e217b72708",
        artifact: PalwArtifactSource::Download {
            filename: "qwen25-1.5b-a16.palwart",
            repo_path: "palw-runtime/qwen25-1.5b-a16.palwart",
            // The repository's LFS object id, which *is* the file's SHA-256.
            sha256: "a8c4e53e5b30dd0d4dc6ef791e0513890a07a2b3a22d045e612536bba1240b1f",
            size_bytes: 1_795_427_276,
            hf_repo: "Misakachain/Qwen2.5-1.5B-PALW-A16-runtime",
            convert_command: "qwen25-convert /path/to/Qwen2.5-1.5B-Instruct --a16 --out qwen25-1.5b-a16.palwart",
        },
        is_base: false,
        // The `@512` in its chain model id, graph-v5@512.
        context_tokens: 512,
        tokenizer: Some(QWEN25_TOKENIZER),
        min_memory_bytes: 0,
    },
    PalwClassSpec {
        name: "QWEN36",
        description: "Qwen3.6-abliterated-35B-A3B under the hybrid integer runtime. The artifact is a 34 GiB \
                      conversion of the Q4_K_M GGUF — downloadable, or reproducible from the source GGUF; every \
                      route lands on the same registered root or the node refuses it.",
        share_permille: 489,
        // Printed in full in docs/testnet11-join-mining.md §6c.
        // Relaunch 5f registers the corrected graph (chain model id Qwen3.6-35B-A3B/graph-v3); the
        // earlier ec7bbcbf… row is not on this chain.
        class_id_hex: "5bd9ae3d91df80650caffe3126a38bafb0b4feb9b046a416d353a7c3f71af6eab5aadf9b1ce41650007a980f1cc6044ef218424f4cbb8299ef9e92c97b99ef8e",
        class_id_complete: true,
        // PALW_RC_GENESIS_QWEN36_ARTIFACT_ROOT — what `qwen36-run --root-only` must print.
        artifact_root_hex: "f4aad4fd543928eb2d3a737555b09da9bf685fc515c0f8d4520988efcffacf08\
                            13d1b727537f0d03d349253aa11ef427e4047c2166b69fd7edb46a4a9984b368",
        artifact: PalwArtifactSource::Download {
            filename: "qwen36.palwq36",
            repo_path: "qwen36.palwq36",
            sha256: "7a944595a4256ab0aa4ca8b59f39fea268654b3630e54fb354cf1fa7658cf08c",
            size_bytes: 36_492_831_232,
            hf_repo: "Misakachain/Qwen3.6-35B-A3B-PALW-runtime",
            convert_command: "qwen36-convert --url <gguf url> --header header.bin --out qwen36.palwq36 --context 512",
        },
        is_base: false,
        // The genesis hybrid row, `palw_qwen36_context_row_profile_v1(512)`.
        context_tokens: 512,
        tokenizer: None,
        min_memory_bytes: 0,
    },
    PalwClassSpec {
        name: "PALW-QWEN25-A16-2M",
        description: "Qwen2.5-1.5B-Instruct, W8A16, at 2,097,152 tokens — the held-context row (ADR-0103), chain \
                      model id Qwen/Qwen2.5-1.5B/graph-v7@2097152. Same Instruct weights as PALW-QWEN25-A16; a \
                      different graph and a 2M rotary table, so the 512-position artifact cannot serve it. The \
                      published artifact is the conversion of the public Instruct weights; rebuilding it yourself \
                      lands on the same container digest, which is the only reason downloading it is safe.",
        // Post-genesis: no genesis grant. Live share is Final work (ADR-0137 D7).
        share_permille: 0,
        // docs/qwen25-a16-2m-held-artifact.md (misakas): the canonical graph-v7 profile id.
        class_id_hex: "74c67e63d9c03daa05880c5d8a47b354ca20e952b1a2d49c107abe14f890a9c5\
                       0790371bb715c7cea33ae8ac9213a3a63da409070cb2c98b8e861598db902f7a",
        class_id_complete: true,
        // PALW container digest of the 2M artifact, same document — not the 512 inventory root.
        artifact_root_hex: "b5baca6364135a62bd4512a58c2ca747373019a495505968d884b2c0e52e4ce9\
                            322a8af0db90d3ec1001c15b5a89fe0e8008519e55a5f3f865e15562fb8967ae",
        artifact: PalwArtifactSource::Download {
            filename: "qwen25-1.5b-a16-2m.palwart",
            repo_path: "palw-runtime/qwen25-1.5b-a16-2m.palwart",
            // docs/qwen25-a16-2m-held-artifact.md (misakas) — file SHA-256, not the container digest.
            sha256: "35d41da6272010035894d76bbd0c17edfb76f20a6b6625063e639ccd06739bf3",
            size_bytes: 2_868_906_956,
            hf_repo: "Misakachain/Qwen2.5-1.5B-PALW-A16-runtime",
            convert_command: "qwen25-convert /path/to/Qwen2.5-1.5B-Instruct --a16 --n-ctx 2097152 --out qwen25-1.5b-a16-2m.palwart",
        },
        is_base: false,
        context_tokens: 2_097_152,
        tokenizer: Some(QWEN25_TOKENIZER),
        min_memory_bytes: 0,
    },
];

/// The class the Studio treats as "the model class" on testnet-12: the one it points the node at
/// when the file is present, and badges in the lists.
///
/// **Not the chain's default.** That is the floor: no file, no download, and what a node mines
/// when no class is named. This is the Studio's answer to a different question — of the classes
/// that actually run a model, which one can a desktop hold? testnet-12 registers two, and the 2M
/// row needs ≈ 11.6 GiB and a week of CPU per attempt. That leaves the 8k row, at 1.68 GiB.
///
/// It is a single verified download from the A16 repository (published 2026-09-26), so the
/// first-run install (`install_default_class_artifact`) can fetch it.
pub const DEFAULT_CLASS: &str = "PALW-QWEN25-A16-8K";

/// testnet-11's equivalent: the 512-position dense row, which did publish a download.
pub const TESTNET11_DEFAULT_CLASS: &str = "PALW-QWEN25-A16";

/// The spec [`DEFAULT_CLASS`] names.
///
/// Panics if the registry no longer carries that name, which is a build that should not have
/// linked rather than a condition to handle at runtime; the test below is what holds it.
pub fn default_class() -> &'static PalwClassSpec {
    default_class_for(NodeNetwork::Testnet12)
}

/// The default model class of `network`'s table.
pub fn default_class_for(network: NodeNetwork) -> &'static PalwClassSpec {
    let name = match network {
        NodeNetwork::Testnet11 => TESTNET11_DEFAULT_CLASS,
        _ => DEFAULT_CLASS,
    };
    classes_for(network).iter().find(|class| class.name == name).expect("the default names a class of its network's table")
}

/// The exact size a class's artifact file must have, where one is pinned: a published download
/// always, a conversion when its output has been measured.
pub fn exact_artifact_size(spec: &PalwClassSpec) -> Option<u64> {
    match &spec.artifact {
        PalwArtifactSource::Download { size_bytes, .. } => Some(*size_bytes),
        PalwArtifactSource::ConvertLocally { exact, .. } => exact.map(|pin| pin.size_bytes),
        PalwArtifactSource::DerivedFromSeed => None,
    }
}

/// **What a class artifact's own header says about the model inside it.**
///
/// A BASE-0-family container (`PALWB0A1`, `PALWB0A2` — the dense A16 tier included) opens with its
/// shape, eight little-endian `u64`s straight after the magic, in the order
/// `misaka-palw-base0::artifact::decode_artifact_file_v1` reads them. Reading those is 64 bytes,
/// not 1.7 GiB, and every field is something the file itself carries — unlike a class's context,
/// which is the registration's.
///
/// `max_position` is the length of the rotary table: the most positions this file can ever be run
/// at. A class registered wider than that cannot be served by this file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PalwArtifactHeader {
    pub n_layers: u64,
    pub n_heads: u64,
    pub n_kv_heads: u64,
    pub d_head: u64,
    pub d_ff: u64,
    pub vocab: u64,
    pub max_position: u64,
}

/// The magics whose shape block this reads. Other artifact formats (`PALWQ361`, the QWEN36
/// hybrid) lay their headers out differently and are left unread rather than misread.
const BASE0_MAGICS: [&[u8; 8]; 2] = [b"PALWB0A2", b"PALWB0A1"];

/// Parse the shape block from the first bytes of an artifact. `None` for another format, a short
/// read, or a shape the runtime itself would refuse — a number from a file that is not what it
/// claims is worse than no number.
pub fn parse_artifact_header(bytes: &[u8]) -> Option<PalwArtifactHeader> {
    let magic = bytes.get(..8)?;
    if !BASE0_MAGICS.iter().any(|m| m.as_slice() == magic) {
        return None;
    }
    let field = |i: usize| -> Option<u64> { Some(u64::from_le_bytes(bytes.get(8 + 8 * i..16 + 8 * i)?.try_into().ok()?)) };
    let header = PalwArtifactHeader {
        n_layers: field(0)?,
        n_heads: field(1)?,
        n_kv_heads: field(2)?,
        d_head: field(3)?,
        d_ff: field(4)?,
        vocab: field(5)?,
        max_position: field(6)?,
    };
    // The runtime's own `Base0ShapeV1::validate`, plus bounds no real model comes near: a header
    // that decodes to 2^60 layers is not a model.
    let sane = header.n_layers > 0
        && header.n_layers <= 4096
        && header.n_heads > 0
        && header.n_kv_heads > 0
        && header.n_kv_heads <= header.n_heads
        && header.n_heads.is_multiple_of(header.n_kv_heads)
        && header.d_head > 0
        && header.d_head.is_multiple_of(2)
        && header.d_ff > 0
        && header.vocab > 0
        && header.max_position > 0
        && header.max_position <= 1 << 40;
    sane.then_some(header)
}

/// Read the header of the artifact at `path`: the first 64 bytes, nothing more.
pub fn read_artifact_header(path: &std::path::Path) -> Option<PalwArtifactHeader> {
    use std::io::Read;
    let mut buf = [0u8; 64];
    let mut file = std::fs::File::open(path).ok()?;
    file.read_exact(&mut buf).ok()?;
    parse_artifact_header(&buf)
}

/// Whether this machine holds a class's artifact, and whether it plausibly can run it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PalwClassReadiness {
    /// Nothing to obtain: the node derives the artifact itself.
    ReadyBuiltIn,
    /// The artifact file is present. `verified` is true only when its SHA-256 has been computed
    /// and matches the pin — presence alone is a filename, not an identity, and the field says
    /// which of the two the UI is showing.
    ArtifactPresent { path: String, size_bytes: u64, verified: bool },
    /// Not on disk. `downloadable` distinguishes "click to download" from "convert locally".
    ArtifactMissing { downloadable: bool },
    /// On disk but the wrong size — a truncated download or a different conversion. Named
    /// separately from Missing because the remedy differs: delete or re-verify, don't re-download
    /// beside it.
    ArtifactMismatch { path: String, size_bytes: u64, expected_bytes: u64 },
}

/// One class, assessed for this machine.
#[derive(Clone, Debug, Serialize)]
pub struct PalwClassStatus {
    pub spec: PalwClassSpec,
    pub readiness: PalwClassReadiness,
    /// A one-line memory note when the artifact is bigger than this machine's RAM — honest
    /// arithmetic (the hybrid runtime maps the artifact), not a benchmark.
    pub memory_note: Option<String>,
    /// The header of the file on disk, when there is one and its format is one this build reads.
    /// Its `max_position` is how many positions the file's rotary table covers — an upper bound on
    /// any class that runs it, and a fact about THIS file rather than about the registration.
    pub artifact_header: Option<PalwArtifactHeader>,
}

/// Assess every class of `network` against a directory scan and the machine.
///
/// `artifact_files` is (path, file name, size) for candidate artifact files — the caller scans
/// its models directory (and the node's app dir if it knows one); this stays pure so it is
/// testable without a filesystem.
pub fn assess_classes(network: NodeNetwork, artifact_files: &[(String, String, u64)], total_memory: u64) -> Vec<PalwClassStatus> {
    assess(classes_for(network), artifact_files, total_memory)
}

/// The same, against an arbitrary class list.
///
/// Separate from [`assess_classes`] so a registry snapshot is not the only input this logic can
/// ever be shown: an unpinned conversion has no live row today, and without this that branch
/// below would be code no test can reach.
pub fn assess(classes: &[PalwClassSpec], artifact_files: &[(String, String, u64)], total_memory: u64) -> Vec<PalwClassStatus> {
    classes
        .iter()
        .map(|spec| {
            let readiness = match &spec.artifact {
                PalwArtifactSource::DerivedFromSeed => PalwClassReadiness::ReadyBuiltIn,
                PalwArtifactSource::Download { filename, size_bytes, .. } => {
                    match artifact_files.iter().find(|(_, name, _)| name == filename) {
                        Some((path, _, size)) if size == size_bytes => {
                            PalwClassReadiness::ArtifactPresent { path: path.clone(), size_bytes: *size, verified: false }
                        }
                        Some((path, _, size)) => {
                            PalwClassReadiness::ArtifactMismatch { path: path.clone(), size_bytes: *size, expected_bytes: *size_bytes }
                        }
                        None => PalwClassReadiness::ArtifactMissing { downloadable: true },
                    }
                }
                PalwArtifactSource::ConvertLocally { filename, exact, .. } => {
                    match artifact_files.iter().find(|(_, name, _)| name == filename) {
                        // A pinned conversion has one right size, and a file of another is a
                        // different conversion (or an unfinished one) the node would refuse.
                        Some((path, _, size)) if exact.is_some_and(|pin| pin.size_bytes != *size) => {
                            PalwClassReadiness::ArtifactMismatch {
                                path: path.clone(),
                                size_bytes: *size,
                                expected_bytes: exact.map_or(0, |pin| pin.size_bytes),
                            }
                        }
                        // Unpinned, a conversion's byte size varies with its input, so presence
                        // is judged by the exact filename and the root check is the node's.
                        Some((path, _, size)) => {
                            PalwClassReadiness::ArtifactPresent { path: path.clone(), size_bytes: *size, verified: false }
                        }
                        None => PalwClassReadiness::ArtifactMissing { downloadable: false },
                    }
                }
            };

            let artifact_bytes = match &spec.artifact {
                PalwArtifactSource::DerivedFromSeed => 0,
                PalwArtifactSource::Download { size_bytes, .. } => *size_bytes,
                PalwArtifactSource::ConvertLocally { approx_size_bytes, .. } => *approx_size_bytes,
            };
            let memory_note = if spec.min_memory_bytes > total_memory {
                Some(format!(
                    "serving it needs about {:.1} GiB (a full-seat replay or an attempt) against {:.1} GiB of RAM — \
                     this machine cannot run this class",
                    spec.min_memory_bytes as f64 / GIB as f64,
                    total_memory as f64 / GIB as f64
                ))
            } else {
                (artifact_bytes > total_memory).then(|| {
                    format!(
                        "the artifact is {:.1} GiB against {:.1} GiB of RAM — this machine cannot run this class",
                        artifact_bytes as f64 / GIB as f64,
                        total_memory as f64 / GIB as f64
                    )
                })
            };

            PalwClassStatus { spec: spec.clone(), readiness, memory_note, artifact_header: None }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rows_are_well_formed(classes: &[PalwClassSpec]) {
        let base: Vec<_> = classes.iter().filter(|c| c.is_base).collect();
        assert_eq!(base.len(), 1, "exactly one floor");
        assert_eq!(base[0].name, "PALW-BASE-0");
        assert!(matches!(base[0].artifact, PalwArtifactSource::DerivedFromSeed));

        for class in classes {
            // A complete Hash64 is 128 hex chars; anything else must say it is a prefix.
            if class.class_id_complete {
                assert_eq!(class.class_id_hex.len(), 128, "{}", class.name);
            }
            if !class.artifact_root_hex.is_empty() {
                assert_eq!(class.artifact_root_hex.len(), 128, "{}", class.name);
            }
            // A repository path that does not end in the name the file takes on disk is two
            // separate failures at once: the download 404s, or it lands under a name the
            // directory scan will never recognise and the class reads as missing forever.
            if let PalwArtifactSource::Download { filename, repo_path, sha256, .. } = &class.artifact {
                assert!(repo_path.ends_with(filename), "{}: {repo_path} does not end with {filename}", class.name);
                assert_eq!(sha256.len(), 64, "{}", class.name);
            }
            if let PalwArtifactSource::ConvertLocally { exact: Some(pin), approx_size_bytes, .. } = &class.artifact {
                assert_eq!(pin.sha256.len(), 64, "{}", class.name);
                assert_eq!(pin.size_bytes, *approx_size_bytes, "{}: a pinned size is the size", class.name);
            }
        }
    }

    #[test]
    fn the_testnet11_snapshot_is_internally_consistent() {
        // Relaunch 5f (2026-09-03) seats three classes at genesis: the floor, graph-v5@512 and graph-v3.
        // The 2M held-context row is catalogued beside them (graph-v7@2097152) with no genesis grant.
        assert_eq!(TESTNET11_CLASSES.len(), 4);
        let genesis: u16 = TESTNET11_CLASSES.iter().filter(|c| c.share_permille > 0).map(|c| c.share_permille).sum();
        assert_eq!(genesis, 1000, "genesis shares are permille of the whole emission");
        assert_rows_are_well_formed(TESTNET11_CLASSES);
    }

    /// testnet-12's genesis card: the floor and the two held dense rows, with the ids and INVENTORY
    /// roots `class_manifest_const_v1.rs` reads from the committed sidecars (misakas
    /// `the_committed_8k_manifest_parses_to_what_it_says` / `the_committed_manifest_parses_to_what_it_says`).
    #[test]
    fn the_testnet12_snapshot_is_the_genesis_card() {
        let names: Vec<_> = TESTNET12_CLASSES.iter().map(|c| c.name).collect();
        assert_eq!(names, ["PALW-BASE-0", "PALW-QWEN25-A16-8K", "PALW-QWEN25-A16-2M"]);
        assert_rows_are_well_formed(TESTNET12_CLASSES);

        let by_name = |n: &str| TESTNET12_CLASSES.iter().find(|c| c.name == n).unwrap();
        let eight = by_name("PALW-QWEN25-A16-8K");
        assert!(eight.class_id_hex.starts_with("ebf44d0aa09ff7d1"));
        assert!(eight.artifact_root_hex.starts_with("88096dc177826d88"));
        assert_eq!(eight.context_tokens, 8_192);
        let two = by_name("PALW-QWEN25-A16-2M");
        assert!(two.class_id_hex.starts_with("74c67e63d9c03daa"));
        // The root, never the container digest the first t12 deployment pinned by mistake.
        assert!(two.artifact_root_hex.starts_with("f63af2c46b3816f6"));
        assert!(!two.artifact_root_hex.starts_with("b5baca63"));
        for row in [eight, two] {
            assert_eq!(row.share_permille, 1, "{}: a model row declares the minimum grantable share", row.name);
            assert_eq!(row.tokenizer, Some(QWEN25_TOKENIZER), "{}: the Instruct weights' tokenizer", row.name);
        }
        // Neither the 512 dense row nor a hybrid row is on this chain.
        assert!(TESTNET12_CLASSES.iter().all(|c| c.name != "PALW-QWEN25-A16" && !c.name.starts_with("QWEN36")));
    }

    /// The 8k row is converted, and its output is pinned: the file the node accepts has one size.
    #[test]
    fn the_8k_row_is_a_verified_download_and_a_reproducible_conversion() {
        let eight = class_for_artifact_filename("qwen25-1.5b-a16-8k.palwart").expect("the 8k class");
        assert_eq!(eight.name, "PALW-QWEN25-A16-8K");
        assert_eq!(eight.tokenizer_filename().as_deref(), Some("qwen25-1.5b-a16-8k.tokenizer.json"));
        match &eight.artifact {
            PalwArtifactSource::Download { size_bytes, sha256, repo_path, hf_repo, convert_command, .. } => {
                assert_eq!(*size_bytes, 1_799_359_436);
                assert!(sha256.starts_with("b73600cf"));
                assert_eq!(*repo_path, "palw-runtime/qwen25-1.5b-a16-8k.palwart");
                assert_eq!(*hf_repo, "Misakachain/Qwen2.5-1.5B-PALW-A16-runtime");
                assert!(convert_command.contains("--n-ctx 8192"), "{convert_command}");
                assert!(convert_command.contains("palw-class manifest --network testnet-12"), "{convert_command}");
            }
            other => panic!("expected a download, got {other:?}"),
        }
        let at = |size: u64| {
            let files = vec![("/m/qwen25-1.5b-a16-8k.palwart".to_string(), "qwen25-1.5b-a16-8k.palwart".to_string(), size)];
            assess_classes(NodeNetwork::Testnet12, &files, 64 << 30).into_iter().find(|s| s.spec.name == eight.name).unwrap().readiness
        };
        assert!(matches!(at(1_799_359_436), PalwClassReadiness::ArtifactPresent { verified: false, .. }));
        assert!(matches!(at(1_000), PalwClassReadiness::ArtifactMismatch { expected_bytes: 1_799_359_436, .. }));
        let missing = assess_classes(NodeNetwork::Testnet12, &[], 64 << 30);
        assert_eq!(
            missing.iter().find(|s| s.spec.name == eight.name).unwrap().readiness,
            PalwClassReadiness::ArtifactMissing { downloadable: true }
        );
    }

    /// A conversion whose output is pinned has one right size, like a download — kept reachable by
    /// a synthetic row now that the 8k row publishes its file.
    #[test]
    fn a_pinned_conversion_of_another_size_is_a_mismatch() {
        const ONLY: &[PalwClassSpec] = &[PalwClassSpec {
            name: "PINNED",
            description: "",
            share_permille: 1,
            class_id_hex: "",
            class_id_complete: false,
            artifact_root_hex: "",
            artifact: PalwArtifactSource::ConvertLocally {
                filename: "out.palwart",
                approx_size_bytes: 100,
                exact: Some(PalwConvertedPin { size_bytes: 100, sha256: "00" }),
                source_repo: "example/weights",
                convert_command: "convert --out out.palwart",
            },
            is_base: false,
            context_tokens: 8_192,
            tokenizer: None,
            min_memory_bytes: 0,
        }];
        let at = |size: u64| assess(ONLY, &[("/m/out.palwart".into(), "out.palwart".into(), size)], 64 << 30)[0].readiness.clone();
        assert!(matches!(at(100), PalwClassReadiness::ArtifactPresent { .. }));
        assert!(matches!(at(99), PalwClassReadiness::ArtifactMismatch { expected_bytes: 100, .. }));
    }

    /// The memory note is about what serving costs, not only the file: an 8k seat on a 3 GiB machine
    /// holds a 1.68 GiB artifact and still cannot replay.
    #[test]
    fn the_memory_note_counts_the_replay_not_just_the_file() {
        let at = |mem: u64, name: &str| {
            assess_classes(NodeNetwork::Testnet12, &[], mem).into_iter().find(|s| s.spec.name == name).unwrap().memory_note
        };
        assert!(at(3 << 30, "PALW-QWEN25-A16-8K").expect("a note").contains("cannot run"));
        assert!(at(16 << 30, "PALW-QWEN25-A16-8K").is_none());
        assert!(at(8 << 30, "PALW-QWEN25-A16-2M").expect("a note").contains("cannot run"), "≈ 11.6 GiB an attempt");
        assert!(at(16 << 30, "PALW-QWEN25-A16-2M").is_none());
    }

    #[test]
    fn each_network_reads_its_own_table() {
        assert_eq!(classes_for(NodeNetwork::Testnet12).len(), TESTNET12_CLASSES.len());
        assert_eq!(classes_for(NodeNetwork::Testnet11).len(), TESTNET11_CLASSES.len());
        assert_eq!(default_class_for(NodeNetwork::Testnet12).name, "PALW-QWEN25-A16-8K");
        assert_eq!(default_class_for(NodeNetwork::Testnet11).name, "PALW-QWEN25-A16");
        // A file is an artifact whichever network is selected: the 512 and hybrid files of
        // testnet-11 still must not reach an inference engine on testnet-12.
        assert!(is_artifact_filename("qwen36.palwq36"));
        assert_eq!(class_for_artifact_filename("qwen25-1.5b-a16.palwart").unwrap().name, "PALW-QWEN25-A16");
        // The 2M file resolves to the public network's row, whose root is the right one.
        assert!(class_for_artifact_filename("qwen25-1.5b-a16-2m.palwart").unwrap().artifact_root_hex.starts_with("f63af2c4"));
    }

    #[test]
    fn the_floor_is_always_ready_even_on_an_empty_machine() {
        let statuses = assess_classes(NodeNetwork::Testnet12, &[], 8 << 30);
        let base = statuses.iter().find(|s| s.spec.is_base).expect("floor");
        assert_eq!(base.readiness, PalwClassReadiness::ReadyBuiltIn);
        assert!(base.memory_note.is_none());
    }

    #[test]
    fn a_present_artifact_is_reported_with_its_path_and_not_called_verified() {
        let files = vec![("/m/qwen36.palwq36".to_string(), "qwen36.palwq36".to_string(), 36_492_831_232u64)];
        let statuses = assess_classes(NodeNetwork::Testnet11, &files, 64 << 30);
        let qwen36 = statuses.iter().find(|s| s.spec.name == "QWEN36").expect("class");
        match &qwen36.readiness {
            PalwClassReadiness::ArtifactPresent { path, verified, .. } => {
                assert_eq!(path, "/m/qwen36.palwq36");
                assert!(!verified, "presence is a filename, not an identity");
            }
            other => panic!("expected present, got {other:?}"),
        }
    }

    /// A truncated 34 GiB download must not be shown as ready — the node would refuse it at
    /// startup, and the UI saying "present" until then wastes the operator's session.
    #[test]
    fn a_wrong_sized_artifact_is_a_mismatch_not_a_presence() {
        let files = vec![("/m/qwen36.palwq36".to_string(), "qwen36.palwq36".to_string(), 1_000_000u64)];
        let statuses = assess_classes(NodeNetwork::Testnet11, &files, 64 << 30);
        let qwen36 = statuses.iter().find(|s| s.spec.name == "QWEN36").expect("class");
        assert!(matches!(qwen36.readiness, PalwClassReadiness::ArtifactMismatch { expected_bytes: 36_492_831_232, .. }));
    }

    /// The default is the model class the Studio points a node at: named in the registry, not the
    /// floor — which needs no file and would make the whole thing a no-op — and with a file whose
    /// size is pinned, so "present" can mean the right file.
    #[test]
    fn the_default_class_is_a_model_class_with_a_pinned_file() {
        for network in [NodeNetwork::Testnet12, NodeNetwork::Testnet11] {
            let spec = default_class_for(network);
            assert!(!spec.is_base, "the floor needs no artifact");
            assert!(exact_artifact_size(spec).is_some(), "{}: the default's file has one right size", spec.name);
        }
        assert_eq!(default_class().name, DEFAULT_CLASS);
    }

    #[test]
    fn a_missing_artifact_offers_a_download_only_where_one_is_published() {
        for network in [NodeNetwork::Testnet12, NodeNetwork::Testnet11] {
            for status in assess_classes(network, &[], 64 << 30).iter().filter(|s| !s.spec.is_base) {
                let downloadable = matches!(status.spec.artifact, PalwArtifactSource::Download { .. });
                assert_eq!(
                    status.readiness,
                    PalwClassReadiness::ArtifactMissing { downloadable },
                    "{}: a published file is one click; a convert-locally class is not",
                    status.spec.name
                );
            }
        }
    }

    /// The first 64 bytes of the real `qwen25-1.5b-a16.palwart` (1,795,427,276 bytes, the file the
    /// chain pins), copied from `xxd`: Qwen2.5-1.5B's shape and a 512-position rotary table.
    #[test]
    fn the_real_a16_artifact_header_reads_as_qwen25_at_512_positions() {
        let head: [u8; 64] = [
            0x50, 0x41, 0x4c, 0x57, 0x42, 0x30, 0x41, 0x32, 0x1c, 0, 0, 0, 0, 0, 0, 0, 0x0c, 0, 0, 0, 0, 0, 0, 0, 0x02, 0, 0, 0, 0, 0,
            0, 0, 0x80, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x23, 0, 0, 0, 0, 0, 0, 0x80, 0x51, 0x02, 0, 0, 0, 0, 0, 0x00, 0x02, 0, 0, 0, 0, 0,
            0,
        ];
        let header = parse_artifact_header(&head).expect("a BASE-0 container");
        assert_eq!(
            header,
            PalwArtifactHeader {
                n_layers: 28,
                n_heads: 12,
                n_kv_heads: 2,
                d_head: 128,
                d_ff: 8960,
                vocab: 151_936,
                max_position: 512
            }
        );
        let a16 = TESTNET11_CLASSES.iter().find(|c| c.name == "PALW-QWEN25-A16").unwrap();
        assert_eq!(u64::from(a16.context_tokens), header.max_position, "the registered class uses the whole table");
    }

    /// Another format, a short read and a nonsense shape are all "not read" — never a number.
    #[test]
    fn a_header_this_build_does_not_read_yields_nothing() {
        let mut qwen36 = [0u8; 64];
        qwen36[..8].copy_from_slice(b"PALWQ361");
        qwen36[8] = 40;
        assert_eq!(parse_artifact_header(&qwen36), None, "the hybrid format lays its header out differently");
        assert_eq!(parse_artifact_header(b"PALWB0A2\x1c\0\0"), None, "short");
        let mut zero_layers = [0u8; 64];
        zero_layers[..8].copy_from_slice(b"PALWB0A2");
        assert_eq!(parse_artifact_header(&zero_layers), None, "a shape the runtime would refuse");
    }

    /// The A16 class's tokenizer lands beside its artifact under a name no other class shares.
    #[test]
    fn the_a16_tokenizer_is_pinned_and_named_after_its_artifact() {
        let a16 = class_for_artifact_filename("qwen25-1.5b-a16.palwart").expect("the A16 class");
        assert_eq!(a16.name, "PALW-QWEN25-A16");
        assert_eq!(a16.tokenizer_filename().as_deref(), Some("qwen25-1.5b-a16.tokenizer.json"));
        let pin = a16.tokenizer.expect("pinned");
        assert_eq!(pin.sha256.len(), 64);
        assert!(class_for_artifact_filename("nope.palwart").is_none());
    }

    /// The 2M held-context row is a different class: same weights family, different graph, different file.
    #[test]
    fn the_2m_class_is_not_the_512_artifact() {
        let two = class_for_artifact_filename("qwen25-1.5b-a16-2m.palwart").expect("the 2M class");
        assert_eq!(two.name, "PALW-QWEN25-A16-2M");
        assert_eq!(two.context_tokens, 2_097_152);
        assert_eq!(two.share_permille, 1, "the testnet-12 row answers for the file: minimum grantable share");
        assert_eq!(two.tokenizer_filename().as_deref(), Some("qwen25-1.5b-a16-2m.tokenizer.json"));
        assert_eq!(class_for_artifact_filename("qwen25-1.5b-a16.palwart").unwrap().name, "PALW-QWEN25-A16");
        let files = vec![("/m/qwen25-1.5b-a16.palwart".into(), "qwen25-1.5b-a16.palwart".into(), 1_795_427_276u64)];
        let two = assess_classes(NodeNetwork::Testnet11, &files, 64 << 30)
            .into_iter()
            .find(|s| s.spec.name == "PALW-QWEN25-A16-2M")
            .unwrap();
        assert_eq!(two.readiness, PalwClassReadiness::ArtifactMissing { downloadable: true });
        assert!(matches!(two.spec.artifact, PalwArtifactSource::Download { filename: "qwen25-1.5b-a16-2m.palwart", .. }));
    }

    /// Every class names the context it was registered at, and none claims zero.
    #[test]
    fn every_class_names_its_registered_context() {
        for class in TESTNET12_CLASSES.iter().chain(TESTNET11_CLASSES) {
            assert!(class.context_tokens > 0, "{}", class.name);
        }
        let by_name: std::collections::HashMap<_, _> = TESTNET11_CLASSES.iter().map(|c| (c.name, c.context_tokens)).collect();
        assert_eq!(by_name["PALW-BASE-0"], 12);
        assert_eq!(by_name["PALW-QWEN25-A16"], 512);
        assert_eq!(by_name["PALW-QWEN25-A16-2M"], 2_097_152);
        assert_eq!(by_name["QWEN36"], 512);
    }

    /// The convert-locally branch is covered by a synthetic row: every live class now publishes
    /// a download, and this path must still compile and assess without one.
    #[test]
    fn a_class_with_no_published_artifact_says_so_instead_of_offering_a_download() {
        const ONLY: &[PalwClassSpec] = &[PalwClassSpec {
            name: "SYNTHETIC",
            description: "",
            share_permille: 1000,
            class_id_hex: "",
            class_id_complete: false,
            artifact_root_hex: "",
            artifact: PalwArtifactSource::ConvertLocally {
                filename: "out.palwart",
                approx_size_bytes: 1 << 30,
                exact: None,
                source_repo: "example/weights",
                convert_command: "convert --out out.palwart",
            },
            is_base: false,
            context_tokens: 512,
            tokenizer: None,
            min_memory_bytes: 0,
        }];

        let missing = assess(ONLY, &[], 64 << 30);
        assert_eq!(missing[0].readiness, PalwClassReadiness::ArtifactMissing { downloadable: false });

        // Presence is judged by the converted filename: a conversion's byte size varies with its
        // input, so there is no size to compare against and the root check is the node's.
        let files = vec![("/m/out.palwart".to_string(), "out.palwart".to_string(), 12u64)];
        let present = assess(ONLY, &files, 64 << 30);
        assert!(matches!(present[0].readiness, PalwClassReadiness::ArtifactPresent { .. }));
    }

    /// A 34 GiB class on a 16 GiB laptop: listed, and honest about why it will not run — not
    /// hidden, because seeing what stronger hardware could mine is part of the point of a list.
    #[test]
    fn an_oversized_class_carries_a_memory_note() {
        let statuses = assess_classes(NodeNetwork::Testnet11, &[], 16 << 30);
        let qwen36 = statuses.iter().find(|s| s.spec.name == "QWEN36").expect("class");
        let note = qwen36.memory_note.as_ref().expect("a note");
        assert!(note.contains("cannot run"), "{note}");
        let qwen25 = statuses.iter().find(|s| s.spec.name == "PALW-QWEN25-A16").expect("class");
        assert!(qwen25.memory_note.is_none(), "1.7 GiB fits a 16 GiB machine");
    }

    /// The extension comes off the filename's LAST dot. Every artifact in the table is named after
    /// a model with a version in it, so the first dot answers `.5-1.5b-a16` and the scan then finds
    /// nothing at all.
    #[test]
    fn an_extension_is_read_from_the_last_dot_of_the_filename() {
        let download = PalwArtifactSource::Download {
            filename: "qwen2.5-1.5b-a16.palwart",
            repo_path: "palw-runtime/qwen2.5-1.5b-a16.palwart",
            sha256: "00",
            size_bytes: 1,
            hf_repo: "example/repo",
            convert_command: "convert",
        };
        assert_eq!(download.file_extension(), Some(".palwart"));
        assert_eq!(
            PalwArtifactSource::ConvertLocally {
                filename: "qwen36.palwq36",
                approx_size_bytes: 1,
                exact: None,
                source_repo: "example/weights",
                convert_command: "convert",
            }
            .file_extension(),
            Some(".palwq36")
        );
        // The floor has no file, and a class with no file must not contribute an extension that
        // would make every name in the directory an artifact.
        assert_eq!(PalwArtifactSource::DerivedFromSeed.file_extension(), None);
    }

    /// Every registered class that has a file must be recognisable from its name. A class whose
    /// filename lost its extension would be invisible to the scan and, worse, invisible to the gate
    /// that keeps artifacts out of an inference engine.
    #[test]
    fn every_registered_artifact_is_recognised_by_its_own_filename() {
        for class in TESTNET12_CLASSES.iter().chain(TESTNET11_CLASSES) {
            let filename = match class.artifact {
                PalwArtifactSource::Download { filename, .. } | PalwArtifactSource::ConvertLocally { filename, .. } => filename,
                PalwArtifactSource::DerivedFromSeed => continue,
            };
            assert!(is_artifact_filename(filename), "{}: {filename} is not recognised as an artifact", class.name);
        }
    }

    /// The two file kinds share one directory, so this predicate is the only thing between a PALW
    /// artifact and `llama-server`, which reads four bytes, finds `PALW` where `GGUF` should be and
    /// aborts. A `.part` is not an artifact yet: offering half a file is offering that same failure.
    #[test]
    fn a_gguf_and_a_half_downloaded_artifact_are_not_artifacts() {
        assert!(is_artifact_filename("qwen25-1.5b-a16.palwart"));
        assert!(is_artifact_filename("qwen25-1.5b-a16-2m.palwart"));
        assert!(is_artifact_filename("qwen36.palwq36"));
        assert!(is_artifact_filename("QWEN36.PALWQ36"), "the models directory is not case-sensitive everywhere");
        assert!(!is_artifact_filename("qwen2.5-1.5b-instruct-q4_k_m.gguf"));
        assert!(!is_artifact_filename("qwen36.palwq36.part"));
        assert!(!is_artifact_filename("qwen25-1.5b-a16.palwart.misaka.json"));
    }
}
