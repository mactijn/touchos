//! Touches: taps, the brightness drag on a light, page swipes, the room
//! buttons; and what a tap asks Home Assistant to do.

use super::*;

impl Ui {
    pub(super) fn down(&mut self, x: i32, y: i32) {
        self.gesture = match self.hit_chip(x, y) {
            Some(c) => Gesture::Chip(c),
            None => Gesture::Pending(x, y, self.hit(x, y)),
        };
    }

    pub(super) fn moved(&mut self, x: i32, y: i32) {
        match self.gesture {
            Gesture::Pending(sx, sy, tile)
                if (x - sx).abs() > DRAG_THRESHOLD && (x - sx).abs() > (y - sy).abs() =>
            {
                let dimmable = tile
                    .and_then(|i| self.entity_of(i))
                    .filter(|e| e.domain() == "light" && e.dimmable);
                self.gesture = match (tile, dimmable) {
                    (Some(i), Some(e)) => {
                        let b = if e.is_on() {
                            e.brightness.unwrap_or(255) as i32
                        } else {
                            0
                        };
                        Gesture::Dim(i, sx, b)
                    }
                    _ => Gesture::Swipe(sx),
                };
                self.moved(x, y);
            }
            Gesture::Dim(i, sx, b0) => {
                let (_, _, w, _) = self.rect(i);
                let b = (b0 + (x - sx) * 255 / (w - 2 * M).max(1)).clamp(0, 255);
                self.preview = Some((i, b));
            }
            _ => {}
        }
    }

    pub(super) fn up(&mut self, x: i32, y: i32) {
        match core::mem::replace(&mut self.gesture, Gesture::None) {
            Gesture::Pending(_, _, Some(i)) => self.tap(i, x, y),
            Gesture::Chip(c) => {
                if self.hit_chip(x, y) == Some(c) {
                    self.select_room(c);
                }
            }
            Gesture::Dim(i, _, _) => {
                if let (Some((_, b)), Some(e)) = (self.preview, self.entity_of(i)) {
                    if b <= 2 {
                        ha::call_service("light", "turn_off", &e.id, CallData::None);
                    } else {
                        ha::call_service(
                            "light",
                            "turn_on",
                            &e.id,
                            CallData::Brightness(b.clamp(0, 255) as u8),
                        );
                    }
                }
                // Keep the preview until Home Assistant reports the new state.
            }
            Gesture::Swipe(sx) => {
                if x - sx < -s(80) && self.page + 1 < self.pages() {
                    self.page += 1;
                } else if x - sx > s(80) && self.page > 0 {
                    self.page -= 1;
                }
            }
            _ => {}
        }
    }

    pub(super) fn tap(&mut self, i: usize, x: i32, _y: i32) {
        let Some(e) = self.entity_of(i) else { return };
        let domain = e.domain().to_string();
        let (tx, _, w, _) = self.rect(i);
        if domain == "climate" {
            // -/+ buttons in the right part of the tile (see render_tile).
            let (step, target) = (e.temp_step.max(0.1), e.target_temp);
            let (_, ty, _, th) = self.rect(i);
            let (plus_x, minus_x, _) = climate_buttons(tx, ty, w, th);
            let delta = if x >= plus_x - s(10) {
                step
            } else if x >= minus_x - s(10) && x < minus_x + CLIMATE_BTN + s(10) {
                -step
            } else {
                return;
            };
            if let Some(t) = target {
                self.pressed = Some((i, time::now_ms()));
                ha::call_service(
                    "climate",
                    "set_temperature",
                    &e.id,
                    CallData::Temperature(t + delta),
                );
            }
            return;
        }
        let service = match domain.as_str() {
            "light" | "switch" | "input_boolean" | "fan" | "cover" => "toggle",
            "lock" => {
                if e.state == "locked" {
                    "unlock"
                } else {
                    "lock"
                }
            }
            "media_player" => "media_play_pause",
            "scene" | "script" => "turn_on",
            _ => return,
        };
        self.pressed = Some((i, time::now_ms()));
        self.preview = None;
        ha::call_service(&domain, service, &e.id, CallData::None);
    }
}
