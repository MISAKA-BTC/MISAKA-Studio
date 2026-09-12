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
    /**
     * ADR-0096 Decision 4 — what the app does with a sampling request the lane cannot commit.
     * `greedy_with_notice` sends the request through and prints what ran beside what was asked;
     * `refuse` answers as the gateway does: by name, before the inference.
     */
    sampling_policy: 'greedy_with_notice' | 'refuse'
    /** ADR-0096 Decision 5 — a trim that would drop more turns than this becomes a summary job. */
    summarize_after_turns: number
    /** ADR-0096 Decision 5 — how many continuation legs may follow a `length` finish (0 to 4). */
    continue_max_legs: number
  }
  huggingface: { endpoint: string; token: string | null; max_concurrent_downloads: number }
  ui: { theme: 'system' | 'light' | 'dark'; show_provenance: boolean; show_performance: boolean }
  provenance: { record_inferences: boolean; keep_transcripts: boolean; max_records: number }
  /**
   * ADR-0096 Decision 10 — where the components manifest is (a local path or an `https://` URL;
   * null means the Studio only knows what is on disk) and whether `GET /api/v1/components` fetches
   * it unasked. Optional because a settings file written before the manifest existed has no
   * `components` key, and the runtime fills the default in; a window talking to an older runtime
   * must not crash on its absence.
   */
  components?: { manifest: string | null; auto_check: boolean }
}

// --- ADR-0096 Decision 11: what the running objects were built from ---------

/** JSON as the runtime hands it over untyped: `configured` and `effective` are `serde_json::Value`. */
export type Json = string | number | boolean | null | Json[] | { [key: string]: Json }

/**
 * One subsystem of `GET /api/v1/settings/effective`. `configured` is what the settings the
 * process holds would build; `effective` is read from the RUNNING object and is null when nothing
 * runs (`source.effective` then says why); `source` names, per effective field, the file, flag,
 * environment variable or discovery step that produced it; `since` is when the running object was
 * built (unix seconds); `differs` is the runtime's own verdict — configured and effective disagree
 * in a field the running object's fingerprint covers. The runtime says THAT they differ, not which
 * fields: naming them is this window's job (`lib/effective.ts`).
 */
export type EffectiveSubsystem = {
  configured: Json
  effective: Json | null
  source: Record<string, string>
  since: number | null
  differs: boolean
}

export type EffectiveSettings = {
  backend: EffectiveSubsystem
  node: EffectiveSubsystem
  records: EffectiveSubsystem
  catalog: EffectiveSubsystem
  pool: EffectiveSubsystem
  gateway: EffectiveSubsystem
}

export type EffectiveSubsystemName = keyof EffectiveSettings

// --- ADR-0096 Decision 10: the components table -----------------------------

export type ComponentKind = 'node' | 'cli' | 'worker' | 'gateway' | 'rail' | 'engine' | 'artifact' | 'tokenizer-table' | 'runtime' | 'shell'

/** Which step of the one search order found a file — the runtime's own spellings, verbatim. */
export type ComponentCandidate = 'configured' | 'beside the executable' | 'engines/' | 'models_dir' | 'PATH' | 'not found'

/**
 * Where a component stands against the manifest. `installed-unverified` is found with the right
 * size and no digest computed (that is `?verify=1`); `mismatch` is found with the wrong size or
 * digest — not this component, whatever its name; `not-in-manifest` is found with no row to hold
 * it to; `retired` is an id the node tree stopped building.
 */
export type ComponentState = 'installed' | 'installed-unverified' | 'mismatch' | 'missing' | 'retired' | 'not-in-manifest'

export type ComponentReport = {
  id: string
  kind: ComponentKind
  installed: {
    path: string
    candidate: ComponentCandidate
    found: boolean
    size?: number
    /** Only on `?verify=1` — a class artifact is 34 GiB. */
    sha256?: string
    /** The first line of `--version`, on `?verify=1`, when the binary answered. */
    version?: string
  }
  manifest: { version: string; sha256: string; size: number; url: string; platform: string; member?: string } | null
  state: ComponentState
  /** Why the state is what it is, when a word is not enough (a retirement, a platform, a size). */
  note?: string
}

/** The cross-repository check over the loaded manifest (ADR-0096 invariant 10), by name. */
export type ComponentFinding =
  | { finding: 'missing_spawnable'; id: string; kind: ComponentKind }
  | { finding: 'retired_still_spawned'; id: string; note: string }
  | { finding: 'retired_in_manifest'; id: string; note: string }

export type ManifestStatus = {
  /** `components.manifest`, as configured. */
  source: string | null
  release: string | null
  network: string | null
  loaded: boolean
  /** Why it is not loaded: unset, switched off, unreachable, or refused by the validator. */
  error?: string
  findings: ComponentFinding[]
}

/** `GET /api/v1/components` — `ComponentsView` in the runtime; named for the table here so the
 *  page component can keep the runtime's name. */
export type ComponentsListing = {
  components: ComponentReport[]
  manifest: ManifestStatus
  /** The triple this runtime was built for, so a row's `platform` can be read against it. */
  host_platform: string
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

// --- ADR-0096: what the lane reports beside an answer ----------------------

/** Decision 4: the sampling that ran, printed beside the sampling that was asked for. */
export type MisakaSampling = {
  requested: Record<string, unknown>
  /** The contract says always; optional here because the runtime merges the app's own notice
   *  (`requested`, `reason`) with the gateway's report key by key, and an object that arrived
   *  with only the app's half must render a notice, not blank the window. */
  applied?: { temperature: number; seed: string }
  reason: string
  /** The gateway's one-word account of what ran (`"greedy"`), where it sends one. */
  enforced?: string
  /** Knobs with no consensus rule on this lane (`top_p`, `top_k`, …): named, never dropped. */
  not_a_rule_on_this_lane?: string[]
}

/** Decision 3: the shape that was asked for, and whether the chain enforced it or only checked it. */
export type MisakaFormat = {
  requested: { type: string; constraint_id: string | null }
  /** `committed` — the seat replays the constraint and the court can try it; `masked` — the
   *  decode was constrained on this machine and nothing reached a chain (the local engine);
   *  `advisory` — the schema rode the prompt as text and the answer was validated after the fact. */
  enforcement: 'advisory' | 'committed' | 'masked'
  valid: boolean
  errors: string[]
  canonical_sha256: string | null
}

/** Decision 5: the row is 512 and the answer is what is left of it; this is what was trimmed. */
export type MisakaContext = {
  n_ctx: number
  prompt_tokens_estimate: number
  dropped_turns: number
  /** How many of the dropped turns a summary job covered, when one ran. */
  summarized_turns?: number
}

export type MisakaJobRole = 'summary' | 'answer' | 'continue' | 'tool_leg'

/** Decision 5: one inference is one claim, so a long thread is a chain of jobs — every one listed. */
export type MisakaJob = {
  /** Null for a leg that did not run — such an entry carries `error` instead. */
  fp_job_id: string | null
  fp_claim_id: string | null
  role: MisakaJobRole
  prompt_tokens: number
  decode_tokens: number
  error?: string
}

/**
 * The `misaka` object on the last chunk of a chat completion. Every field is optional on purpose:
 * a GGUF engine sends none of it, and a gateway from before ADR-0096 sends only the job and claim
 * ids. What is absent is simply not shown.
 */
export type MisakaExtension = {
  fp_job_id?: string
  fp_claim_id?: string
  sampling?: MisakaSampling
  format?: MisakaFormat
  context?: MisakaContext
  jobs?: MisakaJob[]
  /** Fields OpenAI defines as having no effect on the answer — accepted and listed (Decision 1). */
  ignored_fields?: string[]
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
  /**
   * What the lane said about this answer (ADR-0096 Decisions 3–5). Kept on the message and
   * persisted like `stats`: a notice that vanished with the window would be a notice nobody read.
   */
  misaka?: MisakaExtension
  /** The tool calls the answer carried (ADR-0096 Decision 2), in OpenAI's shape. The app that
   *  asked is expected to run them; this window only shows them. */
  toolCalls?: unknown[]
}

export type Conversation = {
  id: string
  title: string
  createdAt: number
  updatedAt: number
  modelId: string | null
  messages: ChatMessage[]
}

// --- ADR-0096 Decision 12: conversations are the runtime's -----------------

/** One row of `GET /api/v1/conversations`: enough to draw the list, without the messages. */
export type ConversationSummary = {
  id: string
  title: string
  createdAt: number
  updatedAt: number
  modelId: string | null
  messageCount: number
}

/** The Studio's own export — and the one shape the import takes back unchanged. */
export type ConversationExport = {
  schema: 'misaka-studio/conversations/v1'
  /** Milliseconds since the epoch, like every timestamp in a conversation. */
  exportedAt: number
  conversations: Conversation[]
}

/** What an import did, by name: every skipped row says why (a non-text part, an unknown role). */
export type ConversationImportReport = {
  imported: number
  skipped: { reason: string; count: number }[]
  ids: string[]
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
      /**
       * The part of `rewards_sompi` the *wallet CLI* calls immature. Kept for compatibility, but
       * do not show it as the truth: that CLI never asks the node for its DNS confirmed anchor, so
       * it always applies the slow 600-DAA fallback and reports rewards as locked that the node
       * would let you spend today. `funds` is the pool's own reading of the same rule, with the
       * anchor, and is what the panel shows when present.
       */
      rewards_immature_sompi: number | null
      /** What the node would actually let this address spend right now; null when unreadable. */
      funds?: PoolFunds | null
      min_funding_sompi: number
      blocks_won: number
      /** The blocks this slot's node produced, newest first — read from the node's own log. */
      blocks?: PoolBlock[]
      /** The lottery's odds as the node last stated them; null until the slot has drawn. */
      difficulty?: PoolDifficulty | null
      activity: string[]
      /** The slot's free-prompt lane, when the pool knows about one. */
      fp: PoolFpStatus | null
    }

/**
 * What the slot address holds, split by what the node's mempool would accept a spend of.
 *
 * A mining reward is a coinbase output, and a coinbase clears two layers: the base maturity, and
 * then EITHER the DNS confirmed anchor passing its own block (the fast path) OR the long
 * settlement fallback elapsing. The pool applies that rule itself against the node's anchor —
 * `spendable_sompi` and `waiting_sompi` are absent when the rule's constants have never been
 * observed, because a guessed constant would report real money as locked.
 */
export type PoolFunds = {
  virtual_daa: number
  /** Coinbase paid to this address — the mining rewards. */
  rewards_sompi: number
  /** Ordinary transfers in: funding, faucet grants, change. */
  transfers_sompi: number
  rule: {
    coinbase_maturity_daa: number
    settlement_daa: number
    confirmed_anchor_daa: number | null
    /** Whether the fast path is available; false means every reward waits the full fallback. */
    accelerated: boolean
  } | null
  spendable_sompi?: number
  waiting_sompi?: number
  /** The DAA score at which the first waiting output clears, if any is waiting. */
  waiting_until_daa?: number | null
}

/** A block the pool slot's node produced, as its log announced it. */
export type PoolBlock = {
  hash: string
  ts_ms: number | null
}

/**
 * The slot's lottery, in the node's own numbers: a draw wins a block when it passes the class
 * ticket (`class_ticket_p`) AND the Layer-0 target (`layer0_p`, from the chain's `bits`). The
 * expected draws per block is the product's inverse; the seconds follow from the draw rate the
 * node reports every five minutes.
 */
export type PoolDifficulty = {
  sampled_at_ms: number | null
  draws_this_run: number
  produced_this_run: number
  class_ticket_wins_this_run: number
  class_ticket_p: number | null
  layer0_p: number | null
  bits: number | string | null
  draws_per_block: number | null
  draws_per_s: number | null
  expected_seconds_per_block: number | null
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

/**
 * ADR-0096 Decision 13: the door for a model that does not exist yet. `url` opens the issue form
 * prefilled; `fields` is the form's every field by its own id — `title` and `machine` filled from
 * this machine (RAM, accelerator, the classes it holds), the rest empty for the person to write —
 * shown before the click, because it goes to a public tracker. `machine` is the same facts,
 * structured.
 */
export type ModelRequestPrefill = { url: string; fields: Record<string, string>; machine?: Record<string, unknown> }

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
