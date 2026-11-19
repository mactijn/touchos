// Rates for the System page's process list: the panel sends monotonic
// counters, and the page takes the difference between two polls, so the
// panel keeps no state and two browsers don't disturb each other.
import { ProcessFigures, Processes } from './api';

export interface ProcessRow {
  name: string;
  pid: number | null;
  kernel: boolean;
  idle: boolean;
  state: string | null;
  priority: number | null;
  /** CPU share since the last poll, 0-100 (null: no earlier poll, or not running). */
  cpu: number | null;
  /** Switches to it per second since the last poll. */
  switchesPerS: number | null;
  /** Since its start. */
  cpuSeconds: number | null;
  memory: number | null;
  restarts: number | null;
  crashes: number | null;
  status: ProcessFigures['status'];
  lastExit: string | null;
}

export interface SystemRow {
  uptimeS: number;
  /** CPU busy (not idle) since the last poll, 0-100. */
  busy: number | null;
  switchesPerS: number | null;
  freeBytes: number;
  heapUsed: number;
  heapSize: number;
  threads: number;
}

export type SortKey = 'name' | 'pid' | 'cpu' | 'switchesPerS' | 'memory' | 'restarts' | 'status';

/** The system's row and the processes' rows from a poll and the one before it. */
export function rows(cur: Processes, prev: Processes | null): { system: SystemRow; processes: ProcessRow[] } {
  const s = cur.system;
  const p = prev?.system;
  // Wall time between the polls, from the panel's own clock.
  const dt = p && s.uptime_us > p.uptime_us ? s.uptime_us - p.uptime_us : 0;
  const cores = Math.max(1, s.cores);
  const pct = (us: number) => clamp((100 * us) / dt / cores, 0, 100);
  const system: SystemRow = {
    uptimeS: Math.floor(s.uptime_us / 1e6),
    busy: dt ? 100 - pct(s.idle_us - p!.idle_us) : null,
    // The kernel's count is 32 bits: a wrap reads as a negative difference.
    switchesPerS: dt ? (wrap32(s.switches - p!.switches) * 1e6) / dt : null,
    freeBytes: s.free_bytes,
    heapUsed: s.heap_used,
    heapSize: s.heap_size,
    threads: s.threads,
  };
  const before = new Map((prev?.processes ?? []).map((x) => [x.name, x]));
  const processes = cur.processes.map((x): ProcessRow => {
    const b = before.get(x.name);
    // The same process as last time? (A restart starts its counters over.)
    const same = dt && b && b.pid === x.pid && x.cpu_us !== null && b.cpu_us !== null;
    return {
      name: x.name,
      pid: x.pid,
      kernel: x.kernel,
      idle: x.idle,
      state: x.state,
      priority: x.priority,
      cpu: same ? pct(x.cpu_us! - b!.cpu_us!) : null,
      switchesPerS: same && x.switches !== null && b!.switches !== null ? ((x.switches - b!.switches) * 1e6) / dt : null,
      cpuSeconds: x.cpu_us === null ? null : x.cpu_us / 1e6,
      memory: x.memory,
      restarts: x.restarts,
      crashes: x.crashes,
      status: x.status,
      lastExit: exitText(x.last_exit),
    };
  });
  return { system, processes };
}

/** "crashed (exception 4)", "exited (code 3)", "stopped". */
export function exitText(e: ProcessFigures['last_exit']): string | null {
  if (!e) return null;
  switch (e.how) {
    case 'crash':
      return e.code ? `crashed (CPU exception ${e.code})` : 'crashed';
    case 'killed':
      return 'stopped';
    default:
      return e.code ? `exited with code ${e.code}` : 'finished';
  }
}

/**
 * Sorted for the table: by `key` (numbers biggest first, text A-Z;
 * `desc` flips it), what isn't running last, then by name.
 */
export function sorted(list: ProcessRow[], key: SortKey, desc = false): ProcessRow[] {
  const val = (r: ProcessRow): number | string | null => r[key];
  const text = key === 'name' || key === 'status';
  return [...list].sort((a, b) => {
    const ra = a.pid === null ? 1 : 0;
    const rb = b.pid === null ? 1 : 0;
    if (ra !== rb) return ra - rb;
    const va = val(a);
    const vb = val(b);
    let c = 0;
    if (va === null && vb !== null) c = 1;
    else if (vb === null && va !== null) c = -1;
    else if (va !== null && vb !== null) {
      c = text ? String(va).localeCompare(String(vb)) : (vb as number) - (va as number);
      if (desc) c = -c;
    }
    return c || a.name.localeCompare(b.name);
  });
}

/** "1.2 MiB", "640 KiB", "512 B". */
export function bytes(n: number | null): string {
  if (n === null) return '';
  if (n >= 1 << 20) return `${(n / (1 << 20)).toFixed(1)} MiB`;
  if (n >= 1 << 10) return `${Math.round(n / (1 << 10))} KiB`;
  return `${n} B`;
}

/** The last `max` values, `v` added. */
export function pushHistory(h: number[], v: number, max: number): number[] {
  const out = [...h, v];
  return out.length > max ? out.slice(out.length - max) : out;
}

/** SVG polyline points for values 0-100 in a `w` x `h` box, newest at the right. */
export function sparkline(values: number[], w: number, h: number, max: number): string {
  if (values.length < 2) return '';
  const step = w / (max - 1);
  const x0 = w - step * (values.length - 1);
  return values
    .map((v, i) => `${(x0 + i * step).toFixed(1)},${(h - (clamp(v, 0, 100) / 100) * h).toFixed(1)}`)
    .join(' ');
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

function wrap32(d: number): number {
  return d < 0 ? d + 2 ** 32 : d;
}
