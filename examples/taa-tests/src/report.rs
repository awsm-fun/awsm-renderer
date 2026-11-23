//! Machine-readable reporting, the player-tests conventions with a `TAA-TEST`
//! prefix:
//!
//! ```text
//! TAA-TEST <name>: PASS — <numbers>
//! TAA-TEST <name>: FAIL — <numbers>
//! TAA-TESTS COMPLETE: <pass>/<total>
//! ```
//!
//! Every line also goes to the `#log` element, and the aggregate to `#hud`
//! (`taa-tests: COMPLETE <pass>/<total>`), so a driver can read the whole
//! table from the DOM without paging the console.

#[derive(Default)]
pub struct Report {
    pub pass: usize,
    pub total: usize,
    pub lines: Vec<String>,
}

impl Report {
    pub fn emit(&mut self, name: &str, ok: bool, detail: &str) {
        self.total += 1;
        if ok {
            self.pass += 1;
        }
        let status = if ok { "PASS" } else { "FAIL" };
        let line = format!("TAA-TEST {name}: {status} — {detail}");
        log(&line);
        self.lines.push(line);
        set_hud(&format!(
            "taa-tests: {}/{} after {name}",
            self.pass, self.total
        ));
    }

    /// An informational line (not counted).
    pub fn info(&mut self, name: &str, detail: &str) {
        let line = format!("TAA-INFO {name}: {detail}");
        log(&line);
        self.lines.push(line);
    }

    pub fn emit_result(&mut self, name: &str, result: anyhow::Result<(bool, String)>) {
        match result {
            Ok((ok, detail)) => self.emit(name, ok, &detail),
            Err(err) => self.emit(name, false, &format!("error: {err:#}")),
        }
    }

    pub fn complete(&self) {
        log(&format!("TAA-TESTS COMPLETE: {}/{}", self.pass, self.total));
        set_hud(&format!("taa-tests: COMPLETE {}/{}", self.pass, self.total));
    }
}

/// Console + `#log` line.
pub fn log(line: &str) {
    web_sys::console::log_1(&line.into());
    if let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("log"))
    {
        let prev = el.text_content().unwrap_or_default();
        el.set_text_content(Some(&format!("{prev}{line}\n")));
    }
}

pub fn set_hud(text: &str) {
    if let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("hud"))
    {
        el.set_text_content(Some(text));
    }
}
