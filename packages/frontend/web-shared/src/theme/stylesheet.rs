use crate::theme::typography::{FONT_FAMILY_BODY, FONT_FAMILY_MONO};
use dominator::stylesheet;

/// Initialise the global design-system stylesheet.
///
/// The token set is ported verbatim from the editor design prototype
/// (`tokens.css`) — a graphite/slate pro-DCC palette in OKLCH. These `:root`
/// custom properties are the single source of truth; the semantic theme enums
/// (`ColorBackground`, `ColorText`, `ChromeFill`, …) resolve to `var(--…)`
/// references into this block.
pub fn init() {
    // ---- :root design tokens (graphite/slate, OKLCH) ----
    stylesheet!(":root", {
        .style("box-sizing", "border-box")
        .style("font-size", "13px")

        // accent — a single restrained azure (user-tweakable at runtime).
        .style("--accent", "#5b8dd6")

        // surfaces (cool-neutral graphite)
        .style("--bg-0", "oklch(0.155 0.006 255)")   // deepest — viewport / app void
        .style("--bg-1", "oklch(0.196 0.006 255)")   // panel base
        .style("--bg-2", "oklch(0.228 0.007 255)")   // elevated / headers / toolbar
        .style("--bg-3", "oklch(0.150 0.006 255)")   // input well / inset
        .style("--bg-hover", "oklch(0.270 0.009 255)")
        .style("--bg-active", "oklch(0.305 0.010 255)")

        // lines
        .style("--line", "oklch(0.315 0.008 255)")
        .style("--line-soft", "oklch(0.262 0.007 255)")
        .style("--line-strong", "oklch(0.38 0.010 255)")

        // text
        .style("--text-0", "oklch(0.945 0.004 255)")
        .style("--text-1", "oklch(0.715 0.007 255)")
        .style("--text-2", "oklch(0.560 0.007 255)")
        .style("--text-3", "oklch(0.440 0.007 255)")

        // accent derivations
        .style("--accent-bright", "color-mix(in oklch, var(--accent) 78%, white)")
        .style("--accent-dim", "color-mix(in oklch, var(--accent) 82%, black)")
        .style("--accent-ghost", "color-mix(in oklch, var(--accent) 15%, transparent)")
        .style("--accent-line", "color-mix(in oklch, var(--accent) 42%, transparent)")

        // functional
        .style("--select", "oklch(0.760 0.150 65)")   // warm amber — viewport selection only
        .style("--select-soft", "oklch(0.760 0.150 65 / 0.16)")
        .style("--danger", "oklch(0.650 0.170 25)")
        .style("--danger-soft", "oklch(0.650 0.170 25 / 0.16)")
        .style("--danger-bright", "oklch(0.730 0.150 25)")
        .style("--ok", "oklch(0.740 0.130 150)")
        .style("--ok-soft", "oklch(0.740 0.130 150 / 0.14)")
        .style("--warn", "oklch(0.800 0.130 85)")
        .style("--warn-soft", "oklch(0.800 0.130 85 / 0.14)")

        // axis colors for gizmo / vector fields
        .style("--axis-x", "oklch(0.640 0.200 25)")
        .style("--axis-y", "oklch(0.720 0.170 145)")
        .style("--axis-z", "oklch(0.660 0.150 250)")

        // density (driven by the Settings drawer: 0..1 -> control scale).
        // row-h/pad-x/gap/section-gap derive from it so a single slider
        // retunes the whole inspector's compactness, exactly as the prototype.
        .style("--density", "0.55")
        .style("--row-h", "calc(24px + 8px * (1 - var(--density)))")   // ~28px default
        .style("--pad-x", "calc(8px + 6px * (1 - var(--density)))")
        .style("--gap", "calc(6px + 5px * (1 - var(--density)))")
        .style("--section-gap", "calc(12px + 10px * (1 - var(--density)))")

        // radii
        .style("--r1", "4px")
        .style("--r2", "6px")
        .style("--r3", "9px")
        .style("--r4", "13px")

        // type
        .style("--font", FONT_FAMILY_BODY)
        .style("--mono", FONT_FAMILY_MONO)

        // code syntax (muted, low-sat to fit graphite)
        .style("--tk-kw", "oklch(0.74 0.12 320)")
        .style("--tk-type", "oklch(0.74 0.11 215)")
        .style("--tk-fn", "oklch(0.80 0.11 90)")
        .style("--tk-num", "oklch(0.78 0.10 40)")
        .style("--tk-var", "oklch(0.76 0.10 160)")
        .style("--tk-id", "oklch(0.86 0.006 255)")
        .style("--tk-op", "oklch(0.62 0.01 255)")
        .style("--tk-cm", "oklch(0.50 0.012 255)")

        // shadows
        .style("--shadow-1", "0 1px 2px oklch(0 0 0 / 0.35)")
        .style("--shadow-2", "0 6px 18px -6px oklch(0 0 0 / 0.55), 0 2px 6px oklch(0 0 0 / 0.35)")
        .style("--shadow-3", "0 18px 50px -12px oklch(0 0 0 / 0.70), 0 4px 12px oklch(0 0 0 / 0.4)")
    });

    stylesheet!("*, ::before, ::after", {
        .style("box-sizing", "inherit")
    });

    stylesheet!("html, body", {
        .style("height", "100%")
        .style("margin", "0")
        .style("padding", "0")
        .style("font-family", "var(--font)")
        .style("font-size", "13px")
        .style("line-height", "1.4")
        .style("background", "var(--bg-0)")
        .style("color", "var(--text-0)")
        .style("-webkit-font-smoothing", "antialiased")
        .style("text-rendering", "optimizeLegibility")
        .style("overflow", "hidden")
    });

    stylesheet!("a", {
        .style("all", "unset")
        .style("cursor", "pointer")
        .style("color", "inherit")
    });

    stylesheet!("input, button, select, textarea", {
        .style("font-family", "inherit")
    });

    // Normalize native button rendering so interaction states never fall back
    // to browser default white/blue styles after click/focus.
    stylesheet!("button", {
        .style("appearance", "none")
        .style("-webkit-appearance", "none")
        .style("background", "none")
        .style("background-color", "transparent")
        .style("background-image", "none")
        .style("border", "0")
        .style("color", "inherit")
        .style("font-family", "var(--font)")
    });

    stylesheet!("::selection", {
        .style("background", "var(--accent-ghost)")
    });

    // Slim pro-tool scrollbars.
    stylesheet!("::-webkit-scrollbar", {
        .style("width", "10px")
        .style("height", "10px")
    });
    stylesheet!("::-webkit-scrollbar-thumb", {
        .style("background", "oklch(0.34 0.008 255)")
        .style("border", "3px solid transparent")
        .style("background-clip", "padding-box")
        .style("border-radius", "10px")
    });
    stylesheet!("::-webkit-scrollbar-thumb:hover", {
        .style("background", "oklch(0.42 0.010 255)")
        .style("background-clip", "padding-box")
    });
    stylesheet!("::-webkit-scrollbar-corner", {
        .style("background", "transparent")
    });

    // Shared utility classes used directly by the editor chrome.
    stylesheet!(".mono", {
        .style("font-family", "var(--mono)")
        .style("font-feature-settings", "\"tnum\" 1")
    });
    stylesheet!(".kicker", {
        .style("font-size", "10.5px")
        .style("font-weight", "650")
        .style("letter-spacing", "0.09em")
        .style("text-transform", "uppercase")
        .style("color", "var(--text-2)")
        .style("user-select", "none")
    });
    // Generic transition utility (prototype `.t`): background/color are NOT
    // transitioned so selection/active states update instantly.
    stylesheet!(".t", {
        .style("transition", "border-color .12s ease, box-shadow .12s ease, transform .12s ease")
    });
    // Keyboard-only focus ring (prototype `.focusring`): a double box-shadow
    // (bg-1 spacer + accent ring) shown for :focus-visible only, so mouse
    // clicks don't paint a ring but tab-navigation does.
    stylesheet!(".focusring:focus-visible", {
        .style("outline", "none")
        .style("box-shadow", "0 0 0 1.5px var(--bg-1), 0 0 0 3px var(--accent-line)")
    });

    // h1-h3 keep a sane default for any incidental headings.
    stylesheet!("h1, h2, h3", {
        .style("font-family", "var(--font)")
        .style("font-weight", "650")
        .style("margin", "0")
    });
}
