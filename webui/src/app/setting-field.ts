// One setting, with the control that fits its kind:
//   yes/no and short choices: segmented buttons (with "Default" first),
//   longer choices: a select, lists: chips, the rest: a text field.
import { Component, computed, input } from '@angular/core';
import { FormControl, ReactiveFormsModule } from '@angular/forms';
import { COMMA, ENTER } from '@angular/cdk/keycodes';
import { MatButtonToggleModule } from '@angular/material/button-toggle';
import { MatChipInputEvent, MatChipsModule } from '@angular/material/chips';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatIconModule } from '@angular/material/icon';
import { MatInputModule } from '@angular/material/input';
import { MatSelectModule } from '@angular/material/select';
import { MatTooltipModule } from '@angular/material/tooltip';
import { Field, SecretState, Value } from './api';
import { defaultOption, placeholder } from './form-values';
import { fieldError } from './settings-form';

@Component({
  selector: 'app-setting-field',
  imports: [
    ReactiveFormsModule,
    MatButtonToggleModule,
    MatChipsModule,
    MatFormFieldModule,
    MatIconModule,
    MatInputModule,
    MatSelectModule,
    MatTooltipModule,
  ],
  templateUrl: './setting-field.html',
  styleUrl: './setting-field.scss',
  host: { '[class.changed]': 'changed()' },
})
export class SettingField {
  readonly field = input.required<Field>();
  readonly control = input.required<FormControl<string>>();
  /** The stored value (for placeholders: "set", the default…). */
  readonly stored = input<Value | SecretState | undefined>();
  readonly changed = input(false);

  protected readonly separators = [ENTER, COMMA];
  protected readonly id = computed(() => 'f-' + this.field().id.replace('.', '-'));
  protected readonly placeholder = computed(() => {
    const f = this.field();
    const p = placeholder(f, this.stored());
    const fromDefault = f.kind.type !== 'secret' && f.default !== null && f.default !== undefined;
    return fromDefault && p ? `${p} (default)` : p;
  });
  protected readonly defaultLabel = computed(() => {
    const t = defaultOption(this.field());
    return t[0].toUpperCase() + t.slice(1);
  });
  protected readonly options = computed(() => {
    const k = this.field().kind;
    return k.type === 'enum' ? k.options : [];
  });
  /** Segmented buttons for yes/no and up to five choices; a select beyond. */
  protected readonly segmented = computed(() => {
    const k = this.field().kind;
    return k.type === 'bool' || (k.type === 'enum' && k.options.length <= 5);
  });
  protected readonly applyNote = computed(() => {
    switch (this.field().apply) {
      case 'restart':
        return 'Changing this restarts the connection';
      case 'reboot':
        return 'Takes effect after the panel restarts';
      default:
        return null;
    }
  });

  protected error(): string | null {
    const c = this.control();
    return c.touched || c.errors?.['server'] ? fieldError(this.field(), c) : null;
  }

  // ---- lists as chips (the control holds "a, b, c") ----

  protected items(): string[] {
    return (this.control().value ?? '')
      .split(',')
      .map((s) => s.trim())
      .filter((s) => s);
  }

  protected addItem(e: MatChipInputEvent) {
    const v = e.value.trim();
    if (v && !this.items().includes(v)) this.setItems([...this.items(), v]);
    e.chipInput.clear();
  }

  protected removeItem(v: string) {
    this.setItems(this.items().filter((x) => x !== v));
  }

  private setItems(items: string[]) {
    const c = this.control();
    c.setValue(items.join(', '));
    c.markAsDirty();
  }
}
