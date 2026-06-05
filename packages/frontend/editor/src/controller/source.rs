// The source/sink seam is consumed by the M11 loader/saver; defined now so the
// load/import command variants + the future MCP transport have a stable contract.
#![allow(dead_code)]

//! Source/sink abstractions for project load + asset import (§5.5).
//!
//! FS file/directory pickers need a user gesture, which an external transport
//! (and headless tests) can't supply. So loading + import are written over these
//! abstractions, with **URL variants that `fetch` over HTTP** (gesture-free) for
//! the external/MCP path, and **picker variants** for interactive use. Saving
//! stays a directory handle for now; the serializer is sink-abstracted so a
//! future server/HTTP-PUT sink is a thin add.

/// Where a project is loaded from.
pub enum ProjectSource {
    /// Fetch `<base>/project.toml` + referenced files over HTTP (gesture-free).
    Url(String),
    /// A picked FS Access directory handle (interactive; needs a user gesture).
    Directory(web_sys::FileSystemDirectoryHandle),
}

/// Where a project is saved to.
pub enum ProjectSink {
    /// Write the directory tree through a picked FS Access directory handle.
    Directory(web_sys::FileSystemDirectoryHandle),
    // A future Url(String) HTTP-PUT sink for the MCP path slots in here.
}

/// Where a single imported asset's bytes come from.
pub enum AssetSource {
    /// Fetch the bytes over HTTP (gesture-free).
    Url(String),
    /// A picked `File` (interactive).
    File(web_sys::File),
}
