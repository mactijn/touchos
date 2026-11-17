// The panel's JSON API (servers/web/src/api.rs) and a small wrapper around
// HttpClient that never throws for an HTTP error: callers look at `status`.
import { HttpClient, HttpErrorResponse, HttpInterceptorFn } from '@angular/common/http';
import { Injectable, inject } from '@angular/core';
import { firstValueFrom } from 'rxjs';

// ---- settings schema and values ----

export type Kind =
  | { type: 'int'; min: number; max: number }
  | { type: 'bool' }
  | { type: 'text'; max?: number; hint?: string }
  | { type: 'secret' }
  | { type: 'enum'; options: string[] }
  | { type: 'list' }
  | { type: 'zone' };

export type Value = string | number | boolean | string[];

export interface Field {
  id: string;
  section: string;
  label: string;
  help: string;
  kind: Kind;
  default: Value | null;
  /** When a change takes effect. */
  apply: 'live' | 'restart' | 'reboot';
}

export interface Schema {
  /** [section id, title], in display order. */
  sections: [string, string][];
  fields: Field[];
  tile_fields: Field[];
}

/** A secret is never sent back; only whether it is set. */
export interface SecretState {
  set: boolean;
}

export type Tile = [entity: string, name: string];

export interface ConfigView {
  version: number;
  /** Only the values set in touchos.toml; a missing id means "default". */
  values: Record<string, Value | SecretState>;
  tiles: Tile[];
  problems: string[];
}

export interface Patch {
  version: number;
  set: Record<string, Value>;
  unset: string[];
  tiles?: Tile[];
}

export interface PatchResult {
  version: number;
  /** What restarts to apply the change ("Wi-Fi", "reboot", …). */
  restart: string[];
  saved: boolean;
  save_failed?: boolean;
}

export interface FieldError {
  field: string;
  message: string;
}

// ---- status ----

export interface Status {
  version: string;
  slot: string | null;
  build: string | null;
  uptime_s: number;
  clock_set: boolean;
  ip: string | null;
  ha: { connected: boolean; entities: number };
  config: { version: number | null; problems: string[] };
}

// ---- software updates ----

export type Phase =
  | { state: 'idle' }
  | { state: 'writing'; file: string; done: number; total: number }
  | { state: 'restarting'; build: string }
  | { state: 'failed'; error: string };

export interface UpdateState {
  enabled: boolean;
  running?: string | null;
  build?: string;
  other?: string | null;
  phase?: Phase;
}

// ---- transport ----

export interface Reply<T> {
  status: number;
  /** The body on success (2xx). */
  data?: T;
  /** The error body otherwise, e.g. {"error": "...", "retry_after": 30}. */
  err?: { error?: string; [k: string]: unknown };
}

/** 0: the panel didn't answer (restarting, network). */
export const OFFLINE = 0;

/**
 * Every request carries X-Touchos: the panel refuses changes without it
 * (a cross-site form or image can't set custom headers).
 */
export const touchosHeader: HttpInterceptorFn = (req, next) =>
  next(req.clone({ setHeaders: { 'X-Touchos': '1' } }));

@Injectable({ providedIn: 'root' })
export class Api {
  private http = inject(HttpClient);

  get<T>(url: string): Promise<Reply<T>> {
    return this.call<T>(this.http.get<T>(url));
  }

  getText(url: string): Promise<Reply<string>> {
    return this.call(this.http.get(url, { responseType: 'text' }));
  }

  post<T>(url: string, body?: unknown): Promise<Reply<T>> {
    return this.call<T>(this.http.post<T>(url, body ?? null));
  }

  patch<T>(url: string, body: unknown): Promise<Reply<T>> {
    return this.call<T>(this.http.patch<T>(url, body));
  }

  private async call<T>(req: import('rxjs').Observable<T>): Promise<Reply<T>> {
    try {
      return { status: 200, data: await firstValueFrom(req) };
    } catch (e) {
      if (!(e instanceof HttpErrorResponse)) throw e;
      let err = e.error;
      if (typeof err === 'string') {
        try {
          err = JSON.parse(err);
        } catch {
          err = { error: err };
        }
      }
      if (err instanceof ProgressEvent || err === null || typeof err !== 'object') {
        err = { error: e.status === OFFLINE ? 'the panel is not reachable' : e.statusText };
      }
      return { status: e.status, err };
    }
  }
}

/** "wait 30 s" style suffix for errors that carry retry_after. */
export function errorText(r: Reply<unknown>, fallback = 'failed'): string {
  const e = r.err ?? {};
  let t = (e.error as string | undefined) ?? `${fallback} (${r.status})`;
  if (typeof e['retry_after'] === 'number') t += ` (wait ${e['retry_after']} s)`;
  return t;
}
