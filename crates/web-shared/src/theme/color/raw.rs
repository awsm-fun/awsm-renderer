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
    pub const fn value(self) -> &'static str {
        match self {
            Self::Darkest => "#05070D",
            Self::Accent => "#67A8FF",
            Self::AccentLite => "#72F5FF",
            Self::AccentVeryLight => "#A8D0FF",
            Self::Whiteish => "#E9F1FF",
            Self::Darkish => "#0B1220",
            Self::MidGrey => "#91A3C0",
            Self::Focus => "#72F5FF",
            Self::Red => "#FF4D7A",
            Self::RedLite => "#FF8FAF",
            Self::RedSubtle => "rgba(255, 77, 122, 0.35)",
            Self::Orange => "#FFC16D",
            Self::Green => "#7DFFA6",
            Self::GreyAlt1 => "#132037",
            Self::GreyAlt2 => "#0F1A2D",
            Self::PureWhite => "#FFFFFF",
            Self::GreenLite => "#CCFFDF",
            Self::CyanLite => "#DFFCFF",
            Self::ModalOverlay => "rgba(3, 10, 20, 0.72)",
            Self::SurfaceBorder => "rgba(42, 58, 82, 0.45)",
            Self::PanelBorder => "rgba(117, 166, 232, 0.25)",
            Self::BlueSilver => "#AFC2DE",
            Self::BlueIce => "#D7E5F8",
        }
    }
}
