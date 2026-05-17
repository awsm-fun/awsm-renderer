use crate::{models::collections::GltfId, prelude::*};

#[derive(Debug, Clone)]
pub enum Route {
    App(AppRoute),
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub enum TopLevelRoute {
    App,
    NotFound,
}

#[derive(Debug, Clone)]
pub enum AppRoute {
    Init,
    Model(GltfId),
}

impl Route {
    pub fn from_url(url: &str) -> Self {
        let url = web_sys::Url::new(url).unwrap_throw();
        let paths = url.pathname();
        let paths = paths
            .split('/')
            // skip all the roots (1 for the domain, 1 for each part of root path)
            .skip(
                CONFIG
                    .root_base_uri_path
                    .chars()
                    .filter(|c| *c == '/')
                    .count()
                    + 1,
            )
            .collect::<Vec<_>>();
        let paths = paths.as_slice();

        // if we need, we can get query params like:
        //let uid = url.search_params().get("uid");

        match paths {
            //[""] => Self::App(AppRoute::Init),
            [""] => Self::App(AppRoute::Model(GltfId::default())),
            ["app", app_route @ ..] => match *app_route {
                ["init"] => Self::App(AppRoute::Init),
                ["model", model] => Self::App(AppRoute::Model(match GltfId::try_from(model) {
                    Ok(gltf_id) => gltf_id,
                    Err(_) => {
                        tracing::error!("Invalid GltfId: {}", model);
                        return Self::NotFound;
                    }
                })),
                _ => Self::NotFound,
            },
            _ => Self::NotFound,
        }
    }

    pub fn link(&self) -> String {
        let s = format!("{}/{}", CONFIG.root_base_uri_path, self);
        let s = s.trim_end_matches(r#"//"#).to_string();

        s
    }

    pub fn go_to_url(&self) {
        dominator::routing::go_to_url(&self.link());
    }

    #[allow(dead_code)]
    pub fn hard_redirect(&self) {
        let location = web_sys::window().unwrap_throw().location();
        let s: String = self.link();
        location.set_href(&s).unwrap_throw();
    }

    pub fn signal() -> impl Signal<Item = Route> {
        dominator::routing::url()
            .signal_cloned()
            .map(|url| Route::from_url(&url))
    }

    pub fn get() -> Route {
        Route::from_url(&dominator::routing::url().lock_ref())
    }
}

impl TopLevelRoute {
    pub fn signal() -> impl Signal<Item = TopLevelRoute> {
        Route::signal()
            .map(|route| match route {
                Route::App(_) => Self::App,
                Route::NotFound => Self::NotFound,
            })
            .dedupe()
    }
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s: String = match self {
            Route::App(app_route) => match app_route {
                AppRoute::Init => "/".to_string(),
                _ => format!("app/{}", app_route),
            },
            Route::NotFound => "404".to_string(),
        };
        write!(f, "{}", s)
    }
}

impl std::fmt::Display for AppRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s: String = match self {
            AppRoute::Init => "init".to_string(),
            AppRoute::Model(gltf_id) => format!("model/{}", gltf_id),
        };
        write!(f, "{}", s)
    }
}
