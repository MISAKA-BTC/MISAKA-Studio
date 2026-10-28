// The runtime's wire types, mirrored.
//
// Hand-written rather than generated, and kept deliberately narrow: the UI reads a subset of what
// the API returns, and a type that claims every field would have to be regenerated for changes
// the UI does not care about. Everything here corresponds to a Rust type in
// `misaka-studio-core` or `misaka-studio-runtime`.

export type Quantization = {
  label: string
  bits_per_weight: number | null
  family: 'float' | 'legacy' | 'k_quant' | 'i_quant' | 'exotic' | 'unknown'
  tier: 'lossless' | 'recommended' | 'compact' | 'aggressive' | 'unknown'
}

export type ModelSource = {
  repo: string | null
  revision: string | null
  filename: string | null
  base_repo: string | null
  base_revision: string | null
  origin: string | null
}

export type ModelRequirements = {
  weights_bytes: number
  kv_cache_bytes: number
  overhead_bytes: number
  total_bytes: number
  context_tokens: number
}

export type FitVerdict =
  | { verdict: 'fits'; device: string; headroom_bytes: number }
  | { verdict: 'tight'; device: string; headroom_bytes: number }
  | { verdict: 'partial_offload'; device: string; gpu_bytes: number; needed_bytes: number }
  | { verdict: 'does_not_fit'; needed_bytes: number; available_bytes: number }

export type ModelIdentity = {
  h_m: string
  gguf_sha256: string
  gguf_size: number
  filename: string
  base_repo: string
  base_revision: string
}

export type ModelView = {
  id: string
  name: string
  path: string
  size_bytes: number
  quantization: Quantization | null
  architecture: string | null
  parameter_count: number | null
  context_length: number | null
  block_count: number | null
  expert_count: number | null
  kv_cache_bytes_per_token: number | null
  has_chat_template: boolean
  source: ModelSource
  sha256: string | null
  modified_at: number | null
  recommended_context: number
  requirements: ModelRequirements
  fit: FitVerdict
  fit_summary: string
  identity: ModelIdentity | null
}

export type RuntimeDescriptor = {
  backend: string
  engine_commit: string
  engine_patch_sha256: string
  engine_build_number: number
  build_profile: string
  class_tag: string
}

/** Where an offload report's numbers came from — the engine's log, its device list, or nowhere. */
export type OffloadEvidence = 'engine_log' | 'device_list' | 'unverified'

/** What became of the layers the Studio asked the engine to put on the GPU. */
export type Offload = {
  asked: number | null
  /** Layers on the accelerator; 0 is a CPU run, null is "not known". */
  layers: number | null
  total_layers: number | null
  /** The device holding the weights, as the engine names it: `MTL0 (Apple M4 Pro)`. */
  device: string | null
  accelerator_bytes: number | null
  cpu_bytes: number | null
  evidence: OffloadEvidence
}

export type RuntimeStatus = {
  backend: string
  backend_available: boolean
  model_id: string | null
  context_size: number | null
  gpu_layers: number | null
  offload: Offload | null
  /** Why the model is on the CPU when it was not meant to be, or null. */
  offload_note: string | null
  load_ms: number | null
  runtime_hash: string | null
  runtime_class_id: string | null
  model_hash: string | null
  descriptor: RuntimeDescriptor | null
}

export type Availability = { state: 'available'; detail: string } | { state: 'unavailable'; reason: string; remedy: string }

export type BackendInfo = { name: string; selected: boolean; availability: Availability }

// --- image generation -------------------------------------------------------

export type ImageModelRequirements = {
  weights_bytes: number
  activation_bytes: number
  overhead_bytes: number
  total_bytes: number
  width: number
  height: number
  batch: number
}

export type ImageModelView = {
  id: string
  name: string
  path: string
  size_bytes: number
  architecture: string | null
  supports_reference_images: boolean
  source: ModelSource
  sha256: string | null
  modified_at: number | null
  requirements: ImageModelRequirements
  fit: FitVerdict
  fit_summary: string
}

export type ImageRuntimeStatus = {
  backend: string
  backend_available: boolean
  model_id: string | null
  load_ms: number | null
}

export type ImageBackendInfo = { name: string; selected: boolean; availability: Availability }

export type ImageGenerateRequest = {
  prompt: string
  negative_prompt?: string | null
  width?: number
  height?: number
  steps?: number
  guidance_scale?: number
  seed?: number | null
  /** Base64-encoded source images, for multi-reference generation or editing. */
  reference_images?: string[]
  n?: number
}

export type ImageGenerateResponse = {
  /** `data:image/png;base64,...` — ready for an `<img src>`. */
  images: string[]
}

/** One generation the Image tab has run, kept for the gallery. */
export type ImageGeneration = {
  id: string
  prompt: string
  modelId: string | null
  images: string[]
  createdAt: number
}

// --- the engine and its devices -------------------------------------------

/** One device as `llama-server --list-devices` reports it. */
export type EngineDevice = {
  id: string
  description: string
  backend: string
  kind: 'gpu' | 'accel' | 'remote' | 'cpu'
  total_mib: number | null
  free_mib: number | null
}

export type EngineFlavorView = {
  id: string
  label: string
  accelerator: string
  requires: string
  download_bytes: number | null
  recommended: boolean
}

export type InstallState = 'idle' | 'resolving' | 'downloading' | 'extracting' | 'verifying' | 'done' | 'failed'

export type InstalledEngine = {
  path: string
  tag: string
  flavor: string
  accelerator: string
  version: string | null
  installed_at_unix: number
  devices: EngineDevice[] | null
}

export type InstallStatus = {
  state: InstallState
  flavor: string | null
  tag: string | null
  detail: string
  download_ids: string[]
  installed: InstalledEngine | null
  error: string | null
  started_ms: number | null
}

/** `/api/v1/engines`: the engine a load would run, what it can drive, and what could replace it. */
export type EnginesView = {
  program: string
  source: 'configured' | 'beside_app' | 'path' | 'missing'
  version: string | null
  error: string | null
  devices: EngineDevice[] | null
  verdict: 'gpu' | 'cpu_only' | 'unknown' | 'missing'
  summary: string
  hardware_gpu: string | null
  flavors: EngineFlavorView[]
  recommendation: string | null
  release: { tag: string | null; error: string | null }
  install: InstallStatus
  installed: InstalledEngine[]
}

export type Accelerator = {
  kind: 'apple_unified' | 'cuda' | 'rocm' | 'vulkan' | 'cpu'
  name: string
  total_memory: number | null
  free_memory: number | null
  usable_memory: number | null
  driver: string | null
  index: number
}

export type HardwareSnapshot = {
  os: string
  arch: string
  cpu_name: string
  physical_cores: number | null
  logical_cores: number
  total_memory: number
  available_memory: number
  accelerators: Accelerator[]
}

export type SystemInfo = {
  hardware: HardwareSnapshot
  data_dir: string
  models_dir: string
  records_path: string
  catalog_endpoint: string
}

export type AcceleratorSample = {
  index: number
  name: string
  utilization_percent: number | null
  memory_used: number | null
  memory_total: number | null
  temperature_c: number | null
}

export type RuntimeSample = {
  hardware: {
    cpu_percent: number
    process_cpu_percent: number
    memory_used: number
    memory_total: number
    process_memory: number
    accelerators: AcceleratorSample[]
  }
  generation: {
    active: number
    last_tokens_per_second: number
    last_time_to_first_token_ms: number
    total_tokens: number
    total_generations: number
  }
}

export type CatalogEntry = {
  id: string
  downloads: number
  likes: number
  tags: string[]
  last_modified: string | null
  gated: boolean
  pipeline_tag: string | null
}

export type CatalogFile = {
  path: string
  size: number | null
  sha256: string | null
  quantization: Quantization | null
}

export type CatalogRepo = {
  id: string
  revision: string | null
  gated: boolean
  files: CatalogFile[]
  base_model: string | null
}

export type DownloadProgress = {
  id: string
  repo: string
  file: string
  model_id: string
  destination: string
  downloaded: number
  total: number | null
  bytes_per_second: number
  status: 'downloading' | 'verifying' | 'completed' | 'failed' | 'cancelled'
  error: string | null
}

export type Settings = {
  models_dir: string
  /** A model id to load as soon as the runtime is up; null loads nothing. */
  load_on_start: string | null
  server: { host: string; port: number; api_key: string | null; cors_origins: string[] }
  backend: {
    kind: 'auto' | 'llama_cpp' | 'mlx' | 'misaka' | 'gateway' | 'mock'
    llama_server_path: string | null
    mlx_server_path: string | null
    misaka_serve_path: string | null
    misaka_tokenizer_path: string | null
    gpu_layers: { mode: 'auto' } | { mode: 'all' } | { mode: 'none' } | { mode: 'fixed'; layers: number }
    threads: number | null
    flash_attention: 'auto' | 'on' | 'off'
    use_mmap: boolean
    use_mlock: boolean
    extra_args: string[]
    startup_timeout_secs: number
  }
  image_backend: {
    kind: 'none' | 'diffusion'
    python_path: string | null
    diffusion_server_path: string | null
    device: string | null
    startup_timeout_secs: number
  }
  context: {
    /** A GGUF model id to summarise older turns with; null extracts them instead. */
    summarizer_model: string | null
    tokenizer_path: string | null
    fetch_class_tokenizer: boolean
  }
  generation: {
    system_prompt: string
    context_size: number | null
    temperature: number
    top_p: number
    top_k: number
    min_p: number
    repeat_penalty: number
    max_tokens: number
    seed: number | null
  }
  node: {
    /** wRPC Borsh endpoint for the CLI, gateway and rail; null uses the network's first public node. */
    misaka_rpc: string | null
    misaka_cli_path: string | null
    network: 'testnet12' | 'testnet11' | 'devnet' | 'simnet'
    producer_key_path: string | null
    install_default_class_artifact: boolean
    pool_url: string | null
    pool_slot_id: string | null
    pool_slot_token: string | null
    palw_gateway_url: string | null
    mining_mode: MiningMode
    remote: RemoteMinerSettings
    free_prompt: FreePromptStackSettings
  }
  huggingface: { endpoint: string; token: string | null; max_concurrent_downloads: number }
  ui: { theme: 'system' | 'light' | 'dark'; show_provenance: boolean; show_performance: boolean }
  provenance: { record_inferences: boolean; keep_transcripts: boolean; max_records: number }
}

export type InferenceRecord = {
  id: string
  model: ModelIdentity | null
  runtime: { h_r: string; class_id: string; descriptor: RuntimeDescriptor }
  params: {
    temperature: number
    top_p: number
    top_k: number
    min_p: number
    repeat_penalty: number
    max_tokens: number
    seed: number | null
  }
  prompt_commitment: string
  output_commitment: string
  prompt_tokens: number
  completion_tokens: number
  inference_hash: string
  replayability: 'deterministic' | 'seeded_sampling' | 'unrepeatable'
  started_at_unix_ms: number
  duration_ms: number
  time_to_first_token_ms: number | null
  tokens_per_second: number
  prompt?: string
  completion?: string
  model_id?: string
}

/** What the UI records about a completed turn, so a message can show how it was produced. */
export type TurnStats = {
  tokensPerSecond: number
  completionTokens: number
  promptTokens: number
  timeToFirstTokenMs: number | null
  model: string
  finishReason: string
}

export type ChatMessage = {
  id: string
  role: 'system' | 'user' | 'assistant'
  content: string
  /** Set while a response is still streaming. */
  streaming?: boolean
  error?: string
  stats?: TurnStats
  /** Set on a user message that was queued for mining behind the chat. */
  mining?: MessageMining
  /** On an assistant reply: what the context manager sent the model to produce it. */
  context?: ContextReport
  /** When the message was written, in ms. Absent on messages saved before this field existed. */
  createdAt?: number
  /** On a user message: files attached in the composer. Sent to the model after the text. */
  attachments?: Attachment[]
  /** On an assistant reply from a thinking model: the reasoning it produced before answering.
   *  Shown collapsed; never sent back as history. */
  reasoning?: string
  /** How long the model reasoned, in ms, when the reasoning arrived as its own stream. */
  reasoningMs?: number
  /** Agent mode, on an assistant step: the tools it called, in the order its text names them. */
  toolCalls?: ToolCall[]
  /** Agent mode: a user-role message that only carries tool results back to the model. It is
   *  history, not something the person wrote, so the transcript does not draw it as a bubble. */
  toolResults?: boolean
}

// --- the chat surface: composer, attachments, the coding agent --------------

/** A file attached to a user message. Text only: the content is what the model is sent. */
export type Attachment = {
  id: string
  name: string
  /** Bytes of the original file. */
  size: number
  content: string
  /** A highlight.js / fence language guessed from the name. */
  language?: string | null
  /** Set when the content was cut to the attachment limit. */
  truncated?: boolean
  /** Agent mode: a workspace-relative path, when the attachment is an `@`-mentioned file. */
  path?: string | null
}

/** What a conversation is: a chat with the loaded model, or a coding agent working in a folder. */
export type ConversationMode = 'chat' | 'agent'

/**
 * How much the coding agent may do without asking.
 *
 * - `read_only` — list, read and search only; no edits, no commands (Cursor's "Ask").
 * - `ask` — edits and commands each wait for approval.
 * - `auto_edit` — edits apply at once (every one can be reverted from the Changes panel); commands
 *   wait for approval. The default.
 * - `full_auto` — edits and commands both run without asking.
 */
export type ApprovalPolicy = 'read_only' | 'ask' | 'auto_edit' | 'full_auto'

export type ToolCallStatus = 'pending' | 'awaiting_approval' | 'running' | 'done' | 'error' | 'denied'

/** What a tool call shows in the transcript. Kept small: a diff, not two copies of the file. */
export type ToolDisplay =
  | { kind: 'text'; text: string }
  | { kind: 'diff'; path: string; patch: string; added: number; removed: number; created: boolean }
  | { kind: 'command'; command: string; exitCode: number | null; stdout: string; stderr: string; timedOut: boolean; durationMs: number }
  | { kind: 'matches'; pattern: string; matches: WorkspaceMatch[]; truncated: boolean }
  | { kind: 'files'; entries: WorkspaceEntry[]; truncated: boolean }
  | { kind: 'read'; path: string; startLine: number; endLine: number; totalLines: number }

/** One tool call an agent step made. */
export type ToolCall = {
  id: string
  name: string
  args: Record<string, unknown>
  status: ToolCallStatus
  /** One line for the card's header, e.g. "Read src/main.rs · lines 1–120". */
  summary?: string
  display?: ToolDisplay
  /** The text returned to the model, exactly as sent. */
  output?: string
  error?: string
  startedAt?: number
  finishedAt?: number
}

/** A file the agent changed in a conversation, with what it was before — so it can be reverted. */
export type FileChange = {
  /** Workspace-relative path. */
  path: string
  /** Content before the agent's first change; `null` when the agent created the file. */
  original: string | null
  /** Content now; `null` when the file was removed. */
  current: string | null
  /** Set when the original was too large to keep: the change is listed but cannot be reverted. */
  untracked?: boolean
  updatedAt: number
}

// --- the workspace API (runtime `/api/v1/workspace/*`) ----------------------

export type WorkspaceInfo = { root: string; name: string; is_git: boolean; branch: string | null }
export type BrowseEntry = { name: string; path: string; is_dir: boolean; is_git: boolean }
export type BrowseView = { path: string; parent: string | null; home: string; entries: BrowseEntry[] }
export type WorkspaceEntry = { path: string; is_dir: boolean; size: number | null }
export type WorkspaceTree = { root: string; path: string; entries: WorkspaceEntry[]; truncated: boolean }
export type WorkspaceFile = {
  path: string
  content: string
  start_line: number
  end_line: number
  total_lines: number
  size: number
  truncated: boolean
  binary: boolean
}
export type WorkspaceMatch = { path: string; line: number; text: string }
export type WorkspaceSearch = { matches: WorkspaceMatch[]; truncated: boolean; files_scanned: number }
export type WorkspaceWrite = { path: string; created: boolean; previous: string | null; content: string; bytes: number }
export type WorkspaceEdit = { path: string; previous: string; content: string; replacements: number }
export type WorkspaceExec = {
  command: string
  exit_code: number | null
  stdout: string
  stderr: string
  timed_out: boolean
  duration_ms: number
  truncated: boolean
}

/** One file in `git status`, with its line counts against HEAD. */
export type GitFile = {
  /** Workspace-relative, `/`-separated. */
  path: string
  /** The path before a rename or copy. */
  orig_path: string | null
  /** Porcelain v2's two status letters: the index (staged) side and the worktree side; `.` = unchanged. */
  index: string
  worktree: string
  status: 'modified' | 'added' | 'deleted' | 'renamed' | 'copied' | 'untracked' | 'conflicted' | 'typechange'
  /** Lines added / removed against HEAD (untracked: every line counts as added). `null` for binary files. */
  added: number | null
  removed: number | null
  binary: boolean
}

/** `git status` of a workspace — or `is_git: false` for a folder that is not a repository. */
export type GitStatus = {
  is_git: boolean
  branch: string | null
  /** Short commit id of HEAD; `null` in a repository with no commits yet. */
  head: string | null
  upstream: string | null
  ahead: number
  behind: number
  files: GitFile[]
  /** Totals over `files`. */
  added: number
  removed: number
}

export type GitDiff = { patch: string; truncated: boolean }

/** One event of a streamed command (`/api/v1/workspace/exec/stream`). */
export type ExecStreamEvent =
  | { type: 'stdout'; data: string }
  | { type: 'stderr'; data: string }
  | { type: 'exit'; exit_code: number | null; timed_out: boolean; duration_ms: number }
  | { type: 'error'; message: string }

/** How a count of tokens was made: the model's own tokenizer, or an upper-bound estimate. */
export type CounterSource = { kind: 'tokenizer'; path: string } | { kind: 'estimate' }

export type MemoryReport = {
  source: 'summary' | 'extract'
  messages_covered: number
  tokens: number
  note: string | null
}

/** The context manager's account of one request (runtime `context::ContextReport`). */
export type ContextReport = {
  /** False when the conversation fitted as it was. */
  managed: boolean
  /** The engine that was sent the request: `llamacpp`, `misaka`, `gateway`… */
  backend: string
  window: number
  answer_reserve: number
  prompt_budget: number
  prompt_tokens: number
  counter: CounterSource
  pinned_included: number
  pinned_omitted: number
  recent_messages: number
  older_messages: number
  memory: MemoryReport | null
  continuation: boolean
  question_over_budget: boolean
  /** The messages exactly as sent, when the manager changed them. */
  sent: { role: string; content: string }[] | null
}

export type Conversation = {
  id: string
  title: string
  createdAt: number
  updatedAt: number
  modelId: string | null
  messages: ChatMessage[]
  /** Standing facts for this conversation, kept in every request ahead of history. */
  pinned?: string[]
  /** Chat (the default) or the coding agent. */
  mode?: ConversationMode
  /** Agent mode: the absolute path of the folder the agent works in. */
  workspace?: string | null
  /** Agent mode: what the agent may do without asking. Default `auto_edit`. */
  approval?: ApprovalPolicy
  /** Kept at the top of the sidebar. (Not to be confused with `pinned`, which are notes.) */
  starred?: boolean
  /** Agent mode: the files this conversation's agent changed, for review and revert. */
  changes?: FileChange[]
  /** Whether this conversation's prompts are queued for prompt mining when background mining is
   *  on. Absent means on. In agent mode only what the person typed is mined — never file contents
   *  or tool output, which may be private code. */
  mine?: boolean
}

// --- the Network tab -------------------------------------------------------

export type PalwArtifactSource =
  | { kind: 'derived_from_seed' }
  | { kind: 'download'; filename: string; repo_path: string; sha256: string; size_bytes: number; hf_repo: string; convert_command: string }
  | { kind: 'convert_locally'; filename: string; approx_size_bytes: number; source_repo: string; convert_command: string }

export type PalwClassReadiness =
  | { state: 'ready_built_in' }
  | { state: 'artifact_present'; path: string; size_bytes: number; verified: boolean }
  | { state: 'artifact_missing'; downloadable: boolean }
  | { state: 'artifact_mismatch'; path: string; size_bytes: number; expected_bytes: number }

/** The shape block of a class artifact on disk, read from its first 64 bytes. */
export type PalwArtifactHeader = {
  n_layers: number
  n_heads: number
  n_kv_heads: number
  d_head: number
  d_ff: number
  vocab: number
  /** Positions the file's rotary table covers: the most any class can run this file at. */
  max_position: number
}

export type PalwClassStatus = {
  spec: {
    name: string
    description: string
    /** Genesis-table leftover. Live share is Final work (ADR-0137), not this number. */
    share_permille: number
    class_id_hex: string
    class_id_complete: boolean
    artifact_root_hex: string
    artifact: PalwArtifactSource
    is_base: boolean
    /** The n_ctx the class was registered at: 12, 512, 2M… A registration choice, not the weights'. */
    context_tokens: number
  }
  readiness: PalwClassReadiness
  memory_note: string | null
  /** Null when no file is on disk or its format is not one the runtime reads a header from. */
  artifact_header: PalwArtifactHeader | null
}

export type NodeStatus = {
  reachable: boolean
  rpc_url: string
  source: string
  server_version: string | null
  network: string | null
  is_synced: boolean | null
  virtual_daa_score: number | null
  block_count: number | null
  header_count: number | null
  difficulty: number | null
  peer_count: number | null
  mempool_size: number | null
  sink: string | null
  sink_timestamp_ms: number | null
  sink_algo_id: number | null
  sink_stand_down_secs: number | null
  error: string | null
}

export type NodeClassRow = {
  class_id: string
  base: boolean
  status: string
  share_permille: number | null
  budget_blocks: number | null
  canonical_leaves: number | null
}

export type NodeBlocker =
  | { kind: 'stale_chain_data'; said: string }
  | { kind: 'refused_arguments'; said: string }

export type MiningState =
  | { state: 'not_mining' }
  | { state: 'starting'; holding: string | null }
  | { state: 'producing'; blocks: number; latest_number: number | null }

export type NodeView = {
  status: NodeStatus
  role: 'observer' | 'verifier' | 'producer'
  command_line: string[] | null
  classes_from_node: NodeClassRow[]
  activity: string[]
  blocker: NodeBlocker | null
  mining: MiningState
  pay_address: string | null
  registered_bond: string | null
  pay_balance_sompi: number | null
  rewards: Rewards | null
  effort: Effort | null
}

/** What the producer is doing while it has won nothing: its own draw counter. */
export type Effort = {
  draws: number
  produced: number
  ticket_one_in: number | null
  /** Class tickets won this run: blocks produced plus tickets that then lost the network's draw. */
  ticket_wins: number
  /** Draws per minute between the last two reports of this run; null for a run's first report. */
  draws_per_min: number | null
}

/** What the chain has actually paid this producer, from the node's own utxo index. */
/** A block this machine produced, as the chain describes it now. */
export type ProducedBlock = {
  hash: string
  seen_at_ms: number
  found: boolean
  daa_score: number | null
  algo_id: number | null
  is_chain_block: boolean | null
  timestamp_ms: number | null
  paid_to_me_sompi: number | null
}

export type Rewards = {
  blocks_paid: number
  total_sompi: number
  spendable_sompi: number
  maturing_sompi: number
  next_mature_daa: number | null
}

export type NetworkOverview = {
  network: 'testnet12' | 'testnet11' | 'devnet' | 'simnet'
  /** The chain as each public node reports it — one node's word apiece. */
  public_nodes: NodeStatus[]
  classes: PalwClassStatus[]
}

/** The gateway's own account of itself: the runtime it runs and the identity it answers for. */
export type GatewayHealth = {
  runtime_manifest_hash: string
  template_id: string
  class_id: string | null
  bond: string | null
  operator_id: string | null
}

/**
 * Three answers, not two. `unknown` is what a catalog of documented prefixes can honestly say
 * about an id it does not hold in full — a prefix that fails to match rules nothing out.
 */
export type ClassMatch =
  | { state: 'registered'; name: string }
  | { state: 'not_registered' }
  | { state: 'unknown'; complete_ids: number; total_classes: number }

export type PromptMiningStatus = {
  gateway_url: string
  unreachable: string | null
  health: GatewayHealth | null
  class: ClassMatch | null
}

/** How far a commitment got. Today there is one value, and its name is the whole truth. */
export type ChainReach = 'committed_not_submitted'

/** Where a chat's mining happens: in the chat (it waits for the lane) or behind it (a queue). */
export type MiningMode = 'inline' | 'background'

export type MiningJobStatus = 'queued' | 'running' | 'committed' | 'refused' | 'failed'

/** One prompt's passage through the slot's lane, as the runtime's queue records it. */
export type MiningJob = {
  id: string
  conversation_id: string | null
  message_id: string | null
  prompt: string
  created_ms: number
  status: MiningJobStatus
  attempts: number
  not_before_ms: number
  started_ms: number | null
  finished_ms: number | null
  fp_job_id: string | null
  claim_id: string | null
  /** The mined answer — the worker's, which need not match what the chat engine said. */
  answer: string | null
  prompt_tokens: number | null
  completion_tokens: number | null
  /** The lane's own words for a refusal or the last failure. */
  error: string | null
  gateway_url: string
}

export type MiningQueueView = {
  mode: MiningMode
  /** Whether `background` can be honoured now: a gateway is configured AND another engine can chat. */
  background_available: boolean
  background_blocker: string | null
  gateway_url: string | null
  counts: { queued: number; running: number; committed: number; refused: number; failed: number }
  jobs: MiningJob[]
}

/** What a chat message knows about its own mining: the job it was queued as. */
export type MessageMining = {
  jobId: string
  status: MiningJobStatus
  claimId?: string | null
  error?: string | null
  /** The mined answer, once there is one — kept on the message so it survives the queue's trim. */
  answer?: string | null
}

export type PromptMiningRun = {
  answer: string
  cu: string
  fp_job_id: string
  trace_root: string
  output_root: string
  schedule_root: string
  artifact: string
  prompt_tokens: number | null
  completion_tokens: number | null
  chain: ChainReach
}

/// **ADR-0090: what opening a line's market needs, against what the joined slot has.**
///
/// `already_seeded` is deliberately three-valued. `null` means the chain could not be asked, which
/// is NOT "not seeded": the next thing a person does with that answer is lock a hundred thousand
/// MSK forever, so an unreachable node must not read as an open door.
export interface SeedReadiness {
  line_id: string | null
  from_address: string | null
  spendable_sompi: number | null
  seed_min_sompi: number
  short_by_sompi: number | null
  can_seed: boolean
  blocked_because: string | null
  already_seeded: boolean | null
}

export interface SeedOutcome {
  submitted: boolean
  txid: string | null
  detail: string
}

/// What registering this machine's artifact as a class needs. A checklist, because registration is
/// a node's act and every `false` here is something the node would refuse on at startup.
export interface RegistrationReadiness {
  artifact: string | null
  bond: string | null
  fee_outpoint: string | null
  has_key: boolean
  can_register: boolean
  blocked_because: string | null
  armed: boolean
  command: string[]
}

/** The node-free miner (`palw-remote-miner` + `palw-attempt-exec`, RFC-0009 stage A). */
export interface RemoteMinerSettings {
  remote_miner_path: string | null
  attempt_exec_path: string | null
  /** wRPC Borsh endpoints; empty uses the network's published list. */
  rpc: string[]
  bond: string | null
  /** Class id (128 hex); null mines the floor. */
  class: string | null
  artifact: string | null
  pay_address: string | null
  key_path: string | null
  /** `<daa>:<block hash>`, the trust root the nodes are held to. */
  checkpoint: string | null
  state_dir: string | null
  verify_headers: boolean
  accept_unverified: 'HEADER_VERIFIED_FORK_CHOICE_UNVERIFIED' | 'UNVERIFIED_REMOTE' | null
  poll_secs: number | null
}

export interface PublicWrpc {
  name: string
  wrpc_borsh: string
  wrpc_json: string
}

export interface RemoteMinerFacts {
  mode: string | null
  verification: string | null
  draws_lost: number
  published: number
  halts: number
  last_halt: string | null
  last_fatal: string | null
  blocks: string[]
  events: { event: string; detail: Record<string, unknown> }[]
}

export interface RemoteMinerView {
  running: boolean
  exited: string | null
  command_line: string[] | null
  planned: { Ok: string[] } | { Err: string }
  state_dir: string
  facts: RemoteMinerFacts
  public_wrpc: PublicWrpc[]
  label: string
}

export interface RemoteNodeRead {
  endpoint: string
  reachable: boolean
  network: string | null
  virtual_daa_score: number | null
  sink: string | null
  error: string | null
}

export interface RemoteMinerPreflight {
  nodes: RemoteNodeRead[]
  quorum: boolean
  remote_miner: string
  remote_miner_found: boolean
  attempt_exec: string
  attempt_exec_found: boolean
  command_error: string | null
  problems: string[]
  label: string
}

export interface CheckpointSuggestion {
  checkpoint: string
  daa_score: number
  block_hash: string
  read_from: string
  confirmed_by: string[]
  note: string
}

/** The node-free prompt-mining stack: `misaka-palw-gateway` + `misaka-palw-fp-rail --watch`, both on the public wRPC. */
export interface FreePromptStackSettings {
  gateway_path: string | null
  rail_path: string | null
  worker_path: string | null
  listen: string
  artifact: string | null
  tokenizer: string | null
  bond: string | null
  evidence_provider: string | null
  accept_unverified: boolean
}

export interface StackProcess {
  running: boolean
  exited: string | null
  command_line: string[] | null
}

export interface PromptStackView {
  gateway: StackProcess
  rail: StackProcess
  planned: { Ok: string[][] } | { Err: string }
  carriers: Record<string, unknown>[]
  gateway_log: string[]
  rail_log: string[]
  identity: string
  outbox: string
  label: string
}

export interface PromptStackPreflight {
  paths: Record<string, string | null>
  binaries: [string, boolean][]
  artifact_present: boolean
  tokenizer_present: boolean
  key_present: boolean
  public_node: NodeStatus | null
  evidence_provider_reachable: boolean
  problems: string[]
}

/** One file of a published PALWART3 artifact. */
export interface PalwartFile {
  repo_path: string
  sha256: string
  size_bytes: number
}

/** A PALW model published as PALWART3 that this network has not registered as a class. */
export interface PublishedPalwartView {
  model: {
    name: string
    description: string
    hf_repo: string
    revision: string
    artifact_root_hex: string
    family: string
    served_context: number
    license: string
    fit: string
    index: PalwartFile
    segments: PalwartFile[]
  }
  present: boolean
  index_path: string
  total_bytes: number
  registered: boolean
}
