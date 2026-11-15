// Between the API's typed values and the form's text inputs.
//
// Every control holds a string: "" means "not set in touchos.toml" (use the
// default). Selects use "true"/"false" for yes/no.
import { ConfigView, Field, Patch, SecretState, Value } from './api';

/** The form text for a stored value. Secrets always start empty. */
export function toText(f: Field, v: Value | SecretState | undefined): string {
  if (f.kind.type === 'secret' || v === undefined || v === null) return '';
  if (Array.isArray(v)) return v.join(', ');
  return String(v);
}

/** The typed value for a non-empty form text. */
export function fromText(f: Field, raw: string): Value {
  switch (f.kind.type) {
    case 'int':
      return Number(raw);
    case 'bool':
      return raw === 'true';
    case 'list':
      return raw
        .split(',')
        .map((s) => s.trim())
        .filter((s) => s !== '');
    default:
      return raw;
  }
}

/** What to show in an empty input. */
export function placeholder(f: Field, v: Value | SecretState | undefined): string {
  const k = f.kind;
  if (k.type === 'secret')
    return (v as SecretState | undefined)?.set ? 'set (type to replace)' : 'not set';
  if (Array.isArray(f.default)) return f.default.join(', ');
  if (f.default !== null && f.default !== undefined) return String(f.default);
  return k.type === 'text' ? (k.hint ?? '') : '';
}

/** The label of a select's "not set" option. */
export function defaultOption(f: Field): string {
  if (f.default === null || f.default === undefined) return 'default';
  if (typeof f.default === 'boolean') return `default (${f.default ? 'yes' : 'no'})`;
  return `default (${f.default})`;
}

/** The changes between the stored values and the form's texts. */
export function diff(fields: Field[], cur: ConfigView, texts: Record<string, unknown>): Patch {
  const set: Record<string, Value> = {};
  const unset: string[] = [];
  for (const f of fields) {
    // (A number input's control holds a number, or null when empty.)
    const t = texts[f.id];
    if (t === undefined) continue;
    const raw = t === null ? '' : String(t).trim();
    const old = cur.values[f.id];
    if (f.kind.type === 'secret') {
      if (raw !== '') set[f.id] = raw;
      continue;
    }
    if (raw === '') {
      if (old !== undefined) unset.push(f.id);
      continue;
    }
    // Unchanged when it reads the same: touchos.toml may say `rotation = 0`
    // where the form has "0".
    if (raw === toText(f, old)) continue;
    const v = fromText(f, raw);
    if (JSON.stringify(v) !== JSON.stringify(old)) set[f.id] = v;
  }
  return { version: cur.version, set, unset };
}

export function isEmpty(p: Patch): boolean {
  return !Object.keys(p.set).length && !p.unset.length && !p.tiles;
}
