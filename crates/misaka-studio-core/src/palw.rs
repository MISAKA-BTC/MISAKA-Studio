//! The PALW execution classes — the list a person consults before mining.
//!
//! On the MISAKA network a block is won by verified LLM inference, and *which* model you run is a
//! chain-registered **class**: an execution graph the whole network can re-derive, with a share of
//! the emission and an artifact every panel seat checks byte-for-byte. "Can I mine, and with
//! what?" therefore has a precise answer per class, and this module is that answer as data — the
//! UX equivalent of the model list, but for participation.
//!
//! Three classes ship in testnet-11's genesis (`docs/testnet11-join-mining.md` §5–6c and
//! `docs/palw-public-testnet-classes-runbook.md` in the misakas repository, plus the pinned
//! constants in its `consensus/core/src/config/params.rs`). A fourth row is the held-context
//! 2M class (`docs/qwen25-a16-2m-held-artifact.md`): same Instruct weights as the dense genesis
//! class, a different graph (`graph-v7@2097152`) and a 2M rotary table.
//!
//! Past ADR-0137 (testnet-11 DAA 6,001) **share is not an input**. A block buys one unit of work
//! from any model (`CCU_m / W`); genesis permille below is the Relaunch 5f table, kept so a card
//! can still name the row, and live share is the fraction of Final work a reader computes.
//!
//! | class | artifact | genesis ‰ (legacy table) |
//! |---|---|---|
//! | `PALW-BASE-0` | none — derived from a seed on every node | 22 |
//! | `PALW-QWEN25-A16` | `qwen25-1.5b-a16.palwart`, 1.7 GiB (chain id graph-v5@512) | 489 |
//! | `QWEN36` | `qwen36.palwq36`, 34 GiB (chain id graph-v3) | 489 |
//! | `PALW-QWEN25-A16-2M` | `qwen25-1.5b-a16-2m.palwart`, 2.7 GiB (chain id graph-v7@2097152) | 0 |
//!
//! # What this table is, and is not
//!
//! It is a **pinned snapshot of the testnet-11 genesis registry**, kept here so the Studio can
//! show the list — with artifact identities a download can be verified against — before any node
//! is running. It is not the source of truth: the chain is, and a node's own startup check
//! (`the node checks this itself and refuses a mismatch`) is what finally gates production. If
//! the registry ever changes, this table is a release-note edit, exactly like the quantization
//! table.
//!
//! Every hash here is copied from the runbooks and the consensus constants verbatim, with its
//! provenance named, so a mismatch is attributable. The one value this table carries only as a
//! prefix is BASE-0's class id: the id is `shape_profile_id()` — a function of the execution
//! graph, computed by the node — and the docs print it truncated. The Studio displays what it can
//! prove and lets the node's own output supply the rest.

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
        /// The public weights the conversion reads.
        source_repo: &'static str,
        convert_command: &'static str,
    },
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
    TESTNET11_CLASSES.iter().filter_map(|class| class.artifact.file_extension()).any(|ext| lower.ends_with(ext))
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
    TESTNET11_CLASSES.iter().find(|class| match class.artifact {
        PalwArtifactSource::Download { filename, .. } | PalwArtifactSource::ConvertLocally { filename, .. } => filename == file_name,
        PalwArtifactSource::DerivedFromSeed => false,
    })
}

/// GiB, binary.
const GIB: u64 = 1 << 30;

/// **How long a block's pay sits before it can be spent, in DAA**, on testnet-11.
///
/// A coinbase output matures one block after acceptance and then waits out the settlement window
/// the network runs (600 DAA), so a reward is spendable at `block DAA + 601`. The node does not
/// publish the window over RPC — it is a consensus parameter — so it is pinned here beside the
/// class table, with the same rule: if the network changes it, this is the line that must change.
/// It is used only to SAY when a reward becomes spendable; nothing in the app spends.
pub const TESTNET11_COINBASE_MATURITY_DAA: u64 = 601;

/// The testnet-11 genesis classes.
///
/// Order is the order a newcomer should read them in: the one that needs nothing first.
pub const TESTNET11_CLASSES: &[PalwClassSpec] = &[
    PalwClassSpec {
        name: "PALW-BASE-0",
        description: "The deterministic integer floor. Its artifact is derived from a seed on every node — no GGUF, \
                      no download, no GPU. Past ADR-0137 it fills whatever cadence the model classes leave and is \
                      paid nothing for it. The default class when none is named.",
        share_permille: 22,
        // docs/palw-rc-testnet11-launch-runbook.md prints the first half; the id is computed by
        // the node (`shape_profile_id()`), and the Studio shows the node's own value once one is
        // connected.
        // testnet-11 Relaunch 5f (2026-09-03, genesis ad30b5cb…): the id and root the public node
        // reports through `getPalwProducerFacts`; `palw-class ledger --network testnet-11` prints the same.
        class_id_hex: "f1c5635c6e47e96e7af864789c94523335dc56584af297cb8cc19021c228b897bee1a50145597e45f8ca2727349bf4aa352a98cc05274b7f059a176642f623c8",
        class_id_complete: true,
        artifact_root_hex: "bcf2d9eb7357bd6c267df2df6588393ca71c67d7c802903ca7031948303c793dcb78bfe26488f52d0393be08e0cc0777b080e2dce9355d3576036b734545b8df",
        artifact: PalwArtifactSource::DerivedFromSeed,
        is_base: true,
        // `PALW_RC_BASE0_GEOMETRY.n_ctx`: sized by the court's cost ceiling, not by capability.
        context_tokens: 12,
        tokenizer: None,
    },
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
        // Byte-identical to `Qwen/Qwen2.5-1.5B-Instruct`'s own tokenizer.json (compared 2026-09-17).
        tokenizer: Some(PalwTokenizerPin {
            hf_repo: "Misakachain/Qwen2.5-1.5B-PALW-A16-runtime",
            repo_path: "tokenizer.json",
            sha256: "c0382117ea329cdf097041132f6d735924b697924d6f6fc3945713e96ce87539",
            size_bytes: 7_031_645,
        }),
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
        tokenizer: Some(PalwTokenizerPin {
            hf_repo: "Misakachain/Qwen2.5-1.5B-PALW-A16-runtime",
            repo_path: "tokenizer.json",
            sha256: "c0382117ea329cdf097041132f6d735924b697924d6f6fc3945713e96ce87539",
            size_bytes: 7_031_645,
        }),
    },
];

/// The class the Studio installs on first run and produces in when none is chosen.
///
/// **Not the chain's default.** That is the floor: no file, no download, and what a node mines
/// when no class is named. This is the Studio's answer to a different question — of the classes
/// that actually run a model, which one can a machine that just downloaded this app be expected
/// to hold and install? QWEN36 is 34 GiB. The floor runs no model at all. That leaves one, at
/// 1.7 GiB and a single verified download, and shipping the floor by default would mean an app
/// whose headline is verified inference that never loads a model.
pub const DEFAULT_CLASS: &str = "PALW-QWEN25-A16";

/// The spec [`DEFAULT_CLASS`] names.
///
/// Panics if the registry no longer carries that name, which is a build that should not have
/// linked rather than a condition to handle at runtime; the test below is what holds it.
pub fn default_class() -> &'static PalwClassSpec {
    TESTNET11_CLASSES.iter().find(|class| class.name == DEFAULT_CLASS).expect("DEFAULT_CLASS names a registered class")
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

/// Assess every testnet-11 class against a directory scan and the machine.
///
/// `artifact_files` is (path, file name, size) for candidate artifact files — the caller scans
/// its models directory (and the node's app dir if it knows one); this stays pure so it is
/// testable without a filesystem.
pub fn assess_classes(artifact_files: &[(String, String, u64)], total_memory: u64) -> Vec<PalwClassStatus> {
    assess(TESTNET11_CLASSES, artifact_files, total_memory)
}

/// The same, against an arbitrary class list.
///
/// Separate from [`assess_classes`] so the registry snapshot is not the only input this logic can
/// ever be shown: every class currently registered publishes its artifact, and without this the
/// convert-locally branch below would be code no test can reach.
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
                PalwArtifactSource::ConvertLocally { filename, .. } => {
                    match artifact_files.iter().find(|(_, name, _)| name == filename) {
                        // A conversion's byte size varies with its input, so presence is judged
                        // by the exact filename and the root check is the node's.
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
            let memory_note = (artifact_bytes > total_memory).then(|| {
                format!(
                    "the artifact is {:.1} GiB against {:.1} GiB of RAM — this machine cannot run this class",
                    artifact_bytes as f64 / GIB as f64,
                    total_memory as f64 / GIB as f64
                )
            });

            PalwClassStatus { spec: spec.clone(), readiness, memory_note, artifact_header: None }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_snapshot_is_internally_consistent() {
        // Relaunch 5f (2026-09-03) seats three classes at genesis: the floor, graph-v5@512 and graph-v3.
        // The 2M held-context row is catalogued beside them (graph-v7@2097152) with no genesis grant.
        assert_eq!(TESTNET11_CLASSES.len(), 4);
        // Genesis rows split the whole emission. A post-genesis entrant carries 0 here — live share
        // is Final work (ADR-0137 D7), the chain's to report, not this table's.
        let genesis: u16 = TESTNET11_CLASSES.iter().filter(|c| c.share_permille > 0).map(|c| c.share_permille).sum();
        assert_eq!(genesis, 1000, "genesis shares are permille of the whole emission");
        let two_m = TESTNET11_CLASSES.iter().find(|c| c.name == "PALW-QWEN25-A16-2M").expect("2M class");
        assert_eq!(two_m.share_permille, 0);
        assert_eq!(two_m.context_tokens, 2_097_152);

        let base: Vec<_> = TESTNET11_CLASSES.iter().filter(|c| c.is_base).collect();
        assert_eq!(base.len(), 1, "exactly one floor");
        assert_eq!(base[0].name, "PALW-BASE-0");
        assert!(matches!(base[0].artifact, PalwArtifactSource::DerivedFromSeed));

        for class in TESTNET11_CLASSES {
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
        }
    }

    #[test]
    fn the_floor_is_always_ready_even_on_an_empty_machine() {
        let statuses = assess_classes(&[], 8 << 30);
        let base = statuses.iter().find(|s| s.spec.is_base).expect("floor");
        assert_eq!(base.readiness, PalwClassReadiness::ReadyBuiltIn);
        assert!(base.memory_note.is_none());
    }

    #[test]
    fn a_present_artifact_is_reported_with_its_path_and_not_called_verified() {
        let files = vec![("/m/qwen36.palwq36".to_string(), "qwen36.palwq36".to_string(), 36_492_831_232u64)];
        let statuses = assess_classes(&files, 64 << 30);
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
        let statuses = assess_classes(&files, 64 << 30);
        let qwen36 = statuses.iter().find(|s| s.spec.name == "QWEN36").expect("class");
        assert!(matches!(qwen36.readiness, PalwClassReadiness::ArtifactMismatch { expected_bytes: 36_492_831_232, .. }));
    }

    /// The default is preinstalled on first run, so it has to be a class that *can* be: named in
    /// the registry, publishing an artifact, and not the floor — which needs no file and would
    /// make the whole bootstrap a no-op.
    #[test]
    fn the_default_class_is_one_that_can_actually_be_preinstalled() {
        let spec = default_class();
        assert_eq!(spec.name, DEFAULT_CLASS);
        assert!(!spec.is_base, "the floor needs no artifact; preinstalling it would install nothing");
        assert!(
            matches!(spec.artifact, PalwArtifactSource::Download { .. }),
            "a default with no published artifact cannot be installed without a toolchain"
        );
    }

    #[test]
    fn every_registered_model_class_can_be_installed_without_a_toolchain() {
        let statuses = assess_classes(&[], 64 << 30);
        for status in statuses.iter().filter(|s| !s.spec.is_base) {
            let downloadable = matches!(status.spec.artifact, PalwArtifactSource::Download { .. });
            assert_eq!(
                status.readiness,
                PalwClassReadiness::ArtifactMissing { downloadable },
                "{}: a published file is one click; a convert-locally class is not",
                status.spec.name
            );
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
        assert_eq!(two.share_permille, 0);
        assert_eq!(two.tokenizer_filename().as_deref(), Some("qwen25-1.5b-a16-2m.tokenizer.json"));
        assert_eq!(class_for_artifact_filename("qwen25-1.5b-a16.palwart").unwrap().name, "PALW-QWEN25-A16");
        let files = vec![("/m/qwen25-1.5b-a16.palwart".into(), "qwen25-1.5b-a16.palwart".into(), 1_795_427_276u64)];
        let two = assess_classes(&files, 64 << 30).into_iter().find(|s| s.spec.name == "PALW-QWEN25-A16-2M").unwrap();
        assert_eq!(two.readiness, PalwClassReadiness::ArtifactMissing { downloadable: true });
        assert!(matches!(two.spec.artifact, PalwArtifactSource::Download { filename: "qwen25-1.5b-a16-2m.palwart", .. }));
    }

    /// Every class names the context it was registered at, and none claims zero.
    #[test]
    fn every_class_names_its_registered_context() {
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
                source_repo: "example/weights",
                convert_command: "convert --out out.palwart",
            },
            is_base: false,
            context_tokens: 512,
            tokenizer: None,
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
        let statuses = assess_classes(&[], 16 << 30);
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
        for class in TESTNET11_CLASSES {
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
