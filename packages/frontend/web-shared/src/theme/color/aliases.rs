use crate::prelude::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColorBackground {
    SidebarSelected,
    Sidebar,
    CheckboxDark,
    CheckboxLight,
    GltfContent,
    ModalContent,
    UnderlinePrimary,
    UnderlineSecondary,
    ButtonPrimary,
    ButtonPrimaryHover,
    ButtonDisabled,
    ButtonRed,
    ButtonRedHover,
    Dropdown,
    Initial,
    /// Semi-transparent near-black overlay behind modal dialogs
    ModalOverlay,
}

impl ColorBackground {
    pub fn value(self) -> &'static str {
        match self {
            Self::SidebarSelected => "var(--bg-active)",
            Self::Sidebar => "var(--bg-1)",
            Self::GltfContent => "var(--bg-1)",
            Self::ModalContent => "var(--bg-2)",
            Self::UnderlinePrimary => ColorRaw::Accent.value(),
            Self::UnderlineSecondary => ColorRaw::MidGrey.value(),
            Self::ButtonPrimary => "var(--accent-dim)",
            Self::ButtonPrimaryHover => ColorRaw::Accent.value(),
            Self::ButtonDisabled => "var(--bg-active)",
            Self::ButtonRed => ColorRaw::Red.value(),
            Self::ButtonRedHover => ColorRaw::RedLite.value(),
            Self::Dropdown => "var(--bg-3)",
            Self::CheckboxDark => "var(--bg-3)",
            Self::CheckboxLight => "var(--accent)",
            Self::Initial => "initial",
            Self::ModalOverlay => ColorRaw::ModalOverlay.value(),
        }
    }

    pub fn class(self) -> &'static str {
        static SIDEBAR_SELECTED: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::SidebarSelected.value())
            }
        });

        static SIDEBAR: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::Sidebar.value())
            }
        });

        static GLTF_CONTENT: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::GltfContent.value())
            }
        });

        static UNDERLINE_PRIMARY: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::UnderlinePrimary.value())
            }
        });

        static UNDERLINE_SECONDARY: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::UnderlineSecondary.value())
            }
        });

        static MODAL_CONTENT: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::ModalContent.value())
            }
        });

        static BUTTON_PRIMARY: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::ButtonPrimary.value())
            }
        });

        static BUTTON_PRIMARY_HOVER: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::ButtonPrimaryHover.value())
            }
        });

        static BUTTON_DISABLED: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::ButtonDisabled.value())
            }
        });

        static BUTTON_RED: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::ButtonRed.value())
            }
        });

        static BUTTON_RED_HOVER: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::ButtonRedHover.value())
            }
        });

        static INITIAL: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::Initial.value())
            }
        });

        static DROPDOWN: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::Dropdown.value())
            }
        });

        static CHECKBOX_DARK: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::CheckboxDark.value())
            }
        });

        static CHECKBOX_LIGHT: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::CheckboxLight.value())
            }
        });

        static MODAL_OVERLAY: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("background-color", ColorBackground::ModalOverlay.value())
            }
        });

        match self {
            Self::SidebarSelected => &SIDEBAR_SELECTED,
            Self::Sidebar => &SIDEBAR,
            Self::UnderlinePrimary => &UNDERLINE_PRIMARY,
            Self::UnderlineSecondary => &UNDERLINE_SECONDARY,
            Self::ModalContent => &MODAL_CONTENT,
            Self::ButtonPrimary => &BUTTON_PRIMARY,
            Self::ButtonPrimaryHover => &BUTTON_PRIMARY_HOVER,
            Self::ButtonDisabled => &BUTTON_DISABLED,
            Self::ButtonRed => &BUTTON_RED,
            Self::ButtonRedHover => &BUTTON_RED_HOVER,
            Self::Dropdown => &DROPDOWN,
            Self::Initial => &INITIAL,
            Self::GltfContent => &GLTF_CONTENT,
            Self::CheckboxDark => &CHECKBOX_DARK,
            Self::CheckboxLight => &CHECKBOX_LIGHT,
            Self::ModalOverlay => &MODAL_OVERLAY,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColorText {
    GltfContent,
    ButtonPrimary,
    ButtonOutlinePrimary,
    ButtonOutlinePrimaryHover,
    ButtonOutlineRed,
    ButtonOutlineRedHover,
    ButtonOutlineGreen,
    ButtonOutlineGreenHover,
    Link,
    Header,
    SidebarHeader,
    Byline,
    Paragraph,
    Input,
    InputPlaceholder,
    Error,
    ErrorMuted,
    Success,
    Label,
    LabelHover,
    CheckboxDark,
    CheckboxLight,
}

impl ColorText {
    pub fn value(self) -> &'static str {
        match self {
            Self::SidebarHeader => ColorRaw::Whiteish.value(),
            Self::ButtonPrimary => ColorRaw::Whiteish.value(),
            Self::GltfContent => ColorRaw::Whiteish.value(),
            Self::ButtonOutlinePrimary => ColorRaw::Accent.value(),
            Self::ButtonOutlinePrimaryHover => ColorRaw::AccentLite.value(),
            Self::ButtonOutlineRed => ColorRaw::Red.value(),
            Self::ButtonOutlineRedHover => ColorRaw::RedLite.value(),
            Self::ButtonOutlineGreen => ColorRaw::Green.value(),
            Self::ButtonOutlineGreenHover => ColorRaw::GreenLite.value(),
            Self::Link => ColorRaw::AccentLite.value(),
            Self::Header => "var(--text-0)",
            Self::Byline => ColorRaw::MidGrey.value(),
            Self::Paragraph => "var(--text-1)",
            Self::Label => "var(--text-1)",
            Self::LabelHover => ColorRaw::AccentLite.value(),
            Self::Input => "var(--text-0)",
            Self::InputPlaceholder => "var(--text-3)",
            Self::Error => ColorRaw::Red.value(),
            Self::ErrorMuted => ColorRaw::RedLite.value(),
            Self::Success => ColorRaw::Green.value(),
            Self::CheckboxDark => "var(--text-0)",
            Self::CheckboxLight => "var(--bg-0)",
        }
    }

    pub fn class(self) -> &'static str {
        static GLTF_CONTENT: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::GltfContent.value())
            }
        });

        static BUTTON_PRIMARY: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::ButtonPrimary.value())
            }
        });

        static HEADER: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::Header.value())
            }
        });

        static SIDEBAR_HEADER: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::SidebarHeader.value())
            }
        });

        static BYLINE: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::Byline.value())
            }
        });

        static PARAGRAPH: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::Paragraph.value())
            }
        });

        static INPUT: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::Input.value())
            }
        });

        static INPUT_PLACEHOLDER: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::InputPlaceholder.value())
            }
        });

        static ERROR: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::Error.value())
            }
        });

        static ERROR_MUTED: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::ErrorMuted.value())
            }
        });

        static SUCCESS: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::Success.value())
            }
        });

        static LABEL: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::Label.value())
            }
        });

        static LABEL_HOVER: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::LabelHover.value())
            }
        });

        static BUTTON_OUTLINE_PRIMARY: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::ButtonOutlinePrimary.value())
            }
        });

        static BUTTON_OUTLINE_PRIMARY_HOVER: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::ButtonOutlinePrimaryHover.value())
            }
        });

        static BUTTON_OUTLINE_RED: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::ButtonOutlineRed.value())
            }
        });

        static BUTTON_OUTLINE_RED_HOVER: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::ButtonOutlineRedHover.value())
            }
        });

        static BUTTON_OUTLINE_GREEN: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::ButtonOutlineGreen.value())
            }
        });

        static BUTTON_OUTLINE_GREEN_HOVER: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::ButtonOutlineGreenHover.value())
            }
        });

        static LINK: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::Link.value())
            }
        });

        static CHECKBOX_DARK: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::CheckboxDark.value())
            }
        });
        static CHECKBOX_LIGHT: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorText::CheckboxLight.value())
            }
        });

        match self {
            Self::GltfContent => &GLTF_CONTENT,
            Self::ButtonPrimary => &BUTTON_PRIMARY,
            Self::Header => &HEADER,
            Self::SidebarHeader => &SIDEBAR_HEADER,
            Self::Byline => &BYLINE,
            Self::Paragraph => &PARAGRAPH,
            Self::Input => &INPUT,
            Self::InputPlaceholder => &INPUT_PLACEHOLDER,
            Self::Error => &ERROR,
            Self::ErrorMuted => &ERROR_MUTED,
            Self::Success => &SUCCESS,
            Self::Label => &LABEL,
            Self::LabelHover => &LABEL_HOVER,
            Self::ButtonOutlinePrimary => &BUTTON_OUTLINE_PRIMARY,
            Self::ButtonOutlinePrimaryHover => &BUTTON_OUTLINE_PRIMARY_HOVER,
            Self::ButtonOutlineRed => &BUTTON_OUTLINE_RED,
            Self::ButtonOutlineRedHover => &BUTTON_OUTLINE_RED_HOVER,
            Self::ButtonOutlineGreen => &BUTTON_OUTLINE_GREEN,
            Self::ButtonOutlineGreenHover => &BUTTON_OUTLINE_GREEN_HOVER,
            Self::Link => &LINK,
            Self::CheckboxDark => &CHECKBOX_DARK,
            Self::CheckboxLight => &CHECKBOX_LIGHT,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColorLabel {
    Input,
}

impl ColorLabel {
    pub fn value(self) -> &'static str {
        match self {
            Self::Input => "var(--text-1)",
        }
    }

    pub fn class(self) -> &'static str {
        static INPUT: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorLabel::Input.value())
            }
        });

        match self {
            Self::Input => &INPUT,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColorBorder {
    Input,
    Warning,
    Error,
    ErrorSubtle,
    Focus,
    UnderlinePrimary,
    UnderlineSecondary,
    ButtonOutlinePrimary,
    ButtonOutlinePrimaryHover,
    ButtonOutlineRed,
    ButtonOutlineRedHover,
    ButtonOutlineGreen,
    ButtonOutlineGreenHover,
    ButtonDisabled,
    Initial,
    CheckboxDark,
    CheckboxLight,
    /// Semi-transparent blue-grey border for modal dialog containers
    ModalContainer,
    /// Soft semi-transparent blue border for content panels
    ContentPanel,
}

impl ColorBorder {
    pub fn value(self) -> &'static str {
        match self {
            Self::Input => "var(--line)",
            Self::Warning => ColorRaw::Orange.value(),
            Self::Error => ColorRaw::Red.value(),
            Self::ErrorSubtle => ColorRaw::RedSubtle.value(),
            Self::Focus => "var(--accent-line)",
            Self::UnderlinePrimary => ColorRaw::Accent.value(),
            Self::UnderlineSecondary => ColorRaw::MidGrey.value(),
            Self::ButtonOutlinePrimary => "var(--accent-line)",
            Self::ButtonOutlinePrimaryHover => ColorRaw::Accent.value(),
            Self::ButtonOutlineRed => ColorRaw::Red.value(),
            Self::ButtonOutlineRedHover => ColorRaw::RedLite.value(),
            Self::ButtonOutlineGreen => ColorRaw::Green.value(),
            Self::ButtonOutlineGreenHover => ColorRaw::GreenLite.value(),
            Self::ButtonDisabled => "var(--line)",
            Self::Initial => "initial",
            Self::CheckboxDark => "var(--line-strong)",
            Self::CheckboxLight => "var(--accent)",
            Self::ModalContainer => ColorRaw::SurfaceBorder.value(),
            Self::ContentPanel => ColorRaw::PanelBorder.value(),
        }
    }

    pub fn class(self) -> &'static str {
        static INPUT: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::Input.value())
            }
        });

        static WARNING: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::Warning.value())
            }
        });

        static ERROR: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::Error.value())
            }
        });

        static ERROR_SUBTLE: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::ErrorSubtle.value())
            }
        });

        static FOCUS: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::Focus.value())
            }
        });

        static UNDERLINE_PRIMARY: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::UnderlinePrimary.value())
            }
        });

        static UNDERLINE_SECONDARY: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::UnderlineSecondary.value())
            }
        });

        static BUTTON_DISABLED: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::ButtonDisabled.value())
            }
        });

        static BUTTON_OUTLINE_PRIMARY: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::ButtonOutlinePrimary.value())
            }
        });

        static BUTTON_OUTLINE_PRIMARY_HOVER: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::ButtonOutlinePrimaryHover.value())
            }
        });

        static BUTTON_OUTLINE_RED: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::ButtonOutlineRed.value())
            }
        });

        static BUTTON_OUTLINE_RED_HOVER: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::ButtonOutlineRedHover.value())
            }
        });

        static BUTTON_OUTLINE_GREEN: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::ButtonOutlineGreen.value())
            }
        });

        static BUTTON_OUTLINE_GREEN_HOVER: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::ButtonOutlineGreenHover.value())
            }
        });

        static INITIAL: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::Initial.value())
            }
        });

        static CHECKBOX_DARK: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::CheckboxDark.value())
            }
        });
        static CHECKBOX_LIGHT: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::CheckboxLight.value())
            }
        });

        static MODAL_CONTAINER: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::ModalContainer.value())
            }
        });

        static CONTENT_PANEL: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("border-color", ColorBorder::ContentPanel.value())
            }
        });

        match self {
            Self::Input => &INPUT,
            Self::Warning => &WARNING,
            Self::Error => &ERROR,
            Self::ErrorSubtle => &ERROR_SUBTLE,
            Self::Focus => &FOCUS,
            Self::UnderlinePrimary => &UNDERLINE_PRIMARY,
            Self::UnderlineSecondary => &UNDERLINE_SECONDARY,
            Self::ButtonDisabled => &BUTTON_DISABLED,
            Self::Initial => &INITIAL,
            Self::ButtonOutlinePrimary => &BUTTON_OUTLINE_PRIMARY,
            Self::ButtonOutlinePrimaryHover => &BUTTON_OUTLINE_PRIMARY_HOVER,
            Self::ButtonOutlineRed => &BUTTON_OUTLINE_RED,
            Self::ButtonOutlineRedHover => &BUTTON_OUTLINE_RED_HOVER,
            Self::ButtonOutlineGreen => &BUTTON_OUTLINE_GREEN,
            Self::ButtonOutlineGreenHover => &BUTTON_OUTLINE_GREEN_HOVER,
            Self::CheckboxDark => &CHECKBOX_DARK,
            Self::CheckboxLight => &CHECKBOX_LIGHT,
            Self::ModalContainer => &MODAL_CONTAINER,
            Self::ContentPanel => &CONTENT_PANEL,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColorLogo {
    Primary,
}

impl ColorLogo {
    pub fn value(self) -> &'static str {
        match self {
            Self::Primary => ColorRaw::Accent.value(),
        }
    }
    pub fn class(self) -> &'static str {
        static PRIMARY: LazyLock<String> = LazyLock::new(|| {
            class! {
              .style("color", ColorLogo::Primary.value())
            }
        });

        match self {
            Self::Primary => &PRIMARY,
        }
    }
}
