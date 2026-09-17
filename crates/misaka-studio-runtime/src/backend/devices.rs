//! What the engine can drive, and what it actually did with the model.
//!
//! The hardware probe (`nvidia-smi`, `rocm-smi`, `sysctl`) answers what the **machine** has. It
//! cannot answer whether the engine binary can use any of it — a `llama-server` from a package
//! repository or a plain `cmake` build is very often CPU-only, and it accepts `--n-gpu-layers 99`
//! without a word. The report from the field that started this file: a tester whose A16 worker and
//! Studio chat both ran on the CPU while nothing in the app said so, and a model bar that would have
//! shown `28/28 on GPU` — the request, echoed back as if it were an observation.
//!
//! Two sources, both the engine's own:
//!
//! * **`llama-server --list-devices`** — the devices THIS binary can offload to. A CPU-only build
//!   lists none. Asked once per binary and cached by path and modification time.
//! * **The load log at `-lv 4`** — `load_tensors: offloaded 29/29 layers to GPU` and the buffer
//!   lines under it. The first line **lies when there is no GPU**: measured with `--device none`,
//!   it still prints `offloaded 29/29` while every buffer is `CPU_Mapped`. So a layer count is
//!   believed only when a non-CPU buffer actually holds bytes, and the buffers are what is
//!   reported.
//!
//! Every number here carries where it came from ([`OffloadEvidence`]), because the difference
//! between "the engine said so", "the device list implies it" and "we could not tell" is the
//! difference the user needs when a model is unexpectedly slow.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

/// What kind of thing a device id names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineDeviceKind {
    /// An offload target: layers placed here run on it.
    Gpu,
    /// A CPU-side accelerator (`BLAS`): speeds up matrix multiplication, holds no layers.
    Accel,
    /// A remote `RPC` device — someone else's GPU.
    Remote,
    Cpu,
}

/// One device as the engine lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineDevice {
    /// As the engine names it on its own command line: `MTL0`, `CUDA0`, `Vulkan0`, `BLAS`.
    pub id: String,
    /// The engine's description: `Apple M4 Pro`, `NVIDIA GeForce RTX 3060`.
    pub description: String,
    /// The backend behind the id, lower-case: `metal`, `cuda`, `vulkan`, `rocm`, `sycl`, `blas`.
    pub backend: String,
    pub kind: EngineDeviceKind,
    pub total_mib: Option<u64>,
    pub free_mib: Option<u64>,
}

impl EngineDevice {
    /// `MTL0 (Apple M4 Pro)` — how the device is named to a person.
    pub fn label(&self) -> String {
        format!("{} ({})", self.id, self.description)
    }

    /// Bytes a model may take on this device: 90 % of what is free, the same margin the hardware
    /// probe keeps for a discrete card (the driver and the desktop hold the rest).
    pub fn usable_bytes(&self) -> Option<u64> {
        self.free_mib.map(|mib| (mib as f64 * 1024.0 * 1024.0 * 0.9) as u64)
    }
}

/// The first offload target in a device list, if any.
pub fn first_gpu(devices: &[EngineDevice]) -> Option<&EngineDevice> {
    devices.iter().find(|d| d.kind == EngineDeviceKind::Gpu)
}

/// Parse `llama-server --list-devices`.
///
/// ```text
/// Available devices:
///   BLAS: Accelerate (0 MiB, 0 MiB free)
///   MTL0: Apple M4 Pro (18186 MiB, 18185 MiB free)
/// ```
///
/// `None` when the text is not that listing at all — an engine too old to know the flag answers
/// `error: invalid argument: --list-devices`, and "unknown" is a different answer from "none".
/// The CPU device is never listed by the engine and is dropped here if it ever is.
pub fn parse_list_devices(text: &str) -> Option<Vec<EngineDevice>> {
    let mut lines = text.lines().map(str::trim);
    lines.find(|line| line.starts_with("Available devices"))?;
    let devices = lines
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let (id, rest) = line.split_once(':')?;
            let id = id.trim();
            if id.is_empty() || id.contains(' ') {
                return None;
            }
            let rest = rest.trim();
            // `<description> (<total> MiB, <free> MiB free)`; the description may itself hold
            // parentheses, so the memory is the LAST parenthesised group.
            let (description, memory) = match rest.rfind(" (") {
                Some(idx) if rest.ends_with(')') => (&rest[..idx], Some(&rest[idx + 2..rest.len() - 1])),
                _ => (rest, None),
            };
            let mut mib = memory
                .into_iter()
                .flat_map(|m| m.split(','))
                .map(|part| part.split_whitespace().next().and_then(|n| n.parse::<u64>().ok()));
            let backend = backend_of(id);
            let kind = kind_of(&backend);
            Some(EngineDevice {
                id: id.to_string(),
                description: description.trim().to_string(),
                total_mib: mib.next().flatten(),
                free_mib: mib.next().flatten(),
                backend,
                kind,
            })
        })
        .filter(|d| d.kind != EngineDeviceKind::Cpu)
        .collect();
    Some(devices)
}

/// The backend a device id belongs to, by the prefix the engine gives it.
fn backend_of(id: &str) -> String {
    let lower = id.to_ascii_lowercase();
    for (prefix, backend) in [
        ("mtl", "metal"),
        ("metal", "metal"),
        ("cuda", "cuda"),
        ("vulkan", "vulkan"),
        ("rocm", "rocm"),
        ("hip", "rocm"),
        ("sycl", "sycl"),
        ("gpuopencl", "opencl"),
        ("opencl", "opencl"),
        ("cann", "cann"),
        ("musa", "musa"),
        ("openvino", "openvino"),
        ("webgpu", "webgpu"),
        ("zdnn", "zdnn"),
        ("blas", "blas"),
        ("rpc", "rpc"),
        ("cpu", "cpu"),
    ] {
        if lower.starts_with(prefix) {
            return backend.to_string();
        }
    }
    lower.trim_end_matches(|c: char| c.is_ascii_digit()).to_string()
}

fn kind_of(backend: &str) -> EngineDeviceKind {
    match backend {
        "cpu" => EngineDeviceKind::Cpu,
        "blas" => EngineDeviceKind::Accel,
        "rpc" => EngineDeviceKind::Remote,
        _ => EngineDeviceKind::Gpu,
    }
}

/// The determinism-class tag for what the engine can drive: `metal`, `cuda`, `vulkan`, `rocm`,
/// `cpu`. `None` when the list is unknown, so the caller falls back to the hardware probe.
///
/// A CPU-only engine on a machine with an NVIDIA card was tagged `cuda` before this existed — an
/// `h_R` that named arithmetic the binary never ran.
pub fn accelerator_tag_for(devices: Option<&[EngineDevice]>) -> Option<&'static str> {
    let devices = devices?;
    Some(match first_gpu(devices).map(|d| d.backend.as_str()) {
        Some("metal") => "metal",
        Some("cuda") => "cuda",
        Some("vulkan") => "vulkan",
        Some("rocm") => "rocm",
        Some("sycl") => "sycl",
        Some("opencl") => "opencl",
        Some(_) => "gpu",
        None => "cpu",
    })
}

/// Where an [`Offload`]'s numbers came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OffloadEvidence {
    /// The engine's own load log: which buffers hold the weights, and on which device.
    EngineLog,
    /// Inferred from the engine's device list — it has (or has not) a device to put layers on.
    DeviceList,
    /// The engine is too old to say. The numbers are the request, and nothing confirms them.
    Unverified,
}

/// What became of the layers the Studio asked to offload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offload {
    /// Layers the Studio asked the engine to place on the accelerator. `None`: the engine's own
    /// default.
    pub asked: Option<u32>,
    /// Layers on the accelerator. `Some(0)` is a CPU run; `None` is "not known".
    pub layers: Option<u32>,
    /// The model's offloadable layers (the repeating layers plus the output layer).
    pub total_layers: Option<u32>,
    /// The device holding the weights, as the engine names it: `MTL0 (Apple M4 Pro)`. `None`
    /// when the CPU does, or when nothing said.
    pub device: Option<String>,
    /// Model bytes on the accelerator, from the engine's buffer report.
    pub accelerator_bytes: Option<u64>,
    /// Model bytes on the CPU side (including mapped and pinned host buffers).
    pub cpu_bytes: Option<u64>,
    pub evidence: OffloadEvidence,
}

impl Offload {
    /// Something was asked for and nothing landed — the case the user has to be told about.
    pub fn asked_but_none_landed(&self) -> bool {
        self.asked.is_some_and(|n| n > 0) && self.layers == Some(0)
    }
}

/// Read the engine's load log for what it did with the model.
///
/// The lines, at `-lv 4`, as a real `llama-server` prints them (timestamp and level prefixes
/// stripped here for width):
///
/// ```text
/// llama_prepare_model_devices: using device MTL0 (Apple M4 Pro) (unknown id) - 18185 MiB free
/// load_tensors: offloaded 29/29 layers to GPU
/// load_tensors:   CPU_Mapped model buffer size =   125.19 MiB
/// load_tensors:  MTL0_Mapped model buffer size =  1059.89 MiB
/// ```
///
/// The LAST `offloaded` line is the one that counts: `--verbose` engines print a dry-run pass
/// first, with every buffer at 0.00 MiB. `None` when there is no such line at all — an engine that
/// was not asked for that verbosity, or whose log has already scrolled past it.
pub fn offload_from_log(lines: &[String], asked: Option<u32>) -> Option<Offload> {
    let (idx, layers, total) =
        lines.iter().enumerate().filter_map(|(i, line)| parse_offloaded(line).map(|(n, m)| (i, n, m))).next_back()?;

    let mut accelerator_bytes = 0u64;
    let mut cpu_bytes = 0u64;
    let mut accelerator_buffer: Option<String> = None;
    for line in &lines[idx + 1..] {
        let Some((name, bytes)) = parse_buffer_line(line) else { continue };
        if is_cpu_buffer(&name) {
            cpu_bytes += bytes;
        } else {
            accelerator_bytes += bytes;
            accelerator_buffer.get_or_insert_with(|| name.trim_end_matches("_Mapped").to_string());
        }
    }

    let landed = accelerator_bytes > 0;
    let device = if landed { lines.iter().rev().find_map(|line| parse_using_device(line)).or(accelerator_buffer) } else { None };
    Some(Offload {
        asked,
        // The engine's own count is trusted only when a device actually holds bytes — see the
        // module note on `--device none`.
        layers: Some(if landed { layers } else { 0 }),
        total_layers: Some(total),
        device,
        accelerator_bytes: Some(accelerator_bytes),
        cpu_bytes: Some(cpu_bytes),
        evidence: OffloadEvidence::EngineLog,
    })
}

/// What the device list implies when the log did not say.
pub fn offload_from_devices(devices: Option<&[EngineDevice]>, asked: Option<u32>) -> Offload {
    match devices {
        Some(list) => match first_gpu(list) {
            Some(gpu) => Offload {
                asked,
                layers: asked,
                total_layers: None,
                device: Some(gpu.label()),
                accelerator_bytes: None,
                cpu_bytes: None,
                evidence: OffloadEvidence::DeviceList,
            },
            None => Offload {
                asked,
                layers: Some(0),
                total_layers: None,
                device: None,
                accelerator_bytes: None,
                cpu_bytes: None,
                evidence: OffloadEvidence::DeviceList,
            },
        },
        None => Offload {
            asked,
            layers: asked,
            total_layers: None,
            device: None,
            accelerator_bytes: None,
            cpu_bytes: None,
            evidence: OffloadEvidence::Unverified,
        },
    }
}

/// `offloaded 29/29 layers to GPU` → `(29, 29)`.
fn parse_offloaded(line: &str) -> Option<(u32, u32)> {
    let rest = line.split("offloaded ").nth(1)?;
    let (fraction, tail) = rest.split_once(' ')?;
    if !tail.starts_with("layers to GPU") {
        return None;
    }
    let (n, m) = fraction.split_once('/')?;
    Some((n.parse().ok()?, m.parse().ok()?))
}

/// `load_tensors:  MTL0_Mapped model buffer size =  1059.89 MiB` → `("MTL0_Mapped", bytes)`.
fn parse_buffer_line(line: &str) -> Option<(String, u64)> {
    const MARKER: &str = " model buffer size =";
    let idx = line.find(MARKER)?;
    let before = &line[..idx];
    let name = before.rsplit(|c: char| c.is_whitespace() || c == ':').next()?.to_string();
    if name.is_empty() {
        return None;
    }
    let after = line[idx + MARKER.len()..].trim();
    let (value, unit) = after.split_once(' ')?;
    let value: f64 = value.parse().ok()?;
    let scale = match unit.trim() {
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "KiB" => 1024.0,
        "B" => 1.0,
        _ => return None,
    };
    Some((name, (value * scale) as u64))
}

/// Buffers that live in system memory whatever their name says: `CPU`, `CPU_Mapped`,
/// `CPU_REPACK`, and the pinned host buffers a CUDA build calls `CUDA_Host`.
fn is_cpu_buffer(name: &str) -> bool {
    name.starts_with("CPU") || name.ends_with("_Host")
}

/// `… using device MTL0 (Apple M4 Pro) (unknown id) - 18185 MiB free` → `MTL0 (Apple M4 Pro)`.
fn parse_using_device(line: &str) -> Option<String> {
    let rest = line.split("using device ").nth(1)?;
    let rest = rest.split(" - ").next().unwrap_or(rest);
    let rest = rest.split(" (unknown id)").next().unwrap_or(rest);
    let rest = rest.trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

// ---------------------------------------------------------------------------
// Asking a binary.
// ---------------------------------------------------------------------------

/// What one run of `<program> --version` and `<program> --list-devices` established.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineProbe {
    /// The first `version:` line of the banner, when the program ran.
    pub version: Option<String>,
    /// The whole banner, both streams.
    pub banner: Option<String>,
    /// The device list. `None` when the engine does not know the flag (too old) or did not run.
    pub devices: Option<Vec<EngineDevice>>,
    /// Why the program could not be run, when it could not.
    pub error: Option<String>,
}

impl EngineProbe {
    pub fn runs(&self) -> bool {
        self.banner.is_some()
    }

    /// Whether this engine has a device it could put layers on.
    pub fn has_gpu(&self) -> bool {
        self.devices.as_deref().and_then(first_gpu).is_some()
    }
}

/// How long a probe may take. A CUDA build initialises the driver to list its devices, which is
/// seconds on a cold machine; a hang past this is a broken install, not a slow one.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

type ProbeKey = (PathBuf, Option<SystemTime>, u64);

fn probe_cache() -> &'static Mutex<HashMap<ProbeKey, EngineProbe>> {
    static CACHE: OnceLock<Mutex<HashMap<ProbeKey, EngineProbe>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn probe_key(program: &Path) -> Option<ProbeKey> {
    let meta = std::fs::metadata(program).ok()?;
    Some((program.to_path_buf(), meta.modified().ok(), meta.len()))
}

/// Ask a binary what it is and what it can drive. Cached per (path, mtime, size): the answer
/// changes when the file does, and never otherwise — except that a probe which timed out is not
/// an answer about the file at all (a blocked working directory, a driver that was busy) and is
/// asked again next time rather than remembered as "too old to list its devices".
pub async fn probe_program(program: &Path) -> EngineProbe {
    let key = probe_key(program);
    if let Some(key) = &key
        && let Some(hit) = probe_cache().lock().expect("probe cache").get(key).cloned()
    {
        return hit;
    }
    let (probe, settled) = probe_program_uncached(program).await;
    if let Some(key) = key
        && probe.runs()
        && settled
    {
        probe_cache().lock().expect("probe cache").insert(key, probe.clone());
    }
    probe
}

/// How a probe command ended.
enum Run {
    /// It exited; whether cleanly, and everything it printed.
    Exited(bool, String),
    /// It did not exit within [`PROBE_TIMEOUT`]. Killed.
    TimedOut,
    /// It could not be started.
    Failed(String),
}

async fn run(program: &Path, arg: &str) -> Run {
    let mut command = tokio::process::Command::new(program);
    command
        .arg(arg)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // A probe that outlives its timeout is a hung process nobody will reap.
        .kill_on_drop(true);
    // The binary's own directory as the working directory, never the caller's. An engine
    // reads its working directory at startup (`getcwd`, while looking for backend libraries),
    // and on macOS a working directory under a TCC-protected folder — Downloads, Documents —
    // turns that read into a permission prompt the child cannot answer: measured as a
    // `--version` that never returns. The Studio's own directory is wherever it was started.
    if let Some(dir) = program.parent().filter(|d| d.is_dir()) {
        command.current_dir(dir);
    }
    match tokio::time::timeout(PROBE_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            Run::Exited(output.status.success(), text)
        }
        Ok(Err(e)) => Run::Failed(format!("could not run {}: {e}", program.display())),
        Err(_) => Run::TimedOut,
    }
}

/// The probe, and whether it is a settled fact about the binary (`false` when a step timed out).
async fn probe_program_uncached(program: &Path) -> (EngineProbe, bool) {
    let banner = match run(program, "--version").await {
        Run::Exited(_, text) => text,
        Run::TimedOut => {
            let error = format!("{} --version did not finish within {}s", program.display(), PROBE_TIMEOUT.as_secs());
            return (EngineProbe { version: None, banner: None, devices: None, error: Some(error) }, false);
        }
        Run::Failed(error) => return (EngineProbe { version: None, banner: None, devices: None, error: Some(error) }, true),
    };
    let version = banner.lines().map(str::trim).find(|l| l.starts_with("version:")).map(str::to_string);
    let (devices, settled) = match run(program, "--list-devices").await {
        Run::Exited(true, text) => (parse_list_devices(&text), true),
        // A usage error: the engine predates the flag. That is a fact about the file.
        Run::Exited(false, _) | Run::Failed(_) => (None, true),
        Run::TimedOut => (None, false),
    };
    (EngineProbe { version, banner: Some(banner), devices, error: None }, settled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The listing as a real `llama-server` (build 10330, Metal) printed it, BLAS line included.
    #[test]
    fn a_real_device_listing_parses() {
        let text = "Available devices:\n  BLAS: Accelerate (0 MiB, 0 MiB free)\n  MTL0: Apple M4 Pro (18186 MiB, 18185 MiB free)\n";
        let devices = parse_list_devices(text).expect("a listing");
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].kind, EngineDeviceKind::Accel, "BLAS is not an offload target");
        let gpu = first_gpu(&devices).expect("a GPU");
        assert_eq!(gpu.id, "MTL0");
        assert_eq!(gpu.description, "Apple M4 Pro");
        assert_eq!(gpu.backend, "metal");
        assert_eq!((gpu.total_mib, gpu.free_mib), (Some(18186), Some(18185)));
        assert_eq!(accelerator_tag_for(Some(&devices)), Some("metal"));
    }

    #[test]
    fn other_backends_are_named_by_their_prefix() {
        let text = "Available devices:\n  CUDA0: NVIDIA GeForce RTX 3060 (12288 MiB, 11000 MiB free)\n  Vulkan0: Intel(R) Arc(TM) A770 Graphics (16384 MiB, 16000 MiB free)\n  ROCm0: AMD Radeon RX 7900 XTX (24560 MiB, 24000 MiB free)\n";
        let devices = parse_list_devices(text).expect("a listing");
        let backends: Vec<_> = devices.iter().map(|d| d.backend.as_str()).collect();
        assert_eq!(backends, ["cuda", "vulkan", "rocm"]);
        assert_eq!(devices[1].description, "Intel(R) Arc(TM) A770 Graphics", "parentheses inside a name survive");
        assert_eq!(devices[1].free_mib, Some(16000));
    }

    /// A CPU-only build lists nothing — which must stay distinct from an engine that does not
    /// know the flag at all.
    #[test]
    fn a_cpu_only_build_is_an_empty_list_and_an_old_engine_is_no_list() {
        let cpu_only = parse_list_devices("Available devices:\n").expect("a listing");
        assert!(cpu_only.is_empty());
        assert_eq!(accelerator_tag_for(Some(&cpu_only)), Some("cpu"));

        assert_eq!(parse_list_devices("error: invalid argument: --list-devices\n"), None);
        assert_eq!(accelerator_tag_for(None), None);
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    /// The Metal load, verbatim from a real engine at `-lv 4`.
    #[test]
    fn a_metal_load_is_read_from_the_buffers() {
        let log = lines(
            "0.00.246.082 I llama_prepare_model_devices: using device MTL0 (Apple M4 Pro) (unknown id) - 18185 MiB free\n\
             0.00.353.075 I load_tensors: offloaded 29/29 layers to GPU\n\
             0.00.353.077 I load_tensors:   CPU_Mapped model buffer size =   125.19 MiB\n\
             0.00.353.077 I load_tensors:  MTL0_Mapped model buffer size =  1059.89 MiB\n\
             0.00.354.618 I ggml_metal_init: allocating\n",
        );
        let offload = offload_from_log(&log, Some(99)).expect("an offload report");
        assert_eq!(offload.layers, Some(29));
        assert_eq!(offload.total_layers, Some(29));
        assert_eq!(offload.device.as_deref(), Some("MTL0 (Apple M4 Pro)"));
        assert_eq!(offload.accelerator_bytes, Some((1059.89 * 1024.0 * 1024.0) as u64));
        assert_eq!(offload.cpu_bytes, Some((125.19 * 1024.0 * 1024.0) as u64));
        assert_eq!(offload.evidence, OffloadEvidence::EngineLog);
        assert!(!offload.asked_but_none_landed());
    }

    /// The lie this module exists for: `--device none` still says `offloaded 29/29 layers to GPU`.
    /// Verbatim from a real engine. Every buffer is on the CPU, and that is the answer.
    #[test]
    fn an_offloaded_line_with_no_accelerator_buffer_is_a_cpu_run() {
        let log = lines(
            "0.00.339.472 I load_tensors: offloaded 29/29 layers to GPU\n\
             0.00.339.473 I load_tensors:   CPU_Mapped model buffer size =   877.31 MiB\n\
             0.00.339.474 I load_tensors:   CPU_REPACK model buffer size =   934.14 MiB\n",
        );
        let offload = offload_from_log(&log, Some(99)).expect("an offload report");
        assert_eq!(offload.layers, Some(0), "no device held a byte");
        assert_eq!(offload.device, None);
        assert_eq!(offload.accelerator_bytes, Some(0));
        assert!(offload.asked_but_none_landed());
    }

    /// `--verbose` engines print a dry-run pass first, every buffer at 0.00 MiB. The last pass is
    /// the real one.
    #[test]
    fn the_last_load_pass_wins() {
        let log = lines(
            "load_tensors: offloaded 29/29 layers to GPU\n\
             load_tensors:          CPU model buffer size =     0.00 MiB\n\
             load_tensors:         MTL0 model buffer size =     0.00 MiB\n\
             ggml_metal_init: found device: Apple M4 Pro\n\
             load_tensors: offloaded 29/29 layers to GPU\n\
             load_tensors:   CPU_Mapped model buffer size =   125.19 MiB\n\
             load_tensors:  MTL0_Mapped model buffer size =  1059.89 MiB\n",
        );
        let offload = offload_from_log(&log, Some(99)).expect("an offload report");
        assert_eq!(offload.layers, Some(29));
        assert_eq!(offload.device.as_deref(), Some("MTL0"), "the buffer names the device when no `using device` line does");
    }

    /// A CUDA build's pinned host buffers are CPU memory, whatever the prefix says.
    #[test]
    fn cuda_host_buffers_count_as_cpu() {
        let log = lines(
            "load_tensors: offloaded 20/33 layers to GPU\n\
             load_tensors:   CUDA_Host model buffer size =  2000.00 MiB\n\
             load_tensors:       CUDA0 model buffer size =  4000.00 MiB\n",
        );
        let offload = offload_from_log(&log, Some(20)).expect("an offload report");
        assert_eq!(offload.layers, Some(20));
        assert_eq!(offload.cpu_bytes, Some(2000 << 20));
        assert_eq!(offload.accelerator_bytes, Some(4000 << 20));
        assert_eq!(offload.device.as_deref(), Some("CUDA0"));
    }

    #[test]
    fn a_log_without_the_line_says_nothing_and_the_device_list_fills_in() {
        assert_eq!(offload_from_log(&lines("srv  llama_server: model loaded\n"), Some(9)), None);

        let gpu = parse_list_devices("Available devices:\n  CUDA0: NVIDIA RTX (8192 MiB, 8000 MiB free)\n").unwrap();
        let from_gpu = offload_from_devices(Some(&gpu), Some(9));
        assert_eq!((from_gpu.layers, from_gpu.evidence), (Some(9), OffloadEvidence::DeviceList));
        assert_eq!(from_gpu.device.as_deref(), Some("CUDA0 (NVIDIA RTX)"));

        let none = offload_from_devices(Some(&[]), Some(9));
        assert_eq!(none.layers, Some(0));
        assert!(none.asked_but_none_landed(), "a CPU-only build asked for nine layers gave none");

        let unknown = offload_from_devices(None, Some(9));
        assert_eq!((unknown.layers, unknown.evidence), (Some(9), OffloadEvidence::Unverified));
    }

    /// A program that is not there is an error with the path in it, not a panic and not a cached
    /// answer — the next call must try again, because the user is about to install it.
    #[tokio::test]
    async fn a_missing_program_is_reported_and_not_cached() {
        let path = PathBuf::from("/definitely/not/here/llama-server");
        let probe = probe_program(&path).await;
        assert!(!probe.runs());
        assert!(probe.error.as_deref().is_some_and(|e| e.contains("llama-server")), "{probe:?}");
        assert!(probe_cache().lock().unwrap().keys().all(|k| k.0 != path));
    }
}
