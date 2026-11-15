//! Drawing a frame: the header (clock, date, status, room buttons), the
//! tiles and the page dots.

use super::*;

impl Ui {
    pub(super) fn render(&mut self, st: &ha::HaState) {
        let w = self.canvas.w;
        let h = self.canvas.h;
        self.canvas.fill_rect(0, 0, w, h, BG);
        self.render_header(st);
        // Room buttons.
        for (k, &(x, y, cw, ch)) in self.chip_rects.iter().enumerate() {
            let selected = k == self.room;
            self.canvas.fill_round_rect(
                x,
                y,
                cw,
                ch,
                ch / 2,
                if selected { TEXT } else { TILE_OFF },
            );
            let name = &self.rooms[k].1;
            self.canvas.text(
                &f::TEXT,
                x + M,
                y + ch / 2 + s(11),
                name,
                if selected { TEXT_ON_DARK } else { TEXT },
            );
        }
        for (i, idx) in self.page_tiles().into_iter().enumerate() {
            self.render_tile(st, i, idx);
        }
        if self.visible.is_empty() {
            let msg = if st.connected { "No tiles" } else { "" };
            self.canvas
                .text(&f::TEXT, PAD + s(8), self.top + s(60), msg, TEXT_DIM);
        }
        // Page dots.
        let pages = self.pages();
        if pages > 1 {
            let y = h - DOTS_H / 2 - s(8);
            let x0 = w / 2 - (pages as i32 * s(36)) / 2 + s(18);
            for p in 0..pages {
                let c = if p == self.page { DOT_ON } else { DOT_OFF };
                self.canvas.circle(x0 + p as i32 * s(36), y, s(8), c);
            }
        }
        self.pointer_drawn = self.pointer_visible();
        if self.pointer_drawn {
            let (x, y) = self.pointer;
            self.canvas.circle(x, y, s(14), Color::rgb(0, 0, 0));
            self.canvas.circle(x, y, s(11), TEXT);
        }
    }

    pub(super) fn render_header(&mut self, st: &ha::HaState) {
        let cv = &mut self.canvas;
        let now = time::unix_now();
        if now != 0 {
            let t = time::local(now, self.utc_offset, self.eu_dst);
            let clock = format!("{:02}:{:02}", t.hour, t.minute);
            let cw = cv.text(&f::CLOCK, PAD + s(8), s(140), &clock, TEXT);
            pub(super) const DAYS: [&str; 7] = [
                "Sunday",
                "Monday",
                "Tuesday",
                "Wednesday",
                "Thursday",
                "Friday",
                "Saturday",
            ];
            pub(super) const MONTHS: [&str; 12] = [
                "January",
                "February",
                "March",
                "April",
                "May",
                "June",
                "July",
                "August",
                "September",
                "October",
                "November",
                "December",
            ];
            cv.text(
                &f::TEXT_BOLD,
                PAD + s(8) + cw + s(36),
                s(92),
                DAYS[t.weekday as usize % 7],
                TEXT,
            );
            let date = format!("{} {}", t.day, MONTHS[(t.month as usize + 11) % 12]);
            cv.text(&f::TEXT, PAD + s(8) + cw + s(36), s(138), &date, TEXT_DIM);
        }
        let (status, ok) = if st.connected {
            ("Home Assistant", true)
        } else if net::with(|n| n.ipv4.is_some()).unwrap_or(false) {
            ("Connecting…", false)
        } else {
            ("Waiting for network…", false)
        };
        let sw = f::TEXT_SMALL.width(status);
        let sx = cv.w - PAD - s(8) - sw;
        let y = self.status_y;
        cv.text(
            &f::TEXT_SMALL,
            sx,
            y,
            status,
            if ok { TEXT_DIM } else { ACCENT_WARN },
        );
        cv.text(
            &f::ICON_SMALL,
            sx - s(48),
            y + s(2),
            if ok { "\u{f1eb}" } else { "\u{f071}" },
            if ok { TEXT_DIM } else { ACCENT_WARN },
        );
    }

    pub(super) fn render_tile(&mut self, st: &ha::HaState, i: usize, idx: usize) {
        let (x, y, w, h) = self.rect(i);
        let t = &self.tiles[idx];
        let e = st.entities.get(&t.entity);
        let unavailable = e.is_none_or(|e| matches!(e.state.as_str(), "unavailable" | "unknown"));
        let domain = t.entity.split('.').next().unwrap_or("");
        let preview = self.preview.filter(|(p, _)| *p == i).map(|(_, b)| b);
        let on = preview.map_or_else(|| e.is_some_and(|e| e.is_on()), |b| b > 0);
        let pressed = self
            .pressed
            .is_some_and(|(p, at)| p == i && time::now_ms() - at < 400);
        let bg = if pressed {
            TILE_PRESSED
        } else if unavailable {
            TILE_UNAVAILABLE
        } else if on && domain == "light" {
            TILE_LIGHT_ON
        } else if on && domain != "climate" {
            TILE_ON
        } else {
            TILE_OFF
        };
        let fg = if on && !pressed && domain != "climate" {
            TEXT_ON_DARK
        } else {
            TEXT
        };
        let dim = if on && !pressed && domain != "climate" {
            TEXT_ON_DARK
        } else {
            TEXT_DIM
        };
        let name = t
            .name
            .clone()
            .or_else(|| e.map(|e| e.name.clone()))
            .unwrap_or_else(|| t.entity.clone());
        let inner = w - 2 * M;
        let cv = &mut self.canvas;
        cv.fill_round_rect(x, y, w, h, RADIUS, bg);

        let Some(e) = e else {
            // Configured entity that Home Assistant doesn't know: make the mistake visible.
            cv.text(&f::ICON, x + M, y + s(88), "\u{f071}", ACCENT_WARN);
            let msg = if st.connected { "Not found" } else { "…" };
            let mw = f::TEXT.width(msg);
            cv.text(&f::TEXT, x + w - M - mw, y + s(72), msg, ACCENT_WARN);
            cv.text_fit(
                &f::TEXT_SMALL,
                x + M,
                y + h - s(84),
                &t.entity,
                inner,
                TEXT_DIM,
            );
            cv.text_fit(&f::TEXT_BOLD, x + M, y + h - s(36), &name, inner, fg);
            return;
        };

        match domain {
            "sensor" if !unavailable => {
                let iw = cv.text(&f::ICON, x + M, y + s(84), &icon_for(e).to_string(), dim);
                let vw = cv.text(&f::VALUE, x + M + iw + s(20), y + s(88), &e.state, fg);
                cv.text(&f::TEXT, x + M + iw + s(20) + vw + s(8), y + s(88), &e.unit, dim);
            }
            "climate" if !unavailable => {
                // Name top-left, mode top-right, current temperature bottom-left,
                // and [-] target [+] bottom-right.
                let cur = e.current_temp.map_or("–".to_string(), fmt_temp);
                let (plus_x, minus_x, by) = climate_buttons(x, y, w, h);
                // Big current temperature only if it fits left of the buttons.
                let big = f::VALUE.width(&cur) + s(16) <= minus_x - (x + M);
                let mut mode = capitalize(&e.state);
                if !big {
                    mode = format!("{mode} · {cur}");
                }
                let mw = f::TEXT_SMALL.width(&mode);
                cv.text(&f::TEXT_SMALL, x + w - M - mw, y + s(52), &mode, TEXT_DIM);
                cv.text_fit(&f::TEXT_BOLD, x + M, y + s(52), &name, w - 2 * M - mw - s(16), fg);
                if big {
                    cv.text(&f::VALUE, x + M, y + h - s(32), &cur, fg);
                }
                let bw = CLIMATE_BTN;
                for (bx, glyph) in [(minus_x, "–"), (plus_x, "+")] {
                    cv.fill_round_rect(bx, by, bw, bw, bw / 2, BUTTON);
                    let gw = f::TEXT_BOLD.width(glyph);
                    cv.text(
                        &f::TEXT_BOLD,
                        bx + (bw - gw) / 2,
                        by + bw / 2 + s(12),
                        glyph,
                        TEXT,
                    );
                }
                let target = e.target_temp.map_or("–".to_string(), fmt_temp);
                let tw = f::TEXT_BOLD.width(&target);
                let mid = (minus_x + bw + plus_x) / 2;
                cv.text(&f::TEXT_BOLD, mid - tw / 2, by + bw / 2 + s(12), &target, TEXT);
                return;
            }
            _ => {
                cv.text(
                    &f::ICON,
                    x + M,
                    y + s(88),
                    &icon_for(e).to_string(),
                    if on { fg } else { dim },
                );
                let mut state = if unavailable {
                    "Unavailable".to_string()
                } else {
                    capitalize(&e.state)
                };
                let bright = preview.or_else(|| e.brightness.filter(|_| on).map(|b| b as i32));
                if let Some(b) = bright.filter(|_| on) {
                    state = format!("On · {}%", (b * 100 + 127) / 255);
                }
                if let Some(p) = e.position.filter(|_| domain == "cover") {
                    state = format!("{state} · {p}%");
                }
                let sw = f::TEXT.width(&state);
                cv.text(&f::TEXT, x + w - M - sw, y + s(72), &state, dim);
                // Brightness bar for dimmable lights.
                if domain == "light" && e.dimmable && on {
                    let b = bright.unwrap_or(255).clamp(0, 255);
                    let (bx, bw, by, bh) = (x + M, w - 2 * M, y + h - s(22), s(8));
                    cv.fill_round_rect(bx, by, bw, bh, bh / 2, BAR_TRACK);
                    cv.fill_round_rect(bx, by, (bw * b / 255).max(bh), bh, bh / 2, BAR_FILL);
                }
            }
        }
        cv.text_fit(&f::TEXT_BOLD, x + M, y + h - s(36), &name, inner, fg);
    }
}
