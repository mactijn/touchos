// /settings/:section — one section of the settings.
import { Component, computed, inject, input } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MatCardModule } from '@angular/material/card';
import { MatIconModule } from '@angular/material/icon';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { Config } from '../config';
import { SettingField } from '../setting-field';
import { SettingsForm } from '../settings-form';

@Component({
  selector: 'app-section',
  imports: [MatButtonModule, MatCardModule, MatIconModule, MatProgressBarModule, SettingField],
  templateUrl: './section.html',
  styleUrl: './section.scss',
})
export class SectionPage {
  protected config = inject(Config);
  protected form = inject(SettingsForm);

  /** Route parameter. */
  readonly section = input.required<string>();

  protected readonly current = computed(() =>
    this.form.sections().find((s) => s.id === this.section()),
  );
}
