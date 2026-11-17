// The settings: schema (fixed per build), current values, and saving.
import { Injectable, inject, signal } from '@angular/core';
import {
  Api,
  ConfigView,
  FieldError,
  Patch,
  PatchResult,
  Reply,
  Schema,
  Tile,
  errorText,
} from './api';
import { Session } from './session';

export type SaveOutcome =
  | { kind: 'saved'; message: string; warn: boolean }
  | { kind: 'invalid'; errors: FieldError[] }
  | { kind: 'conflict' }
  | { kind: 'error'; message: string };

@Injectable({ providedIn: 'root' })
export class Config {
  private api = inject(Api);
  private session = inject(Session);

  readonly schema = signal<Schema | null>(null);
  readonly current = signal<ConfigView | null>(null);
  readonly loadError = signal<string | null>(null);
  /** Home Assistant entities: [id, friendly name]. */
  readonly entities = signal<Tile[]>([]);
  /** The time zones the panel knows (IANA names), for the zone picker. */
  readonly zones = signal<string[]>([]);

  async load(): Promise<void> {
    if (!this.schema()) {
      const s = await this.api.get<Schema>('/api/schema');
      if (!this.ok(s)) return;
      this.schema.set(s.data!);
    }
    const c = await this.api.get<ConfigView>('/api/config');
    if (!this.ok(c)) return;
    this.loadError.set(null);
    this.current.set(c.data!);
  }

  async loadEntities(): Promise<void> {
    const r = await this.api.get<Tile[]>('/api/ha/entities');
    if (r.data) this.entities.set([...r.data].sort((a, b) => a[0].localeCompare(b[0])));
  }

  async loadZones(): Promise<void> {
    if (this.zones().length) return;
    const r = await this.api.get<string[]>('/api/zones');
    if (r.data) this.zones.set(r.data);
  }

  async save(patch: Patch): Promise<SaveOutcome> {
    const r = await this.api.patch<PatchResult>('/api/config', patch);
    if (r.data) {
      await this.load();
      return { kind: 'saved', ...describe(r.data) };
    }
    switch (r.status) {
      case 422:
        return { kind: 'invalid', errors: (r.err?.['errors'] as FieldError[]) ?? [] };
      case 409:
        await this.load();
        return { kind: 'conflict' };
      case 401:
        this.session.lost();
        return { kind: 'error', message: 'Not paired anymore.' };
      default:
        return { kind: 'error', message: errorText(r, 'Not saved') };
    }
  }

  private ok(r: Reply<unknown>): boolean {
    if (r.status === 401) {
      this.session.lost();
      return false;
    }
    if (!r.data) {
      this.loadError.set(errorText(r, "Couldn't load the settings"));
      return false;
    }
    return true;
  }
}

/** What a successful save means for the user. */
export function describe(r: PatchResult): { message: string; warn: boolean } {
  let message = 'Saved.';
  if (r.restart.length) message += ` Applying: ${r.restart.join(', ')}.`;
  if (r.save_failed) {
    return {
      message:
        message + " Saving to the panel's storage FAILED: the change is lost at the next restart.",
      warn: true,
    };
  }
  if (!r.saved) message += ' (Not saved to storage: kept until the panel restarts.)';
  return { message, warn: false };
}
