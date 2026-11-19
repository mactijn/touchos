import { describe, expect, it } from 'vitest';
import { ProcessFigures, Processes } from './api';
import { bytes, exitText, pushHistory, rows, sorted, sparkline } from './processes';

const proc = (name: string, pid: number | null, cpu_us: number | null, switches: number | null): ProcessFigures => ({
  name,
  pid,
  kernel: pid === 0,
  idle: name === 'idle',
  state: pid === null ? null : 'sleeping',
  priority: pid === null ? null : 4,
  cpu_us,
  switches,
  memory: pid ? 1 << 20 : null,
  restarts: pid === 0 ? null : 0,
  crashes: pid === 0 ? null : 0,
  status: pid === null ? 'gave up' : 'running',
  last_exit: null,
});

const poll = (uptime_us: number, idle_us: number, switches: number, processes: ProcessFigures[]): Processes => ({
  system: {
    uptime_us,
    ticks: 0,
    idle_us,
    switches,
    cores: 1,
    threads: processes.length,
    free_bytes: 90 << 20,
    heap_used: 50_000,
    heap_size: 8 << 20,
  },
  processes,
});

describe('rows', () => {
  it('has no rates from the first poll', () => {
    const r = rows(poll(10e6, 9e6, 100, [proc('net', 5, 1000, 10)]), null);
    expect(r.system.busy).toBeNull();
    expect(r.system.switchesPerS).toBeNull();
    expect(r.system.uptimeS).toBe(10);
    expect(r.processes[0].cpu).toBeNull();
    expect(r.processes[0].cpuSeconds).toBe(0.001);
  });

  it('computes rates between two polls', () => {
    const a = poll(10e6, 9e6, 100, [proc('net', 5, 1e6, 10), proc('idle', 0, 8e6, 50), proc('wifi', 7, 0, 0)]);
    // 2 s later: idle 1.5 s, net 0.4 s, wifi restarted (new pid).
    const b = poll(12e6, 10.5e6, 200, [proc('net', 5, 1.4e6, 30), proc('idle', 0, 9.5e6, 80), proc('wifi', 9, 1000, 1)]);
    const r = rows(b, a);
    expect(r.system.busy).toBeCloseTo(25);
    expect(r.system.switchesPerS).toBeCloseTo(50);
    const by = new Map(r.processes.map((p) => [p.name, p]));
    expect(by.get('net')!.cpu).toBeCloseTo(20);
    expect(by.get('net')!.switchesPerS).toBeCloseTo(10);
    expect(by.get('idle')!.cpu).toBeCloseTo(75);
    expect(by.get('wifi')!.cpu).toBeNull(); // restarted: its counters started over
  });

  it('copes with the 32-bit switch counter wrapping', () => {
    const a = poll(1e6, 0, 2 ** 32 - 10, []);
    const b = poll(2e6, 0, 10, []);
    expect(rows(b, a).system.switchesPerS).toBe(20);
  });

  it('keeps programs that aren\'t running', () => {
    const r = rows(poll(1e6, 0, 0, [proc('ha', null, null, null)]), null);
    expect(r.processes[0]).toMatchObject({ name: 'ha', pid: null, cpu: null, status: 'gave up' });
  });
});

describe('sorted', () => {
  const list = rows(
    poll(2e6, 0, 0, [proc('b', 2, 10, 0), proc('a', 1, 300, 0), proc('gone', null, null, null), proc('c', 3, 200, 0)]),
    poll(1e6, 0, 0, [proc('b', 2, 0, 0), proc('a', 1, 0, 0), proc('c', 3, 0, 0)]),
  ).processes;

  it('sorts numbers biggest first, what isn\'t running last', () => {
    expect(sorted(list, 'cpu').map((r) => r.name)).toEqual(['a', 'c', 'b', 'gone']);
    expect(sorted(list, 'cpu', true).map((r) => r.name)).toEqual(['b', 'c', 'a', 'gone']);
  });

  it('sorts names A-Z', () => {
    expect(sorted(list, 'name').map((r) => r.name)).toEqual(['a', 'b', 'c', 'gone']);
  });
});

describe('texts', () => {
  it('exits', () => {
    expect(exitText(null)).toBeNull();
    expect(exitText({ how: 'crash', code: 4 })).toBe('crashed (CPU exception 4)');
    expect(exitText({ how: 'crash', code: 0 })).toBe('crashed');
    expect(exitText({ how: 'killed', code: 0 })).toBe('stopped');
    expect(exitText({ how: 'exit', code: 0 })).toBe('finished');
    expect(exitText({ how: 'exit', code: 3 })).toBe('exited with code 3');
  });

  it('bytes', () => {
    expect(bytes(null)).toBe('');
    expect(bytes(512)).toBe('512 B');
    expect(bytes(655360)).toBe('640 KiB');
    expect(bytes(2.5 * 2 ** 20)).toBe('2.5 MiB');
  });
});

describe('history', () => {
  it('keeps the last values', () => {
    expect(pushHistory([1, 2, 3], 4, 3)).toEqual([2, 3, 4]);
    expect(pushHistory([], 1, 3)).toEqual([1]);
  });

  it('draws newest at the right edge', () => {
    expect(sparkline([0], 100, 20, 3)).toBe('');
    expect(sparkline([0, 100], 100, 20, 3)).toBe('50.0,20.0 100.0,0.0');
    expect(sparkline([50, 50, 150], 100, 20, 3)).toBe('0.0,10.0 50.0,10.0 100.0,0.0');
  });
});
