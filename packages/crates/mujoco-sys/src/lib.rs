//! Runtime-loaded FFI to the official MuJoCo library.
//!
//! MuJoCo's own C code is the ONLY thing that ever turns MJCF/URDF/`.mjb` into a
//! compiled `mjModel` — we never parse those formats (see `docs/plans/mujoco.md`).
//! This crate is the whole of that seam: dlopen the release library, load a model,
//! read `mjModel` fields as safe slices. Native tools only; never wasm, never the
//! renderer.
//!
//! See `README.md` for why this dlopens instead of linking, and how to install
//! MuJoCo.

#[allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    clippy::all,
    rustdoc::all
)]
pub mod bindings;

use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};

pub use bindings::{mjData, mjModel, mjtGeom, mjtObj};

/// MuJoCo release these bindings were generated against, in `mj_version()`'s
/// encoding (`major * 1_000_000 + minor * 1_000 + patch`, e.g. `3_011_000`).
///
/// Taken straight from the generated bindings' `mjVERSION_HEADER`, so regenerating
/// against a new release moves it automatically — there is no second place to
/// forget to bump.
///
/// `mjModel`/`mjData` are plain C structs whose layout moves between releases, so
/// loading a different library would silently misread every field. [`Library::load`]
/// refuses a mismatch rather than producing subtly wrong geometry.
pub const EXPECTED_VERSION: i32 = bindings::mjVERSION_HEADER as i32;

/// Human-readable form of [`EXPECTED_VERSION`], e.g. `"3.11.0"`.
pub fn expected_version_string() -> String {
    let v = EXPECTED_VERSION;
    format!("{}.{}.{}", v / 1_000_000, (v / 1_000) % 1_000, v % 1_000)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "could not load the MuJoCo library{}: {source}\n\
         Install a MuJoCo {} release and set MUJOCO_DIR \
         (see packages/crates/mujoco-sys/README.md).",
        .tried.as_ref().map(|p| format!(" from {}", p.display())).unwrap_or_default(),
        expected_version_string()
    )]
    Library {
        tried: Option<PathBuf>,
        #[source]
        source: libloading::Error,
    },

    #[error(
        "MuJoCo version mismatch: loaded library is {found_string} ({found}), but these \
         bindings are generated for {} ({EXPECTED_VERSION}). \
         mjModel's layout is version-specific, so this cannot be read safely — install \
         the matching release, or regenerate with packages/crates/mujoco-sys/regen-bindings.sh.",
        expected_version_string()
    )]
    VersionMismatch { found: i32, found_string: String },

    #[error("model path is not valid UTF-8 / contains a NUL: {0}")]
    BadPath(PathBuf),

    #[error("MuJoCo failed to load {path}: {message}")]
    LoadFailed { path: PathBuf, message: String },
}

/// A loaded `libmujoco`, and the only way to get a [`Model`].
pub struct Library {
    mj: bindings::Mujoco,
}

impl Library {
    /// Load MuJoCo from the first place it turns up:
    ///
    /// 1. `$MUJOCO_LIB` — a full path to the shared library itself;
    /// 2. `$MUJOCO_DIR` — an unpacked release root (macOS framework, Linux `lib/`,
    ///    Windows `bin/`);
    /// 3. the bare platform soname, i.e. wherever the system loader looks.
    pub fn load() -> Result<Self, Error> {
        let mut last: Option<Error> = None;
        for candidate in Self::candidates() {
            match unsafe { Self::load_from(&candidate) } {
                Ok(lib) => return Ok(lib),
                // A version mismatch means we FOUND MuJoCo and it is the wrong one —
                // reporting "not found" after that would be actively misleading.
                Err(e @ Error::VersionMismatch { .. }) => return Err(e),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or(Error::Library {
            tried: None,
            source: libloading::Error::DlOpenUnknown,
        }))
    }

    /// Load a specific shared-library file.
    ///
    /// # Safety
    /// Runs the library's initializers, like any `dlopen`. The path must be an
    /// actual MuJoCo build; the version check that follows catches the honest
    /// mistake (wrong release), not a hostile one.
    pub unsafe fn load_from(path: &Path) -> Result<Self, Error> {
        let mj = unsafe { bindings::Mujoco::new(path) }.map_err(|source| Error::Library {
            tried: Some(path.to_path_buf()),
            source,
        })?;

        let found = unsafe { mj.mj_version() };
        if found != EXPECTED_VERSION {
            let found_string = unsafe { CStr::from_ptr(mj.mj_versionString()) }
                .to_string_lossy()
                .into_owned();
            return Err(Error::VersionMismatch {
                found,
                found_string,
            });
        }
        Ok(Self { mj })
    }

    fn candidates() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(lib) = std::env::var_os("MUJOCO_LIB") {
            out.push(PathBuf::from(lib));
        }
        if let Some(dir) = std::env::var_os("MUJOCO_DIR") {
            let dir = PathBuf::from(dir);
            // macOS release: a framework bundle. The dylib inside carries the full
            // version in its name, so glob rather than guess.
            let framework = dir.join("mujoco.framework/Versions/A");
            if let Ok(entries) = std::fs::read_dir(&framework) {
                let mut dylibs: Vec<_> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("libmujoco.") && n.ends_with(".dylib"))
                    })
                    .collect();
                dylibs.sort();
                out.extend(dylibs);
            }
            // Linux / Windows release layouts.
            for rel in ["lib", "bin", "."] {
                let sub = dir.join(rel);
                if let Ok(entries) = std::fs::read_dir(&sub) {
                    let mut libs: Vec<_> = entries
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| {
                            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                                (n.starts_with("libmujoco.") || n == "mujoco.dll")
                                    && !n.ends_with(".a")
                            })
                        })
                        .collect();
                    libs.sort();
                    out.extend(libs);
                }
            }
        }
        out.push(PathBuf::from(if cfg!(target_os = "macos") {
            "libmujoco.dylib"
        } else if cfg!(target_os = "windows") {
            "mujoco.dll"
        } else {
            "libmujoco.so"
        }));
        out
    }

    /// e.g. `"3.11.0"`.
    pub fn version_string(&self) -> String {
        unsafe { CStr::from_ptr(self.mj.mj_versionString()) }
            .to_string_lossy()
            .into_owned()
    }

    /// Compile a model file: MJCF (`.xml`), URDF (`.urdf`), or a version-locked
    /// binary `.mjb`. We never look inside any of them — MuJoCo's compiler does all
    /// of it (defaults inheritance, mesh fitting, procedural textures).
    pub fn load_model(&self, path: &Path) -> Result<Model<'_>, Error> {
        let c_path = path
            .to_str()
            .and_then(|s| CString::new(s).ok())
            .ok_or_else(|| Error::BadPath(path.to_path_buf()))?;

        let is_binary = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("mjb"));

        let (ptr, message) = if is_binary {
            // mj_loadModel has no error-string out-param; it reports through the
            // error callback, which we don't install. A null is all we get.
            let ptr = unsafe { self.mj.mj_loadModel(c_path.as_ptr(), std::ptr::null()) };
            (ptr, String::from("mj_loadModel returned NULL"))
        } else {
            let mut err = vec![0i8; 1024];
            let ptr = unsafe {
                self.mj.mj_loadXML(
                    c_path.as_ptr(),
                    std::ptr::null(),
                    err.as_mut_ptr(),
                    err.len() as i32,
                )
            };
            let message = unsafe { CStr::from_ptr(err.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            (ptr, message)
        };

        if ptr.is_null() {
            return Err(Error::LoadFailed {
                path: path.to_path_buf(),
                message,
            });
        }
        Ok(Model { ptr, lib: self })
    }
}

impl std::fmt::Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library")
            .field("version", &self.version_string())
            .finish()
    }
}

/// A compiled `mjModel`, freed on drop.
pub struct Model<'lib> {
    ptr: *mut mjModel,
    lib: &'lib Library,
}

impl Drop for Model<'_> {
    fn drop(&mut self) {
        unsafe { (self.lib.mj.mj_deleteModel)(self.ptr) }
    }
}

/// Reading `mjModel` is a wall of `field: *T` + `count` pairs. Each accessor here
/// pairs a pointer field with the count that governs it and the per-element stride,
/// which is the *only* thing that can be wrong — so it is stated once, next to the
/// name, exactly as the header comments state it (`(ngeom x 3)`).
macro_rules! model_slices {
    ($( $(#[$m:meta])* $name:ident : $ty:ty = $count:ident x $stride:literal ),* $(,)?) => {
        $(
            $(#[$m])*
            pub fn $name(&self) -> &[$ty] {
                let raw = self.raw();
                unsafe { slice(raw.$name, (raw.$count as usize) * $stride) }
            }
        )*
    };
}

/// # Safety
/// `ptr` must be valid for `len` elements, or `len` must be 0 (MuJoCo leaves
/// pointers for empty arrays dangling/null, so the length check comes first).
unsafe fn slice<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 || ptr.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

impl Model<'_> {
    /// The raw struct, for fields without an accessor yet.
    pub fn raw(&self) -> &mjModel {
        unsafe { &*self.ptr }
    }

    pub fn nbody(&self) -> usize {
        self.raw().nbody as usize
    }
    pub fn ngeom(&self) -> usize {
        self.raw().ngeom as usize
    }
    pub fn nmesh(&self) -> usize {
        self.raw().nmesh as usize
    }
    pub fn nmat(&self) -> usize {
        self.raw().nmat as usize
    }
    pub fn ntex(&self) -> usize {
        self.raw().ntex as usize
    }
    pub fn nsite(&self) -> usize {
        self.raw().nsite as usize
    }
    pub fn ntendon(&self) -> usize {
        self.raw().ntendon as usize
    }
    pub fn nskin(&self) -> usize {
        self.raw().nskin as usize
    }
    pub fn nflex(&self) -> usize {
        self.raw().nflex as usize
    }
    pub fn nhfield(&self) -> usize {
        self.raw().nhfield as usize
    }

    model_slices! {
        /// `mjtGeom` discriminants; see [`geom_kind`](Self::geom_kind).
        geom_type: i32 = ngeom x 1,
        /// Visibility group. MuJoCo shows 0–2 by default; menagerie models put
        /// collision meshes in group 3 and visual meshes in 0/2.
        geom_group: i32 = ngeom x 1,
        /// Owning body — the node a geom hangs under, and what the pose stream drives.
        geom_bodyid: i32 = ngeom x 1,
        /// Mesh or heightfield index, `-1` for primitives.
        geom_dataid: i32 = ngeom x 1,
        /// Material index, `-1` when the geom carries its own `geom_rgba`.
        geom_matid: i32 = ngeom x 1,
        /// Type-dependent size parameters (radius/half-extents/half-length).
        geom_size: f64 = ngeom x 3,
        /// Offset from the owning body's frame.
        geom_pos: f64 = ngeom x 3,
        /// Offset rotation from the owning body's frame, `[w, x, y, z]`.
        geom_quat: f64 = ngeom x 4,
        /// Used only when `geom_matid` is `-1`.
        geom_rgba: f32 = ngeom x 4,

        body_parentid: i32 = nbody x 1,
        body_pos: f64 = nbody x 3,
        body_quat: f64 = nbody x 4,

        mat_rgba: f32 = nmat x 4,
        mat_specular: f32 = nmat x 1,
        mat_shininess: f32 = nmat x 1,
        mat_reflectance: f32 = nmat x 1,
        mat_emission: f32 = nmat x 1,
    }

    /// Typed geom kind, or `None` for a discriminant this MuJoCo knows and these
    /// bindings don't (the enum is `#[non_exhaustive]` for exactly that reason).
    pub fn geom_kind(&self, geom: usize) -> Option<mjtGeom> {
        use mjtGeom::*;
        Some(match self.geom_type()[geom] {
            0 => mjGEOM_PLANE,
            1 => mjGEOM_HFIELD,
            2 => mjGEOM_SPHERE,
            3 => mjGEOM_CAPSULE,
            4 => mjGEOM_ELLIPSOID,
            5 => mjGEOM_CYLINDER,
            6 => mjGEOM_BOX,
            7 => mjGEOM_MESH,
            8 => mjGEOM_SDF,
            _ => return None,
        })
    }

    /// Name of an object, or `None` when it was authored unnamed.
    pub fn name(&self, kind: mjtObj, id: usize) -> Option<&str> {
        let ptr = unsafe { (self.lib.mj.mj_id2name)(self.ptr, kind as i32, id as i32) };
        if ptr.is_null() {
            return None;
        }
        unsafe { CStr::from_ptr(ptr) }.to_str().ok()
    }

    /// Index of a named object, or `None`.
    pub fn id(&self, kind: mjtObj, name: &str) -> Option<usize> {
        let c_name = CString::new(name).ok()?;
        let id = unsafe { (self.lib.mj.mj_name2id)(self.ptr, kind as i32, c_name.as_ptr()) };
        (id >= 0).then_some(id as usize)
    }
}

impl std::fmt::Debug for Model<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model")
            .field("nbody", &self.nbody())
            .field("ngeom", &self.ngeom())
            .field("nmesh", &self.nmesh())
            .field("nmat", &self.nmat())
            .finish_non_exhaustive()
    }
}
