//! A "?" help button that opens a definition-list modal explaining a group of
//! settings. Drop it into a `Section` / `DrawerSection` header via `.right(...)`
//! so a panel of terse controls can carry its own inline documentation.

use crate::prelude::*;

/// A "?" icon button that opens a modal titled `title`, listing each
/// `(term, description)` pair in `entries`. `entries` is owned (static strs) so
/// the modal content can be rebuilt on each open — `Modal::open` wants `Fn`.
pub fn settings_help_button(
    title: impl Into<String>,
    entries: Vec<(&'static str, &'static str)>,
) -> Dom {
    let title = title.into();
    IconBtn::new("help")
        .size(13.0)
        .title("What do these settings do?")
        .on_click(move || {
            let title = title.clone();
            let entries = entries.clone();
            Modal::open(move || help_card(&title, &entries));
        })
        .render()
}

fn help_card(title: &str, entries: &[(&'static str, &'static str)]) -> Dom {
    ModalCard::new(title)
        .width(460.0)
        .child(html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            .style("gap", "12px")
            .style("font-size", "12.5px")
            .style("color", "var(--text-1)")
            .style("line-height", "1.5")
            .style("max-height", "60vh")
            .style("overflow-y", "auto")
            .children(entries.iter().map(|(term, desc)| {
                html!("div", {
                    .style("display", "flex")
                    .style("flex-direction", "column")
                    .style("gap", "2px")
                    .child(html!("strong", {
                        .style("color", "var(--text-0)")
                        .text(term)
                    }))
                    .child(html!("span", { .text(desc) }))
                })
            }))
        }))
        .footer(
            Btn::new()
                .label("Close")
                .variant(BtnVariant::Primary)
                .on_click(Modal::close)
                .render(),
        )
        .render()
}
