// Types of the Collector API. Source of truth: docs/collector-api.md.

export type Status = 'up' | 'degraded' | 'down';

export interface Totals {
  requests: number;
  prompt_tokens: number;
  predicted_tokens: number;
  cached_tokens: number;
  draft_tokens: number;
  draft_accepted: number;
  structured_requests: number;
  cache_hits: number;
  cache_misses: number;
  cache_evicted: number;
  cache_rewrites: number;
  cache_disk_hits: number;
  cache_disk_cut_backs: number;
}

export interface Sample {
  t: number;
  status: Status;
  busy_slots: number | null;
  queued: number | null;
  kv_used: number | null;
  kv_reserved: number | null;
  kv_use: number | null;
  gen_speed: number | null;
  throughput: number | null;
  prefill_speed: number | null;
  prefill_throughput: number | null;
  requests_per_min: number | null;
  mean_service_time: number | null;
  /** Estimated time to first token, s: prefill seconds per request. */
  ttft: number | null;
  prompt_cache_hit_rate: number | null;
  draft_acceptance: number | null;
  // Halogen 0.17.5+. Null when the series is missing (an old Halogen) or undefined.
  /** Run time of the oldest running request, s. */
  busy_for: number | null;
  kv_waiting: number | null;
  cache_entries: number | null;
  cache_bytes: number | null;
  /** Age of the engine's last cache report, s. Cache values can lag by this much. */
  cache_stats_age: number | null;
  /** 0 or 1. */
  cache_disk_enabled: number | null;
  cache_disk_bytes: number | null;
  cache_disk_budget: number | null;
  /** Share of prompt-cache lookups that found an entry. Not the Prompt-cache hit rate (token share). */
  cache_lookup_hit_rate: number | null;
  cache_evictions_per_min: number | null;
  cache_rewrites_per_min: number | null;
  cache_disk_hits_per_min: number | null;
  cache_disk_cut_backs_per_min: number | null;
  // Host hardware, read by the Collector. Null where the host has no such sensor.
  /** Shares, 0 to 1. */
  cpu_use: number | null;
  gpu_use: number | null;
  /** °C: CPU Tctl, GPU edge, NVMe composite. */
  cpu_temp: number | null;
  gpu_temp: number | null;
  nvme_temp: number | null;
  /** APU package power (CPU and GPU), W. */
  apu_power: number | null;
  /** GPU shader clock, MHz. */
  gpu_clock: number | null;
  /** GPU memory (VRAM + GTT) and system RAM, bytes. */
  gpu_mem_used: number | null;
  gpu_mem_total: number | null;
  ram_used: number | null;
  ram_total: number | null;
  totals: Totals | null;
}

export interface NpuInfo {
  status: string;
  models: string[];
  loaded: string[];
  queued: number;
  passes: number;
  contexts: number;
  error?: string;
}

export interface Info {
  halogen_url: string;
  model: string;
  version_api: string;
  version_engine: string;
  slots: number;
  slot_ctx: number;
  kv_pool: number;
  drafter_default: string;
  prompt_cache_cap_mb: number;
  engine_responds: boolean;
  engine_probe_s: number;
  npu: NpuInfo | null;
  uptime_since: number | null;
  last_scrape_ok: number | null;
}

export interface CacheInfo {
  t: number;
  entries: number;
  max_entries: number;
  bytes: number;
  hits: number;
  misses: number;
  evicted: number;
  refused: number;
  hit_rate: number;
  token_hit_rate: number;
  prompt_tokens_saved: number;
  pool: {
    positions: number;
    used: number;
    busy_regions: number;
    held_regions: number;
    waiting_for_room: number;
    relocated: number;
  };
}

export type EventKind = 'restart' | 'outage_start' | 'outage_end' | 'status_change';

export interface HEvent {
  t: number;
  kind: EventKind;
  from?: Status;
  to?: Status;
  detail?: string;
}

export interface Snapshot {
  info: Info | null;
  cache: CacheInfo | null;
  latest: Sample | null;
  events: HEvent[];
}

/** Numeric columns of a Series, in the order the page uses them. */
export const SERIES_FIELDS = [
  'busy_slots',
  'queued',
  'kv_use',
  'kv_reserved_use',
  'gen_speed',
  'throughput',
  'prefill_speed',
  'prefill_throughput',
  'requests_per_min',
  'mean_service_time',
  'prompt_cache_hit_rate',
  'draft_acceptance',
  'busy_for',
  'kv_waiting',
  'cache_entries',
  'cache_bytes',
  'cache_lookup_hit_rate',
  'cache_evictions_per_min',
  'cache_rewrites_per_min',
  'cache_disk_hits_per_min',
  'cache_disk_cut_backs_per_min',
  'cache_disk_bytes',
  'cpu_use',
  'cpu_temp',
  'gpu_use',
  'gpu_temp',
  'nvme_temp',
  'apu_power',
  'gpu_mem_used',
  'ram_used',
  'ttft',
  'busy_slots_max',
  'queued_max',
  'kv_use_max',
  'busy_for_max',
  'kv_waiting_max',
  'cpu_temp_max',
  'gpu_temp_max',
  'apu_power_max',
] as const;

export type SeriesField = (typeof SERIES_FIELDS)[number];

export type Column = (number | null)[];

export type SeriesColumns = { [K in SeriesField]: Column };

export interface Series extends SeriesColumns {
  from: number;
  to: number;
  step: number;
  source: Source;
  t: number[];
  status: (Status | null)[];
  totals_in_range: Totals;
  /** Mean, lowest and highest Sample value of each field over the range. */
  summary?: Summary;
}

/** raw: Samples (48 h). rollup: one-minute Rollups (30 days). hourly: one year. daily: all time. */
export type Source = 'raw' | 'rollup' | 'hourly' | 'daily';

export interface Stat {
  mean: number | null;
  min: number | null;
  max: number | null;
}

export type Summary = Partial<Record<SeriesField, Stat>>;
