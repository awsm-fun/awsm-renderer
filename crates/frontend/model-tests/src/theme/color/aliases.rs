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
}

impl ColorBackground {
    pub fn value(self) -> &'static str {
        match self {
            Self::SidebarSelected => ColorRaw::GreyAlt1.value(),
            Self::Sidebar => ColorRaw::Darkish.value(),
            Self::GltfContent => ColorRaw::Darkish.value(),
            Self::ModalContent => ColorRaw::Whiteish.value(),
            Self::UnderlinePrimary => ColorRaw::Accent.value(),
            Self::UnderlineSecondary => ColorRaw::MidGrey.value(),
            Self::ButtonPrimary => ColorRaw::Accent.value(),
            Self::ButtonPrimaryHover => ColorRaw::AccentLite.value(),
            Self::ButtonDisabled => ColorRaw::AccentVeryLight.value(),
            Self::ButtonRed => ColorRaw::Red.value(),
            Self::ButtonRedHover => ColorRaw::RedLite.value(),
            Self::Dropdown => ColorRaw::Whiteish.value(),
            Self::CheckboxDark => ColorRaw::Whiteish.value(),
            Self::CheckboxLight => ColorRaw::Darkish.value(),
            Self::Initial => "initial",
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
    Link,
    Header,
    SidebarHeader,
    Byline,
    Paragraph,
    Input,
    InputPlaceholder,
    Error,
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
            Self::Link => ColorRaw::Accent.value(),
            Self::Header => ColorRaw::Darkest.value(),
            Self::Byline => ColorRaw::MidGrey.value(),
            Self::Paragraph => ColorRaw::Darkish.value(),
            Self::Label => ColorRaw::Darkish.value(),
            Self::LabelHover => ColorRaw::Accent.value(),
            Self::Input => ColorRaw::Darkish.value(),
            Self::InputPlaceholder => ColorRaw::MidGrey.value(),
            Self::Error => ColorRaw::Red.value(),
            Self::Success => ColorRaw::Green.value(),
            Self::CheckboxDark => ColorRaw::Whiteish.value(),
            Self::CheckboxLight => ColorRaw::Darkish.value(),
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
            Self::Success => &SUCCESS,
            Self::Label => &LABEL,
            Self::LabelHover => &LABEL_HOVER,
            Self::ButtonOutlinePrimary => &BUTTON_OUTLINE_PRIMARY,
            Self::ButtonOutlinePrimaryHover => &BUTTON_OUTLINE_PRIMARY_HOVER,
            Self::ButtonOutlineRed => &BUTTON_OUTLINE_RED,
            Self::ButtonOutlineRedHover => &BUTTON_OUTLINE_RED_HOVER,
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
            Self::Input => ColorRaw::Darkest.value(),
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
    Focus,
    UnderlinePrimary,
    UnderlineSecondary,
    ButtonOutlinePrimary,
    ButtonOutlinePrimaryHover,
    ButtonOutlineRed,
    ButtonOutlineRedHover,
    ButtonDisabled,
    Initial,
    CheckboxDark,
    CheckboxLight,
}

impl ColorBorder {
    pub fn value(self) -> &'static str {
        match self {
            Self::Input => ColorRaw::MidGrey.value(),
            Self::Warning => ColorRaw::Orange.value(),
            Self::Error => ColorRaw::Red.value(),
            Self::Focus => ColorRaw::Focus.value(),
            Self::UnderlinePrimary => ColorRaw::Accent.value(),
            Self::UnderlineSecondary => ColorRaw::MidGrey.value(),
            Self::ButtonOutlinePrimary => ColorRaw::Accent.value(),
            Self::ButtonOutlinePrimaryHover => ColorRaw::AccentLite.value(),
            Self::ButtonOutlineRed => ColorRaw::Red.value(),
            Self::ButtonOutlineRedHover => ColorRaw::RedLite.value(),
            Self::ButtonDisabled => ColorRaw::AccentVeryLight.value(),
            Self::Initial => "initial",
            Self::CheckboxDark => ColorRaw::Whiteish.value(),
            Self::CheckboxLight => ColorRaw::Darkish.value(),
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

        match self {
            Self::Input => &INPUT,
            Self::Warning => &WARNING,
            Self::Error => &ERROR,
            Self::Focus => &FOCUS,
            Self::UnderlinePrimary => &UNDERLINE_PRIMARY,
            Self::UnderlineSecondary => &UNDERLINE_SECONDARY,
            Self::ButtonDisabled => &BUTTON_DISABLED,
            Self::Initial => &INITIAL,
            Self::ButtonOutlinePrimary => &BUTTON_OUTLINE_PRIMARY,
            Self::ButtonOutlinePrimaryHover => &BUTTON_OUTLINE_PRIMARY_HOVER,
            Self::ButtonOutlineRed => &BUTTON_OUTLINE_RED,
            Self::ButtonOutlineRedHover => &BUTTON_OUTLINE_RED_HOVER,
            Self::CheckboxDark => &CHECKBOX_DARK,
            Self::CheckboxLight => &CHECKBOX_LIGHT,
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
