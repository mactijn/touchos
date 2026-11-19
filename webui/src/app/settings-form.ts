// The settings being edited: every section's fields and the tiles, in one
// form that outlives the pages (switching sections keeps the edits), saved
// together from the save bar.
import { Injectable, computed, effect, inject, signal, untracked } from '@angular/core';
import { FormArray, FormControl, FormGroup, ValidatorFn, Validators } from '@angular/forms';
import { merge } from 'rxjs';
import { ConfigView, Field, Patch, Schema, Tile } from './api';
import { Config } from './config';
import { diff, toText } from './form-values';

export type TileRow = FormGroup<{ entity: FormControl<string>; name: FormControl<string> }>;

export interface Section {
  id: string;
  title: string;
  icon: string;
  fields: Field[];
}

/** Icons for the schema's sections (servers/web/src/api.rs). */
const SECTION_ICONS: Record<string, string> = {
  homeassistant: 'home',
  wifi: 'wifi',
  display: 'brightness_medium',
  dashboard: 'space_dashboard',
  time: 'schedule',
  web: 'language',
  metrics: 'monitoring',
  debug: 'bug_report',
};

export type SaveResult =
  { ok: true; message: string; warn: boolean } | { ok: false; message: string; section?: string };

@Injectable({ providedIn: 'root' })
export class SettingsForm {
  private config = inject(Config);

  readonly fields = new FormGroup<Record<string, FormControl<string>>>({});
  readonly tiles = new FormArray<TileRow>([]);
  readonly saving = signal(false);

  /** Bumped on every change to the forms: they aren't signals themselves. */
  private readonly tick = signal(0);
  /** The controls exist (the settings have been loaded once). */
  private readonly filled = signal(false);

  readonly sections = computed<Section[]>(() => {
    const s = this.config.schema();
    if (!s || !this.filled()) return [];
    return s.sections
      .map(([id, title]) => ({
        id,
        title,
        icon: SECTION_ICONS[id] ?? 'tune',
        fields: s.fields.filter((f) => f.section === id),
      }))
      .filter((sec) => sec.fields.length);
  });

  /** The change to send; null before the settings are loaded. */
  readonly patch = computed<Patch | null>(() => {
    this.tick();
    const schema = this.config.schema();
    const cur = this.config.current();
    if (!schema || !cur) return null;
    const p = diff(schema.fields, cur, this.fields.getRawValue());
    const tiles = this.tileValues();
    if (JSON.stringify(tiles) !== JSON.stringify(cur.tiles)) p.tiles = tiles;
    return p;
  });

  readonly changedIds = computed(() => {
    const p = this.patch();
    return new Set(p ? [...Object.keys(p.set), ...p.unset] : []);
  });

  readonly tilesChanged = computed(() => !!this.patch()?.tiles);

  /** How many things would change (each field, and the tile list as one). */
  readonly count = computed(() => this.changedIds().size + (this.tilesChanged() ? 1 : 0));

  /** Per section: something changed / something invalid. */
  readonly sectionState = computed(() => {
    this.tick();
    const changed = this.changedIds();
    const state: Record<string, { changed: boolean; invalid: boolean }> = {};
    for (const s of this.sections()) {
      state[s.id] = {
        changed: s.fields.some((f) => changed.has(f.id)),
        invalid: s.fields.some((f) => this.fields.controls[f.id]?.invalid),
      };
    }
    return state;
  });

  /** The tile rows, as a new array whenever they change (for templates). */
  readonly tileRows = computed(() => {
    this.tick();
    return [...this.tiles.controls];
  });

  constructor() {
    merge(this.fields.valueChanges, this.fields.statusChanges, this.tiles.valueChanges).subscribe(
      () => this.tick.update((n) => n + 1),
    );
    // (Re)fill the forms from the stored settings: when they load, after a
    // save, after a conflict.
    effect(() => {
      const schema = this.config.schema();
      const cur = this.config.current();
      if (schema && cur) untracked(() => this.fill(schema, cur));
    });
  }

  control(id: string): FormControl<string> {
    return this.fields.controls[id];
  }

  /** Throw away the edits. */
  discard() {
    const schema = this.config.schema();
    const cur = this.config.current();
    if (schema && cur) this.fill(schema, cur);
  }

  addTile(entity = '', name = '') {
    this.tiles.push(tileRow(entity, name));
  }

  removeTile(i: number) {
    this.tiles.removeAt(i);
  }

  moveTile(from: number, to: number) {
    if (from === to) return;
    const r = this.tiles.at(from);
    this.tiles.removeAt(from, { emitEvent: false });
    this.tiles.insert(to, r);
  }

  async save(): Promise<SaveResult> {
    this.fields.markAllAsTouched();
    if (this.fields.invalid) {
      this.tick.update((n) => n + 1);
      return { ok: false, message: 'Fix the marked fields first.', section: this.firstInvalid() };
    }
    const p = this.patch();
    if (!p || this.count() === 0) return { ok: true, message: 'Nothing changed.', warn: false };
    this.saving.set(true);
    const r = await this.config.save(p);
    this.saving.set(false);
    switch (r.kind) {
      case 'saved':
        return { ok: true, message: r.message, warn: r.warn };
      case 'invalid': {
        const other: string[] = [];
        for (const e of r.errors) {
          const c = this.fields.controls[e.field];
          if (c) {
            c.setErrors({ server: e.message });
            c.markAsTouched();
          } else {
            other.push(e.field ? `${e.field}: ${e.message}` : e.message);
          }
        }
        this.tick.update((n) => n + 1);
        return {
          ok: false,
          message: other.length
            ? `Not saved: ${other.join('; ')}`
            : 'Not saved: fix the marked fields.',
          section:
            this.firstInvalid() ??
            (r.errors.some((e) => e.field.startsWith('tiles')) ? 'tiles' : undefined),
        };
      }
      case 'conflict':
        return {
          ok: false,
          message:
            'The settings were changed elsewhere and have been reloaded. Please make your change again.',
        };
      case 'error':
        return { ok: false, message: r.message };
    }
  }

  private firstInvalid(): string | undefined {
    return this.sections().find((s) => s.fields.some((f) => this.fields.controls[f.id]?.invalid))
      ?.id;
  }

  private tileValues(): Tile[] {
    return this.tiles
      .getRawValue()
      .map((r): Tile => [r.entity.trim(), r.name.trim()])
      .filter(([e]) => e !== '');
  }

  private fill(schema: Schema, cur: ConfigView) {
    for (const f of schema.fields) {
      const text = toText(f, cur.values[f.id]);
      const c = this.fields.controls[f.id];
      if (c) c.reset(text, { emitEvent: false });
      else {
        this.fields.addControl(
          f.id,
          new FormControl(text, { nonNullable: true, validators: validators(f) }),
          { emitEvent: false },
        );
      }
    }
    this.tiles.clear({ emitEvent: false });
    for (const [e, n] of cur.tiles) this.tiles.push(tileRow(e, n), { emitEvent: false });
    this.fields.updateValueAndValidity({ emitEvent: false });
    this.filled.set(true);
    this.tick.update((n) => n + 1);
  }
}

function tileRow(entity: string, name: string): TileRow {
  return new FormGroup({
    entity: new FormControl(entity, { nonNullable: true }),
    name: new FormControl(name, { nonNullable: true }),
  });
}

function validators(f: Field): ValidatorFn[] {
  const k = f.kind;
  if (k.type === 'int')
    return [Validators.min(k.min), Validators.max(k.max), Validators.pattern(/^\s*-?\d+\s*$/)];
  if (k.type === 'text' && k.max) return [Validators.maxLength(k.max)];
  return [];
}

/** The message for an invalid field. */
export function fieldError(f: Field, c: FormControl<string>): string | null {
  const e = c.errors;
  if (!e) return null;
  if (e['server']) return e['server'];
  if (e['min'] || e['max']) {
    const k = f.kind as { min: number; max: number };
    return `Between ${k.min} and ${k.max}`;
  }
  if (e['pattern']) return 'A whole number';
  if (e['maxlength']) return `At most ${e['maxlength'].requiredLength} characters`;
  return 'Not valid';
}
