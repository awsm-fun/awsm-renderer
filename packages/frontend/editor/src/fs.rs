//! Thin wrapper over the File System Access API.
//!
//! The whole editor is Chromium-only by necessity (WebGPU + this API). A
//! `ProjectDir` holds a `FileSystemDirectoryHandle` rooted at the project
//! directory and exposes relative-path helpers for reading / writing files,
//! including through subdirectories (forward-slash-joined paths).

// The full FS surface is kept; some helpers (binary read, existence) are
// consumed as asset import/save grows.
#![allow(dead_code)]

use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    DirectoryPickerOptions, FileSystemDirectoryHandle, FileSystemFileHandle,
    FileSystemGetDirectoryOptions, FileSystemGetFileOptions, FileSystemHandle,
    FileSystemHandlePermissionDescriptor, FileSystemPermissionMode, FileSystemWritableFileStream,
};

#[derive(thiserror::Error, Debug)]
pub enum FsError {
    #[error("file system access is not supported in this browser")]
    Unsupported,
    #[error("user cancelled the directory picker")]
    Cancelled,
    #[error("could not get write permission on the project directory")]
    PermissionDenied,
    #[error("no such path: {0}")]
    NotFound(String),
    #[error("file system error: {0}")]
    Js(String),
}

impl From<wasm_bindgen::JsValue> for FsError {
    fn from(value: wasm_bindgen::JsValue) -> Self {
        FsError::Js(js_value_to_string(&value))
    }
}

fn js_value_to_string(value: &wasm_bindgen::JsValue) -> String {
    if let Some(s) = value.as_string() {
        return s;
    }
    if let Some(err) = value.dyn_ref::<js_sys::Error>() {
        return format!("{}: {}", err.name(), err.message());
    }
    format!("{value:?}")
}

/// A live handle to the project directory on the user's disk.
#[derive(Clone)]
pub struct ProjectDir {
    root: FileSystemDirectoryHandle,
}

impl ProjectDir {
    /// Prompt the user to pick a directory. Requests readwrite access up-front.
    pub async fn pick() -> Result<Self, FsError> {
        let window = web_sys::window().ok_or(FsError::Unsupported)?;

        let options = DirectoryPickerOptions::new();
        options.set_mode(FileSystemPermissionMode::Readwrite);

        let promise = window
            .show_directory_picker_with_options(&options)
            .map_err(|_| FsError::Unsupported)?;

        let handle_value = match JsFuture::from(promise).await {
            Ok(value) => value,
            Err(err) => {
                // AbortError = user cancelled. Everything else we treat as error.
                if is_abort_error(&err) {
                    return Err(FsError::Cancelled);
                }
                return Err(FsError::Js(js_value_to_string(&err)));
            }
        };

        let root: FileSystemDirectoryHandle = handle_value
            .dyn_into()
            .map_err(|_| FsError::Js("picker returned a non-directory handle".into()))?;

        let project = Self { root };
        project.ensure_readwrite().await?;
        Ok(project)
    }

    /// Ensure we still have readwrite permission. Re-prompts if the browser
    /// has downgraded us since the initial pick.
    pub async fn ensure_readwrite(&self) -> Result<(), FsError> {
        let handle: &FileSystemHandle = self.root.as_ref();

        let descriptor = FileSystemHandlePermissionDescriptor::new();
        descriptor.set_mode(FileSystemPermissionMode::Readwrite);

        let query = JsFuture::from(handle.query_permission_with_descriptor(&descriptor))
            .await
            .map_err(FsError::from)?;

        if query.as_string().as_deref() == Some("granted") {
            return Ok(());
        }

        let request = JsFuture::from(handle.request_permission_with_descriptor(&descriptor))
            .await
            .map_err(FsError::from)?;

        if request.as_string().as_deref() == Some("granted") {
            Ok(())
        } else {
            Err(FsError::PermissionDenied)
        }
    }

    pub fn name(&self) -> String {
        self.root.name()
    }

    /// Read a UTF-8 file relative to the project root.
    pub async fn read_text(&self, path: &str) -> Result<String, FsError> {
        let bytes = self.read_bytes(path).await?;
        String::from_utf8(bytes).map_err(|err| FsError::Js(format!("invalid utf-8: {err}")))
    }

    /// Read a binary file relative to the project root.
    pub async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let file_handle = self.resolve_file_handle(path, false).await?;
        let file_value = JsFuture::from(file_handle.get_file()).await?;
        let file: web_sys::File = file_value
            .dyn_into()
            .map_err(|_| FsError::Js("getFile() did not return a File".into()))?;
        let buffer_value = JsFuture::from(file.array_buffer()).await?;
        let buffer: js_sys::ArrayBuffer = buffer_value
            .dyn_into()
            .map_err(|_| FsError::Js("arrayBuffer() did not return an ArrayBuffer".into()))?;
        let array = Uint8Array::new(&buffer);
        let mut out = vec![0u8; array.length() as usize];
        array.copy_to(&mut out);
        Ok(out)
    }

    /// Write a UTF-8 file relative to the project root, creating any
    /// subdirectories along the way.
    pub async fn write_text(&self, path: &str, content: &str) -> Result<(), FsError> {
        self.write_bytes(path, content.as_bytes()).await
    }

    /// Write a binary file relative to the project root, creating any
    /// subdirectories along the way.
    pub async fn write_bytes(&self, path: &str, content: &[u8]) -> Result<(), FsError> {
        let file_handle = self.resolve_file_handle(path, true).await?;
        let stream_value = JsFuture::from(file_handle.create_writable()).await?;
        let stream: FileSystemWritableFileStream = stream_value
            .dyn_into()
            .map_err(|_| FsError::Js("createWritable() did not return a writable stream".into()))?;
        let uint8 = Uint8Array::new_with_length(content.len() as u32);
        uint8.copy_from(content);
        JsFuture::from(stream.write_with_buffer_source(&uint8)?).await?;
        JsFuture::from(stream.close()).await?;
        // Verify the bytes actually landed. Some File System Access backends (esp.
        // a picked cross-process directory) can resolve `close()` yet drop/truncate
        // the write — which would silently produce a partial project (missing
        // meshes / textures on reload). Re-read the file size and fail LOUD on a
        // mismatch so the save errors (and the prior good save is untouched) rather
        // than half-writing. Cheap: one getFile per file.
        let file_value = JsFuture::from(file_handle.get_file()).await?;
        let file: web_sys::File = file_value
            .dyn_into()
            .map_err(|_| FsError::Js("write verify: getFile() did not return a File".into()))?;
        let wrote = file.size() as usize;
        if wrote != content.len() {
            return Err(FsError::Js(format!(
                "write verify failed for {path}: wrote {} bytes but file is {wrote} \
                 (File System Access dropped/truncated the write)",
                content.len()
            )));
        }
        Ok(())
    }

    /// Does `path` exist as a file under the project root?
    pub async fn file_exists(&self, path: &str) -> bool {
        self.resolve_file_handle(path, false).await.is_ok()
    }

    /// Walk a forward-slash-joined path, optionally creating missing
    /// directories, returning the final `FileSystemFileHandle`.
    async fn resolve_file_handle(
        &self,
        path: &str,
        create: bool,
    ) -> Result<FileSystemFileHandle, FsError> {
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Err(FsError::NotFound(path.into()));
        }
        let mut parts = trimmed.split('/').collect::<Vec<_>>();
        let file_name = parts
            .pop()
            .ok_or_else(|| FsError::NotFound(path.into()))?
            .to_string();

        let mut dir = self.root.clone();
        for segment in parts {
            if segment.is_empty() || segment == "." {
                continue;
            }
            if segment == ".." {
                return Err(FsError::Js(format!(
                    "relative paths must not escape the project directory: {path}"
                )));
            }
            let options = FileSystemGetDirectoryOptions::new();
            options.set_create(create);
            let next_value = match JsFuture::from(
                dir.get_directory_handle_with_options(segment, &options),
            )
            .await
            {
                Ok(value) => value,
                Err(err) if is_not_found_error(&err) => {
                    return Err(FsError::NotFound(path.into()));
                }
                Err(err) => return Err(FsError::Js(js_value_to_string(&err))),
            };
            dir = next_value
                .dyn_into()
                .map_err(|_| FsError::Js("getDirectoryHandle did not return a directory".into()))?;
        }

        let options = FileSystemGetFileOptions::new();
        options.set_create(create);
        let handle_value =
            match JsFuture::from(dir.get_file_handle_with_options(&file_name, &options)).await {
                Ok(value) => value,
                Err(err) if is_not_found_error(&err) => {
                    return Err(FsError::NotFound(path.into()));
                }
                Err(err) => return Err(FsError::Js(js_value_to_string(&err))),
            };
        handle_value
            .dyn_into()
            .map_err(|_| FsError::Js("getFileHandle did not return a file handle".into()))
    }
}

fn is_abort_error(err: &wasm_bindgen::JsValue) -> bool {
    err.dyn_ref::<js_sys::Error>()
        .map(|e| e.name().as_string().as_deref() == Some("AbortError"))
        .unwrap_or(false)
}

fn is_not_found_error(err: &wasm_bindgen::JsValue) -> bool {
    err.dyn_ref::<js_sys::Error>()
        .map(|e| e.name().as_string().as_deref() == Some("NotFoundError"))
        .unwrap_or(false)
}
