// /tiles — which entities the panel shows, in which order.
import { CdkDragDrop, DragDropModule } from '@angular/cdk/drag-drop';
import { Component, computed, inject } from '@angular/core';
import { ReactiveFormsModule } from '@angular/forms';
import { MatAutocompleteModule } from '@angular/material/autocomplete';
import { MatButtonModule } from '@angular/material/button';
import { MatCardModule } from '@angular/material/card';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatIconModule } from '@angular/material/icon';
import { MatInputModule } from '@angular/material/input';
import { MatTooltipModule } from '@angular/material/tooltip';
import { Tile } from '../api';
import { Config } from '../config';
import { SettingsForm } from '../settings-form';

/** Suggestions shown at once (Home Assistant may have hundreds). */
const MAX_SUGGESTIONS = 40;

@Component({
  selector: 'app-tiles',
  imports: [
    DragDropModule,
    ReactiveFormsModule,
    MatAutocompleteModule,
    MatButtonModule,
    MatCardModule,
    MatFormFieldModule,
    MatIconModule,
    MatInputModule,
    MatTooltipModule,
  ],
  templateUrl: './tiles.html',
  styleUrl: './tiles.scss',
})
export class TilesPage {
  protected config = inject(Config);
  protected form = inject(SettingsForm);

  /** Friendly names from Home Assistant, by entity id. */
  protected readonly names = computed(() => new Map(this.config.entities()));

  constructor() {
    this.config.loadEntities();
  }

  protected suggestions(typed: string): Tile[] {
    const q = typed.trim().toLowerCase();
    const all = this.config.entities();
    const hits = q
      ? all.filter(([id, name]) => id.toLowerCase().includes(q) || name.toLowerCase().includes(q))
      : all;
    return hits.slice(0, MAX_SUGGESTIONS);
  }

  protected add() {
    this.form.addTile();
    const i = this.form.tiles.length - 1;
    setTimeout(() => document.getElementById(`tile-${i}`)?.focus());
  }

  protected drop(e: CdkDragDrop<unknown>) {
    this.form.moveTile(e.previousIndex, e.currentIndex);
  }
}
