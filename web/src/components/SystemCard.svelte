<script lang="ts">
  // One full-width card with the host's activity at a glance, laid out like btop/nvtop: a box per device with its
  // live number, a bar where there is a whole, and the last 5 minutes as a line in the device's own colour.
  import { bytes, int, pct, rate, DASH } from '../lib/format';
  import { dash } from '../lib/store.svelte';
  import type { ColorVar } from '../lib/charts';
  import type { SeriesField } from '../lib/types';
  import Meter from './Meter.svelte';
  import Spark from './Spark.svelte';

  const s = $derived(dash.latest);
  const share = (used: number | null | undefined, total: number | null | undefined) =>
    used != null && total ? used / total : null;
  const deg = (v: number | null | undefined) => (v == null ? null : `${rate(v)} °C`);
  const perSec = (v: number | null | undefined) => (v == null ? DASH : `${bytes(v)}/s`);
  const tone = (v: number | null) => (v === null ? 'none' : v >= 0.95 ? 'crit' : v >= 0.85 ? 'warn' : 'none');

  interface Line {
    label: string;
    value: string;
    field: SeriesField;
    color: ColorVar;
  }
  interface Box {
    name: string;
    /** Big live number. */
    value: string;
    /** Share 0..1 for the bar; null = no bar (traffic has no whole). */
    bar: number | null;
    details: (string | null)[];
    /** One line each; the first also drives the big number. */
    lines: Line[];
    max: number | null;
  }

  const boxes = $derived.by((): Box[] => {
    if (!s) return [];
    const out: Box[] = [
      {
        name: 'CPU',
        value: pct(s.cpu_use),
        bar: s.cpu_use,
        details: [deg(s.cpu_temp)],
        lines: [{ label: 'use', value: pct(s.cpu_use), field: 'cpu_use', color: '--s2' }],
        max: 1,
      },
      {
        name: 'GPU',
        value: pct(s.gpu_use),
        bar: s.gpu_use,
        details: [
          deg(s.gpu_temp),
          s.gpu_clock == null ? null : `${int(s.gpu_clock)} MHz`,
          s.apu_power == null ? null : `APU ${rate(s.apu_power)} W`,
        ],
        lines: [{ label: 'use', value: pct(s.gpu_use), field: 'gpu_use', color: '--s1' }],
        max: 1,
      },
    ];
    if (s.npu_use != null) {
      const loaded = dash.info?.npu?.loaded ?? [];
      out.push({
        name: 'NPU',
        value: pct(s.npu_use),
        bar: s.npu_use,
        details: [loaded.length ? loaded.join(', ') : null],
        lines: [{ label: 'use', value: pct(s.npu_use), field: 'npu_use', color: '--s3' }],
        max: 1,
      });
    }
    out.push(
      {
        name: 'RAM',
        value: bytes(s.ram_used),
        bar: share(s.ram_used, s.ram_total),
        details: [s.ram_total == null ? null : `of ${bytes(s.ram_total)}`],
        lines: [{ label: 'used', value: bytes(s.ram_used), field: 'ram_used', color: '--s5' }],
        max: s.ram_total ?? null,
      },
      {
        name: 'GPU memory',
        value: bytes(s.gpu_mem_used),
        bar: share(s.gpu_mem_used, s.gpu_mem_total),
        details: [s.gpu_mem_total == null ? null : `of ${bytes(s.gpu_mem_total)}`],
        lines: [{ label: 'used', value: bytes(s.gpu_mem_used), field: 'gpu_mem_used', color: '--s1' }],
        max: s.gpu_mem_total ?? null,
      },
      {
        name: 'SSD',
        value: perSec((s.ssd_read ?? 0) + (s.ssd_write ?? 0)),
        bar: null,
        details: [deg(s.nvme_temp)],
        lines: [
          { label: 'read', value: perSec(s.ssd_read), field: 'ssd_read', color: '--s1' },
          { label: 'write', value: perSec(s.ssd_write), field: 'ssd_write', color: '--s2' },
        ],
        max: null,
      },
    );
    if (s.hdd_read != null) {
      out.push({
        name: 'HDD',
        value: perSec((s.hdd_read ?? 0) + (s.hdd_write ?? 0)),
        bar: null,
        details: [],
        lines: [
          { label: 'read', value: perSec(s.hdd_read), field: 'hdd_read', color: '--s3' },
          { label: 'write', value: perSec(s.hdd_write), field: 'hdd_write', color: '--s4' },
        ],
        max: null,
      });
    }
    out.push({
      name: 'Network',
      value: perSec((s.net_in ?? 0) + (s.net_out ?? 0)),
      bar: null,
      details: [],
      lines: [
        { label: 'in', value: perSec(s.net_in), field: 'net_in', color: '--s5' },
        { label: 'out', value: perSec(s.net_out), field: 'net_out', color: '--s4' },
      ],
      max: null,
    });
    return out;
  });
</script>

<article class="card system">
  <header>
    <h2>System</h2>
    <span class="aside">last 5 minutes</span>
  </header>
  {#if boxes.length}
    <div class="grid">
      {#each boxes as b (b.name)}
        <section class="box" aria-label={b.name}>
          <div class="head">
            <h3>{b.name}</h3>
            <span class="big num">{b.value}</span>
          </div>
          {#if b.bar !== null}
            <Meter value={b.bar} label="{b.name} use" tone={tone(b.bar)} />
          {/if}
          {#if b.details.some((d) => d)}
            <p class="details num">{b.details.filter((d) => d).join(' · ')}</p>
          {/if}
          {#each b.lines as l (l.field)}
            {#if b.lines.length > 1}
              <p class="line num"><i style="background: var({l.color})" aria-hidden="true"></i>{l.label} {l.value}</p>
            {/if}
            <Spark field={l.field} max={b.max} color={l.color} />
          {/each}
        </section>
      {/each}
    </div>
  {:else}
    <p class="none">No Sample yet.</p>
  {/if}
</article>

<style>
  .system {
    padding: var(--sp-3) var(--sp-4) var(--sp-4);
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }
  .aside {
    font-size: var(--fs-xs);
    color: var(--faint);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(190px, 1fr));
    gap: var(--sp-3);
  }
  .box {
    border: 1px solid var(--line-soft);
    border-radius: var(--r-card);
    padding: var(--sp-2) var(--sp-3);
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-2);
  }
  h3 {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--muted);
    font-weight: 600;
  }
  .big {
    font-size: var(--fs-md);
    font-weight: 600;
    color: var(--text);
    white-space: nowrap;
  }
  .details,
  .line {
    margin: 0;
    font-size: var(--fs-xs);
    color: var(--faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .line i {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 2px;
    margin-right: 5px;
  }
  .none {
    color: var(--muted);
    font-size: var(--fs-sm);
  }
</style>
