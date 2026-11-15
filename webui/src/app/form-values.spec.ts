import { describe, expect, it } from 'vitest';
import { ConfigView, Field } from './api';
import { defaultOption, diff, placeholder, toText } from './form-values';

const field = (id: string, kind: Field['kind'], def: Field['default'] = null): Field => ({
  id,
  section: 's',
  label: id,
  help: '',
  kind,
  default: def,
  apply: 'live',
});

const fields = [
  field('ha.port', { type: 'int', min: 1, max: 65535 }, 8123),
  field('ha.tls', { type: 'bool' }),
  field('ha.token', { type: 'secret' }),
  field('display.rotation', { type: 'enum', options: ['auto', '0', '90'] }, 'auto'),
  field('dashboard.rooms', { type: 'list' }),
  field('ha.host', { type: 'text', hint: 'host' }),
];

const cur: ConfigView = {
  version: 7,
  values: {
    'ha.port': 8123,
    'ha.token': { set: true },
    'display.rotation': 0,
    'dashboard.rooms': ['Kitchen', 'Hall'],
    'ha.host': 'ha.lan',
  },
  tiles: [],
  problems: [],
};

const texts = () => Object.fromEntries(fields.map((f) => [f.id, toText(f, cur.values[f.id])]));

describe('diff', () => {
  it('finds no changes in an untouched form', () => {
    expect(diff(fields, cur, texts())).toEqual({ version: 7, set: {}, unset: [] });
  });

  it('keeps an integer enum that reads the same', () => {
    expect(diff(fields, cur, { ...texts(), 'display.rotation': '0' }).set).toEqual({});
  });

  it('sends typed values', () => {
    const t = {
      ...texts(),
      'ha.port': 443,
      'ha.tls': 'true',
      'dashboard.rooms': 'Kitchen , Attic,',
    };
    expect(diff(fields, cur, t).set).toEqual({
      'ha.port': 443,
      'ha.tls': true,
      'dashboard.rooms': ['Kitchen', 'Attic'],
    });
  });

  it('unsets cleared values, including an emptied number input', () => {
    const t = { ...texts(), 'ha.host': '  ', 'ha.port': null };
    expect(diff(fields, cur, t).unset.sort()).toEqual(['ha.host', 'ha.port']);
  });

  it('sends a secret only when one is typed', () => {
    expect(diff(fields, cur, texts()).set['ha.token']).toBeUndefined();
    expect(diff(fields, cur, { ...texts(), 'ha.token': 'abc' }).set['ha.token']).toBe('abc');
  });

  it('ignores list formatting', () => {
    expect(diff(fields, cur, { ...texts(), 'dashboard.rooms': 'Kitchen,Hall' }).set).toEqual({});
  });
});

describe('labels', () => {
  it('describes defaults', () => {
    expect(defaultOption(fields[1])).toBe('default');
    expect(defaultOption(field('x', { type: 'bool' }, true))).toBe('default (yes)');
    expect(placeholder(fields[0], 8123)).toBe('8123');
    expect(placeholder(fields[2], { set: false })).toBe('not set');
    expect(placeholder(fields[5], undefined)).toBe('host');
  });
});
