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
    Orange,
    Green,
    AccentVeryLight,
    GreyAlt1,
    GreyAlt2,
    PureWhite,
}

impl ColorRaw {
    pub const fn value(self) -> &'static str {
        match self {
            Self::Darkest => "#11131A",
            Self::Accent => "#3375BB",
            Self::AccentLite => "#6FA1D8",
            Self::AccentVeryLight => "#9FC1E5",
            Self::Whiteish => "#FAFAFA",
            Self::Darkish => "#45474F",
            Self::MidGrey => "#92949F",
            Self::Focus => "#73A2FF",
            Self::Red => "#E00C0C",
            Self::RedLite => "#FF4D4D",
            Self::Orange => "#ED933F",
            Self::Green => "#3AD365",
            Self::GreyAlt1 => "#D9D9D9",
            Self::GreyAlt2 => "#EFEFEF",
            Self::PureWhite => "#FFFFFF",
        }
    }
}
