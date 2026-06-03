use std::sync::LazyLock;

use crate::{
    error::{WebError, WebResult},
    util::window::WINDOW,
};

static LOCAL_STORAGE: LazyLock<web_sys::Storage> = LazyLock::new(|| {
    WINDOW
        .local_storage()
        .expect("Failed to access local storage")
        .expect("Local storage is unavailable in this runtime")
});

pub fn has_local_storage(key: &str) -> WebResult<bool> {
    LOCAL_STORAGE
        .get_item(key)
        .map_err(WebError::LocalStorage)
        .map(|opt| opt.is_some())
}

pub fn get_local_storage(key: &str) -> WebResult<Option<String>> {
    LOCAL_STORAGE.get_item(key).map_err(WebError::LocalStorage)
}

pub fn set_local_storage(key: &str, value: &str) -> WebResult<()> {
    LOCAL_STORAGE
        .set_item(key, value)
        .map_err(WebError::LocalStorage)
}

pub fn delete_local_storage(key: &str) -> WebResult<()> {
    LOCAL_STORAGE.delete(key).map_err(WebError::LocalStorage)
}
