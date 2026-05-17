#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChromeColor {
    Rule,
    PanelBorder,
    CtaBorder,
    CtaBorderHover,
    CardBorder,
    CardBorderHover,
    ButtonCyanBorder,
    ButtonCyanBorderHover,
    ButtonGreenBorder,
    ButtonGreenBorderHover,
    FieldBorder,
    DangerBorder,
}

impl ChromeColor {
    pub const fn value(self) -> &'static str {
        match self {
            Self::Rule => "rgba(112, 185, 255, 0.25)",
            Self::PanelBorder => "rgba(114, 245, 255, 0.3)",
            Self::CtaBorder => "rgba(114, 245, 255, 0.5)",
            Self::CtaBorderHover => "rgba(114, 245, 255, 0.9)",
            Self::CardBorder => "rgba(114, 245, 255, 0.36)",
            Self::CardBorderHover => "rgba(114, 245, 255, 0.78)",
            Self::ButtonCyanBorder => "rgba(112, 185, 255, 0.35)",
            Self::ButtonCyanBorderHover => "rgba(114, 245, 255, 0.8)",
            Self::ButtonGreenBorder => "rgba(125, 255, 166, 0.45)",
            Self::ButtonGreenBorderHover => "rgba(125, 255, 166, 0.85)",
            Self::FieldBorder => "rgba(112, 185, 255, 0.45)",
            Self::DangerBorder => "rgba(255, 77, 122, 0.5)",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChromeFill {
    Frame,
    Canvas,
    Panel,
    ContentPanel,
    Selection,
    Field,
    CtaCyan,
    ButtonCyan,
    ButtonGreen,
    Card,
    CardHalo,
    Recessed,
    RecessedHover,
    RecessedActive,
    DangerRecessed,
}

impl ChromeFill {
    pub const fn value(self) -> &'static str {
        match self {
            Self::Frame => {
                "linear-gradient(180deg, rgba(18, 33, 58, 0.84), rgba(12, 23, 42, 0.78))"
            }
            Self::Canvas => {
                "linear-gradient(180deg, rgba(9, 18, 34, 0.98), rgba(8, 16, 31, 0.99))"
            }
            Self::Panel => {
                "linear-gradient(160deg, rgba(12, 25, 45, 0.78), rgba(8, 17, 31, 0.74))"
            }
            Self::ContentPanel => {
                "linear-gradient(160deg, rgba(16, 29, 53, 0.58), rgba(10, 17, 34, 0.35))"
            }
            Self::Selection => "rgba(143, 168, 206, 0.22)",
            Self::Field => "rgba(7, 14, 27, 0.74)",
            Self::CtaCyan => {
                "linear-gradient(120deg, rgba(114, 245, 255, 0.18), rgba(103, 168, 255, 0.14))"
            }
            Self::ButtonCyan => {
                "linear-gradient(120deg, rgba(114, 245, 255, 0.16), rgba(103, 168, 255, 0.1))"
            }
            Self::ButtonGreen => {
                "linear-gradient(120deg, rgba(125, 255, 166, 0.16), rgba(114, 245, 255, 0.1))"
            }
            Self::Card => {
                "linear-gradient(155deg, rgba(12, 24, 46, 0.88), rgba(8, 17, 31, 0.84))"
            }
            Self::CardHalo => {
                "radial-gradient(circle, rgba(114,245,255,0.24) 0%, rgba(114,245,255,0.08) 33%, rgba(114,245,255,0.0) 70%)"
            }
            Self::Recessed => "rgba(12, 23, 41, 0.45)",
            Self::RecessedHover => "rgba(16, 31, 56, 0.62)",
            Self::RecessedActive => {
                "linear-gradient(135deg, rgba(16, 31, 56, 0.72), rgba(12, 24, 46, 0.58))"
            }
            Self::DangerRecessed => "rgba(56, 16, 28, 0.45)",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChromeShadow {
    None,
    FieldInset,
    CtaRest,
    CtaHover,
    CardRest,
    CardHover,
    ButtonCyanHover,
    ButtonGreenHover,
    HeroTitle,
    NavLinkHover,
    NavLinkActive,
}

impl ChromeShadow {
    pub const fn value(self) -> &'static str {
        match self {
            Self::None => "0 0 0 rgba(0, 0, 0, 0)",
            Self::FieldInset => "inset 0 1px 0 rgba(255,255,255,0.04)",
            Self::CtaRest => "0 0 0 rgba(114, 245, 255, 0)",
            Self::CtaHover => "0 0 20px rgba(114, 245, 255, 0.25)",
            Self::CardRest => "0 0 0 rgba(114, 245, 255, 0), inset 0 1px 0 rgba(255,255,255,0.06)",
            Self::CardHover => {
                "0 14px 32px rgba(4, 18, 40, 0.6), 0 0 28px rgba(114, 245, 255, 0.28), inset 0 1px 0 rgba(255,255,255,0.12)"
            }
            Self::ButtonCyanHover => "0 0 24px rgba(114, 245, 255, 0.26)",
            Self::ButtonGreenHover => "0 0 22px rgba(125, 255, 166, 0.24)",
            Self::HeroTitle => {
                "0 0 16px rgba(103, 168, 255, 0.35), 0 0 26px rgba(114, 245, 255, 0.25)"
            }
            Self::NavLinkHover => "0 0 12px rgba(103, 168, 255, 0.7)",
            Self::NavLinkActive => "0 0 12px rgba(114, 245, 255, 0.35)",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChromeFilter {
    None,
    CardIconHover,
}

impl ChromeFilter {
    pub const fn value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::CardIconHover => "drop-shadow(0 0 10px rgba(114, 245, 255, 0.45))",
        }
    }
}
