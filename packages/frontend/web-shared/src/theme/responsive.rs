use crate::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Breakpoint {
    SmallPhone,
    Phone,
    Tablet,
    SmallDesktop,
    Desktop,
}

impl Breakpoint {
    pub fn signal() -> impl Signal<Item = Self> {
        dominator::window_size().map(|size| {
            if size.width < 600.0 {
                Breakpoint::SmallPhone
            } else if size.width < 768.0 {
                Breakpoint::Phone
            } else if size.width < 992.0 {
                Breakpoint::Tablet
            } else if size.width < 1200.0 {
                Breakpoint::SmallDesktop
            } else {
                Breakpoint::Desktop
            }
        })
    }

    pub fn font_size(&self) -> String {
        let pixels: f64 = match self {
            Breakpoint::SmallPhone => 13.0,
            Breakpoint::Phone => 14.0,
            Breakpoint::Tablet => 15.0,
            Breakpoint::SmallDesktop => 15.0,
            Breakpoint::Desktop => 16.0,
        };

        format!("{}em", pixels / 16.0)
    }
}
