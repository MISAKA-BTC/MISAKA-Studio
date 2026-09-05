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

export type RuntimeStatus = {
  backend: string
  backend_available: boolean
  model_id: string | null
  context_size: number | null
  gpu_layers: number | null
  load_ms: number | null
  runtime_hash: string | null
  runtime_class_id: string | null
  model_hash: string | null
  descriptor: RuntimeDescriptor | null
}

export type Availability = { state: 'available'; detail: string } | { state: 'unavailable'; reason: string; remedy: string }

export type BackendInfo = { name: string; selected: boolean; availability: Availability }

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
    kaspad_path: string | null
    rpc_url: string | null
    network: 'testnet11' | 'devnet' | 'simnet'
    role: 'observer' | 'verifier' | 'producer'
    mining_address: string | null
    producer_key_path: string | null
    producer_bond: string | null
    fee_outpoint: string | null
    producer_class: string | null
    class_artifact: string | null
    appdir: string | null
    extra_args: string[]
    install_default_class_artifact: boolean
    pool_url: string | null
    pool_slot_id: string | null
    pool_slot_token: string | null
    palw_gateway_url: string | null
    mining_mode: MiningMode
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
}

export type Conversation = {
  id: string
  title: string
  createdAt: number
  updatedAt: number
  modelId: string | null
  messages: ChatMessage[]
}

// --- the Network tab -------------------------------------------------------

export type PalwArtifactSource =
  | { kind: 'derived_from_seed' }
  | { kind: 'download'; filename: string; repo_path: string; sha256: string; size_bytes: number; hf_repo: string; convert_command: string }
  | { kind: 'convert_locally'; extension: string; approx_size_bytes: number; source_repo: string; convert_command: string }

export type PalwClassReadiness =
  | { state: 'ready_built_in' }
  | { state: 'artifact_present'; path: string; size_bytes: number; verified: boolean }
  | { state: 'artifact_missing'; downloadable: boolean }
  | { state: 'artifact_mismatch'; path: string; size_bytes: number; expected_bytes: number }

export type PalwClassStatus = {
  spec: {
    name: string
    description: string
    share_permille: number
    class_id_hex: string
    class_id_complete: boolean
    artifact_root_hex: string
    artifact: PalwArtifactSource
    is_base: boolean
  }
  readiness: PalwClassReadiness
  memory_note: string | null
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
  role: 'observer' | 'verifier' | 'producer'
  network: 'testnet11' | 'devnet' | 'simnet'
  node: NodeView
  classes: PalwClassStatus[]
  kaspad_found: boolean
  kaspad_path: string
}

// --- the miner pool --------------------------------------------------------

export type PoolStatus =
  | { joined: false; default_url: string }
  | {
      joined: true
      pool_url: string
      seed_path: string
      slot_id: string
      address: string
      phase: string
      bond_outpoint: string | null
      fee_outpoint: string | null
      balance_sompi: number | null
      /** Coinbase paid to the slot address so far — the mining rewards the chain has already
       *  handed over (an attempt block's reward is escrowed until its claim is Final, and shows up
       *  here only then). Null when the pool's node could not be asked. */
      rewards_sompi: number | null
      /** The part of `rewards_sompi` that is still maturing and cannot be spent yet. */
      rewards_immature_sompi: number | null
      min_funding_sompi: number
      blocks_won: number
      activity: string[]
      /** The slot's free-prompt lane, when the pool knows about one. */
      fp: PoolFpStatus | null
    }

/** What a slot's free-prompt lane is doing: the chat that mines, on that slot's own bond. */
export type PoolFpStatus = {
  mode: 'floor' | 'fp' | string
  class: string
  gateway_running: boolean
  submitter_running: boolean
  claims_submitted: number
  bond_exposure_ceiling: string | null
  bond_claim_exposure: string | null
  fp_certified: boolean | null
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
