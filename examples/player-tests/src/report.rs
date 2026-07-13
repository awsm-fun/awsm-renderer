//! The machine-readable console reporter — one line per check, plus the
//! terminator line a headless driver waits for.

/// Collects pass/fail tallies and prints the `PLAYER-TEST` lines.
#[derive(Default)]
pub struct Report {
    pub pass: usize,
    pub total: usize,
}

impl Report {
    /// Emit one check line and fold it into the tally.
    pub fn emit(&mut self, name: &str, ok: bool, detail: &str) {
        self.total += 1;
        if ok {
            self.pass += 1;
        }
        let status = if ok { "PASS" } else { "FAIL" };
        web_sys::console::log_1(&format!("PLAYER-TEST {name}: {status} — {detail}").into());
        set_hud(&format!(
            "player-tests: {}/{} after {name}",
            self.pass, self.total
        ));
    }

    /// Emit a check line from a `Result` (Ok = PASS with its detail).
    pub fn emit_result(&mut self, name: &str, result: anyhow::Result<String>) {
        match result {
            Ok(detail) => self.emit(name, true, &detail),
            Err(err) => self.emit(name, false, &format!("{err:#}")),
        }
    }

    /// The terminator line the driver greps for.
    pub fn complete(&self) {
        web_sys::console::log_1(
            &format!("PLAYER-TESTS COMPLETE: {}/{}", self.pass, self.total).into(),
        );
        set_hud(&format!(
            "player-tests: COMPLETE {}/{}",
            self.pass, self.total
        ));
    }
}

/// Best-effort progress text in the corner of the page.
pub fn set_hud(text: &str) {
    if let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("hud"))
    {
        el.set_text_content(Some(text));
    }
}
