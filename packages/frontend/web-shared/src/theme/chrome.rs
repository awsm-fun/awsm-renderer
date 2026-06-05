//! "Chrome" surface tokens — panel fills, borders, shadows, filters used by the
//! editor frame. Re-mapped onto the graphite/slate design tokens
//! (`tokens.css`, injected at `:root`). The former cyan-neon gradients flatten
//! to layered graphite surfaces; enum variants are preserved for source-compat.

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
            Self::Rule => "var(--line)",
            Self::PanelBorder => "var(--line-soft)",
            Self::CtaBorder => "var(--accent-line)",
            Self::CtaBorderHover => "var(--accent)",
            Self::CardBorder => "var(--line)",
            Self::CardBorderHover => "var(--accent-line)",
            Self::ButtonCyanBorder => "var(--line)",
            Self::ButtonCyanBorderHover => "var(--accent-line)",
            Self::ButtonGreenBorder => "color-mix(in oklch, var(--ok) 40%, transparent)",
            Self::ButtonGreenBorderHover => "var(--ok)",
            Self::FieldBorder => "var(--line)",
            Self::DangerBorder => "color-mix(in oklch, var(--danger) 50%, transparent)",
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
            Self::Frame => "var(--bg-2)",
            Self::Canvas => "var(--bg-0)",
            Self::Panel => "var(--bg-1)",
            Self::ContentPanel => "var(--bg-1)",
            Self::Selection => "var(--accent-ghost)",
            Self::Field => "var(--bg-3)",
            Self::CtaCyan => "var(--accent-ghost)",
            Self::ButtonCyan => "var(--bg-2)",
            Self::ButtonGreen => "var(--ok-soft)",
            Self::Card => "var(--bg-2)",
            Self::CardHalo => "radial-gradient(circle, var(--accent-ghost) 0%, transparent 70%)",
            Self::Recessed => "var(--bg-3)",
            Self::RecessedHover => "var(--bg-hover)",
            Self::RecessedActive => "var(--bg-active)",
            Self::DangerRecessed => "var(--danger-soft)",
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
            Self::None => "none",
            Self::FieldInset => "inset 0 1px 0 oklch(1 0 0 / 0.04)",
            Self::CtaRest => "var(--shadow-1)",
            Self::CtaHover => "var(--shadow-2)",
            Self::CardRest => "var(--shadow-1)",
            Self::CardHover => "var(--shadow-2)",
            Self::ButtonCyanHover => "var(--shadow-1)",
            Self::ButtonGreenHover => "var(--shadow-1)",
            Self::HeroTitle => "none",
            Self::NavLinkHover => "none",
            Self::NavLinkActive => "none",
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
            Self::CardIconHover => "none",
        }
    }
}
