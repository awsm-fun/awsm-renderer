use crate::prelude::*;

// Graphite/slate design system (prototype `tokens.css`): system-ui chrome +
// JetBrains Mono for code and tabular numerics.
pub const FONT_FAMILY_BODY: &str =
    r#"ui-sans-serif, system-ui, -apple-system, "Segoe UI", "Helvetica Neue", sans-serif"#;
pub const FONT_FAMILY_DISPLAY: &str = FONT_FAMILY_BODY;
pub const FONT_FAMILY_MONO: &str = r#""JetBrains Mono", ui-monospace, "SF Mono", Menlo, monospace"#;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextDirection {
    Ltr,
    Rtl,
}

impl TextDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ltr => "ltr",
            Self::Rtl => "rtl",
        }
    }

    pub fn into_class(self) -> &'static str {
        static RTL: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("dir", "rtl")
            }
        });
        static LTR: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("dir", "ltr")
            }
        });

        match self {
            Self::Ltr => &LTR,
            Self::Rtl => &RTL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FontSize {
    H1,
    H2,
    H3,
    Lg,
    Md,
    Sm,
    Xlg,
}

impl FontSize {
    pub fn value(self) -> &'static str {
        match self {
            Self::H1 => "2.8rem",
            Self::H2 => "2.1rem",
            Self::H3 => "1.5rem",
            Self::Lg => "1.08rem",
            Self::Md => "0.96rem",
            Self::Sm => "0.82rem",
            Self::Xlg => "1.3rem",
        }
    }

    pub fn class(self) -> &'static str {
        // these could all individually have .style_signal
        // driven from Breakpoint::signal()
        // but instead we just set the font-size directly
        // on the root element and rems flow from thereA
        static H1: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("font-size", "2.8rem")
            }
        });

        static H2: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("font-size", "2.1rem")
            }
        });

        static H3: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("font-size", "1.5rem")
            }
        });

        static LG: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("font-size", "1.08rem")
            }
        });

        static MD: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("font-size", "0.96rem")
            }
        });

        static SM: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("font-size", "0.82rem")
            }
        });

        static XLG: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("font-size", "1.3rem")
            }
        });

        match self {
            Self::H1 => &H1,
            Self::H2 => &H2,
            Self::H3 => &H3,
            Self::Lg => &LG,
            Self::Md => &MD,
            Self::Sm => &SM,
            Self::Xlg => &XLG,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FontWeight {
    SemiBold,
    Bold,
}

impl FontWeight {
    pub fn class(self) -> &'static str {
        static SEMI_BOLD: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("font-weight", "600")
            }
        });

        static BOLD: LazyLock<String> = LazyLock::new(|| {
            class! {
                .style("font-weight", "700")
            }
        });

        match self {
            Self::SemiBold => &SEMI_BOLD,
            Self::Bold => &BOLD,
        }
    }
}
