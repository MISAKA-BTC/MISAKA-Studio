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
//! constants in its `consensus/core/src/config/params.rs`):
//!
//! | class | artifact | share |
//! |---|---|---|
//! | `PALW-BASE-0` | none — derived from a seed on every node | 22‰ |
//! | `PALW-QWEN25-A16` | `qwen25-1.5b-a16.palwart`, 1.7 GiB, downloadable (chain id graph-v5@512) | 489‰ |
//! | `QWEN36` | `qwen36.palwq36`, 34 GiB, downloadable (chain id graph-v3) | 489‰ |
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
        /// Extension the converted artifact carries, e.g. `.palwart`.
        extension: &'static str,
        approx_size_bytes: u64,
        /// The public weights the conversion reads.
        source_repo: &'static str,
        convert_command: &'static str,
    },
}

/// One chain-registered execution class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PalwClassSpec {
    /// The name operators know it by.
    pub name: &'static str,
    pub description: &'static str,
    /// Share of emission, in permille. The floor's is what remains after the model classes.
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
    /// The floor: default when no class is named, exempt from the per-class epoch budget — the
    /// one class that can always produce.
    pub is_base: bool,
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
                      no download, no GPU — and it is exempt from the per-class epoch budget, so it can always \
                      produce. The default class when none is named.",
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
                PalwArtifactSource::ConvertLocally { extension, .. } => {
                    match artifact_files.iter().find(|(_, name, _)| name.ends_with(extension)) {
                        // A conversion's byte size varies with its input, so presence is judged
                        // by extension and the root check is the node's.
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

            PalwClassStatus { spec: spec.clone(), readiness, memory_note }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_snapshot_is_internally_consistent() {
        // Relaunch 5f (2026-09-03) seats three classes at genesis: the floor, graph-v5@512 and graph-v3.
        assert_eq!(TESTNET11_CLASSES.len(), 3);
        // The GENESIS rows split the whole emission. A post-genesis entrant carries 0 here —
        // its share follows production (ADR-0054) and is the chain's to report, not this table's.
        let total: u16 = TESTNET11_CLASSES.iter().map(|c| c.share_permille).sum();
        assert_eq!(total, 1000, "genesis shares are permille of the whole emission");

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
            assert_eq!(
                status.readiness,
                PalwClassReadiness::ArtifactMissing { downloadable: true },
                "{} publishes an artifact, so an empty machine must be one click from it",
                status.spec.name
            );
        }
    }

    /// The convert-locally branch, which no currently registered class takes. Kept covered
    /// because "not reachable today" and "correct" are different claims.
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
                extension: ".palwart",
                approx_size_bytes: 1 << 30,
                source_repo: "example/weights",
                convert_command: "convert --out out.palwart",
            },
            is_base: false,
        }];

        let missing = assess(ONLY, &[], 64 << 30);
        assert_eq!(missing[0].readiness, PalwClassReadiness::ArtifactMissing { downloadable: false });

        // Presence is judged by extension here: a conversion's byte size varies with its input,
        // so there is no size to compare against and the root check is the node's.
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
}
