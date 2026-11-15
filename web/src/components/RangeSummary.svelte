<script lang="ts">
  import { bytes, compact, pct, rate, seconds } from '../lib/format';
  import { dash } from '../lib/store.svelte';
  import type { SeriesField } from '../lib/types';

  type Fmt = (v: number | null) => string;
  const tokS: Fmt = (v) => (v === null ? rate(v) : `${rate(v)} tokens/s`);
  const celsius: Fmt = (v) => (v === null ? rate(v) : `${rate(v)} °C`);
  const watts: Fmt = (v) => (v === null ? rate(v) : `${rate(v)} W`);
  const perMin: Fmt = (v) => (v === null ? rate(v) : `${rate(v)}/min`);

  const GROUPS: { name: string; rows: { field: SeriesField; label: string; fmt: Fmt }[] }[] = [
    {
      name: 'Halogen',
      rows: [
        { field: 'ttft', label: 'Time to first token', fmt: (v) => seconds(v) },
        { field: 'gen_speed', label: 'Generation speed', fmt: tokS },
        { field: 'prefill_speed', label: 'Prefill speed', fmt: tokS },
        { field: 'throughput', label: 'Throughput', fmt: tokS },
        { field: 'busy_slots', label: 'Busy Slots', fmt: (v) => compact(v) },
        { field: 'kv_use', label: 'KV pool use', fmt: (v) => pct(v) },
        { field: 'requests_per_min', label: 'Requests/min', fmt: perMin },
        { field: 'mean_service_time', label: 'Mean service time', fmt: (v) => seconds(v) },
      ],
    },
    {
      name: 'Hardware',
      rows: [
        { field: 'gpu_use', label: 'GPU use', fmt: (v) => pct(v) },
        { field: 'cpu_use', label: 'CPU use', fmt: (v) => pct(v) },
        { field: 'gpu_temp', label: 'GPU temperature', fmt: celsius },
        { field: 'cpu_temp', label: 'CPU temperature', fmt: celsius },
        { field: 'nvme_temp', label: 'SSD temperature', fmt: celsius },
        { field: 'apu_power', label: 'APU power', fmt: watts },
        { field: 'gpu_mem_used', label: 'GPU memory', fmt: (v) => bytes(v) },
        { field: 'ram_used', label: 'RAM', fmt: (v) => bytes(v) },
      ],
    },
  ];

  const s = $derived(dash.summary);
</script>

<section class="card summary" aria-labelledby="summary-title">
  <header>
    <h2 id="summary-title">Range summary</h2>
    <span class="aside">{dash.rangeDef.name}: average, low and high of every Sample</span>
  </header>
  <div class="groups">
    {#each GROUPS as g (g.name)}
      <table>
        <thead>
          <tr><th scope="col">{g.name}</th><th scope="col">Average</th><th scope="col">Low</th><th scope="col">High</th></tr>
        </thead>
        <tbody>
          {#each g.rows as r (r.field)}
            {@const v = s?.[r.field]}
            <tr>
              <th scope="row">{r.label}</th>
              <td class="num">{r.fmt(v?.mean ?? null)}</td>
              <td class="num">{r.fmt(v?.min ?? null)}</td>
              <td class="num">{r.fmt(v?.max ?? null)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/each}
  </div>
</section>

<style>
  .summary {
    padding: var(--sp-3) var(--sp-4) var(--sp-4);
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-3);
    flex-wrap: wrap;
  }
  .aside {
    font-size: var(--fs-xs);
    color: var(--faint);
  }
  .groups {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--sp-5);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--fs-sm);
  }
  th,
  td {
    padding: 5px 0;
    border-bottom: 1px solid var(--line-soft);
    text-align: right;
    white-space: nowrap;
  }
  th:first-child {
    text-align: left;
    padding-right: var(--sp-3);
  }
  thead th {
    color: var(--faint);
    font-size: var(--fs-xs);
    font-weight: 500;
  }
  thead th:first-child {
    color: var(--text);
    font-size: var(--fs-sm);
    font-weight: 600;
  }
  tbody th {
    color: var(--muted);
    font-weight: 400;
  }
  td {
    padding-left: var(--sp-3);
  }
  tr:last-child > * {
    border-bottom: 0;
  }

  @media (max-width: 1000px) {
    .groups {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
