#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColorRaw {
    Darkest,
    Accent,
    Whiteish,
    Darkish,
    MidGrey,
    AccentLite,
    Focus,
    Red,
    RedLite,
    RedSubtle,
    Orange,
    Green,
    AccentVeryLight,
    GreyAlt1,
    GreyAlt2,
    PureWhite,
    /// Light green for text on dark green surfaces (e.g. action link labels)
    GreenLite,
    /// Light cyan for text on dark cyan surfaces (e.g. CTA link labels)
    CyanLite,
    /// Semi-transparent near-black for modal backdrop overlays
    ModalOverlay,
    /// Semi-transparent blue-grey for borders on dark surfaces (modal, cards)
    SurfaceBorder,
    /// Soft semi-transparent blue for borders on content panels
    PanelBorder,
    /// Muted blue-silver for secondary text on dark surfaces (e.g. nav labels)
    BlueSilver,
    /// Light ice-blue for hover-state text on dark surfaces
    BlueIce,
}

impl ColorRaw {
    /// Resolves to a prototype design token (`tokens.css`, injected at
    /// `:root` by [`crate::theme::stylesheet::init`]). Variant names are kept
    /// for backwards source-compat, but the values now follow the graphite/
    /// slate palette — e.g. `Accent` is the restrained azure, `Whiteish` is the
    /// brightest text, `Darkish` is a panel surface.
    pub const fn value(self) -> &'static str {
        match self {
            Self::Darkest => "var(--bg-0)",
            Self::Accent => "var(--accent)",
            Self::AccentLite => "var(--accent-bright)",
            Self::AccentVeryLight => "var(--accent-bright)",
            Self::Whiteish => "var(--text-0)",
            Self::Darkish => "var(--bg-1)",
            Self::MidGrey => "var(--text-2)",
            Self::Focus => "var(--accent-bright)",
            Self::Red => "var(--danger)",
            Self::RedLite => "var(--danger-bright)",
            Self::RedSubtle => "var(--danger-soft)",
            Self::Orange => "var(--warn)",
            Self::Green => "var(--ok)",
            Self::GreyAlt1 => "var(--bg-2)",
            Self::GreyAlt2 => "var(--bg-1)",
            Self::PureWhite => "#ffffff",
            Self::GreenLite => "var(--ok)",
            Self::CyanLite => "var(--accent-bright)",
            Self::ModalOverlay => "oklch(0 0 0 / 0.55)",
            Self::SurfaceBorder => "var(--line)",
            Self::PanelBorder => "var(--line-soft)",
            Self::BlueSilver => "var(--text-2)",
            Self::BlueIce => "var(--text-1)",
        }
    }
}
