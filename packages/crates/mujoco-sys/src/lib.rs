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

pub use bindings::{mjData, mjModel, mjtGeom, mjtObj, mjtWrap};

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
    pub fn nflexvert(&self) -> usize {
        self.raw().nflexvert as usize
    }
    pub fn nflexelemdata(&self) -> usize {
        self.raw().nflexelemdata as usize
    }
    pub fn nflexshelldata(&self) -> usize {
        self.raw().nflexshelldata as usize
    }
    pub fn nflextexcoord(&self) -> usize {
        self.raw().nflextexcoord as usize
    }
    pub fn nhfield(&self) -> usize {
        self.raw().nhfield as usize
    }
    pub fn nmeshvert(&self) -> usize {
        self.raw().nmeshvert as usize
    }
    pub fn nmeshnormal(&self) -> usize {
        self.raw().nmeshnormal as usize
    }
    pub fn nmeshtexcoord(&self) -> usize {
        self.raw().nmeshtexcoord as usize
    }
    pub fn nmeshface(&self) -> usize {
        self.raw().nmeshface as usize
    }
    pub fn nhfielddata(&self) -> usize {
        self.raw().nhfielddata as usize
    }
    pub fn nwrap(&self) -> usize {
        self.raw().nwrap as usize
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

        // Meshes are stored OBJ-style: one shared pool per attribute, sliced per
        // mesh by an (adr, num) pair, with SEPARATE face-index arrays for
        // positions/normals/texcoords. glTF needs one index per vertex tuple, so a
        // consumer has to de-index; see the exporter's `mesh` module.
        /// First vertex of mesh `i` in [`mesh_vert`](Self::mesh_vert).
        mesh_vertadr: i32 = nmesh x 1,
        mesh_vertnum: i32 = nmesh x 1,
        /// First face of mesh `i` in [`mesh_face`](Self::mesh_face).
        mesh_faceadr: i32 = nmesh x 1,
        mesh_facenum: i32 = nmesh x 1,
        mesh_normaladr: i32 = nmesh x 1,
        mesh_normalnum: i32 = nmesh x 1,
        /// `-1` when the mesh has no texture coordinates.
        mesh_texcoordadr: i32 = nmesh x 1,
        mesh_texcoordnum: i32 = nmesh x 1,
        /// Post-compile vertex positions — already recentred to the mesh's own
        /// frame, which is why geom_pos/geom_quat of mesh geoms are not the
        /// authored ones. Only ever pair these two together.
        mesh_vert: f32 = nmeshvert x 3,
        mesh_normal: f32 = nmeshnormal x 3,
        mesh_texcoord: f32 = nmeshtexcoord x 2,
        /// Position indices, **relative to the mesh's own `mesh_vertadr`** — add
        /// the mesh's base before indexing the shared pool. (The exporter's tests
        /// assert this against the per-mesh counts, so an upstream change to the
        /// convention fails loudly instead of reading a neighbouring mesh.)
        mesh_face: i32 = nmeshface x 3,
        /// Normal indices for the same faces, relative to `mesh_normaladr`.
        mesh_facenormal: i32 = nmeshface x 3,
        /// Texcoord indices for the same faces, relative to `mesh_texcoordadr`.
        mesh_facetexcoord: i32 = nmeshface x 3,

        // Spatial tendons. A tendon's path is a list of wrap OBJECTS (sites,
        // geoms it wraps around, pulleys); at runtime each contributes up to TWO
        // points, which is why `wrap_xpos` is `nwrap x 6`. That factor of two is
        // the compile-time bound on how many waypoints a tendon can ever have —
        // the number the renderer preallocates segment nodes for.
        tendon_adr: i32 = ntendon x 1,
        tendon_num: i32 = ntendon x 1,
        tendon_matid: i32 = ntendon x 1,
        tendon_group: i32 = ntendon x 1,
        /// Rendering radius, metres.
        tendon_width: f64 = ntendon x 1,
        tendon_rgba: f32 = ntendon x 4,
    /// Per wrap OBJECT (not per tendon): an `mjtWrap` discriminant. A tendon
    /// whose objects are `mjWRAP_JOINT` is a FIXED tendon — a joint-coupling
    /// constraint with no path through space — which is why the exporter has to
    /// read this rather than assume every tendon is drawable.
    wrap_type: i32 = nwrap x 1,

        // Flexes: MuJoCo's deformable primitive, and the only one that survives
        // into 3.x — `mjSkin` is legacy and no shipped model or menagerie robot
        // uses it. A flex is a soup of vertices (each rigidly attached to a
        // body) plus elements: edges for `dim` 1, TRIANGLES for 2 (cloth),
        // tetrahedra for 3 (solids, whose visible surface is `flex_shell`).
        /// 1 = edges, 2 = triangles, 3 = tetrahedra.
        flex_dim: i32 = nflex x 1,
        flex_matid: i32 = nflex x 1,
        flex_group: i32 = nflex x 1,
        /// Vertex/contact radius, metres. A cloth is a zero-thickness surface
        /// inflated by this.
        flex_radius: f64 = nflex x 1,
        flex_rgba: f32 = nflex x 4,
        flex_vertadr: i32 = nflex x 1,
        flex_vertnum: i32 = nflex x 1,
        flex_elemadr: i32 = nflex x 1,
        flex_elemnum: i32 = nflex x 1,
        /// Where this flex's slice of the shared `flex_elem` pool starts —
        /// SEPARATE from `flex_elemadr`, which counts elements rather than the
        /// ints they occupy.
        flex_elemdataadr: i32 = nflex x 1,
        flex_shellnum: i32 = nflex x 1,
        flex_shelldataadr: i32 = nflex x 1,
        flex_texcoordadr: i32 = nflex x 1,
        /// The BODY each flex vertex is rigidly attached to. This is the whole
        /// deformation model: a flex deforms because its vertices belong to
        /// different bodies that move independently.
        flex_vertbodyid: i32 = nflexvert x 1,
        /// Element vertex indices, `dim + 1` per element, relative to the
        /// flex's own vertex range.
        flex_elem: i32 = nflexelemdata x 1,
        /// Surface fragment vertex indices, `dim` per fragment. For a 3D flex
        /// this is the visible skin; for a 2D one the elements already are.
        flex_shell: i32 = nflexshelldata x 1,
        flex_texcoord: f32 = nflextexcoord x 2,

        // Heightfields: an nrow x ncol elevation grid, sliced out of one shared
        // pool by (adr, nrow*ncol) — same pattern as meshes.
        /// `[x half-extent, y half-extent, z scale, z base]`, metres. Elevations
        /// are normalized 0..1 and multiplied by `z scale`; the solid base
        /// extends `z base` BELOW zero.
        hfield_size: f64 = nhfield x 4,
        hfield_nrow: i32 = nhfield x 1,
        hfield_ncol: i32 = nhfield x 1,
        /// First sample of hfield `i` in [`hfield_data`](Self::hfield_data).
        hfield_adr: i32 = nhfield x 1,
        /// Normalized elevations, row-major, all heightfields concatenated.
        hfield_data: f32 = nhfielddata x 1,

        // Sites are massless, non-colliding marker geoms: same shape as a geom,
        // their own id space, and driven by the same kind of pose stream.
        /// `mjtGeom` discriminant used to DRAW the site.
        site_type: i32 = nsite x 1,
        site_bodyid: i32 = nsite x 1,
        site_matid: i32 = nsite x 1,
        /// Site visibility group. Separate from geom groups; MuJoCo shows 0-2.
        site_group: i32 = nsite x 1,
        site_size: f64 = nsite x 3,
        site_pos: f64 = nsite x 3,
        site_quat: f64 = nsite x 4,
        site_rgba: f32 = nsite x 4,

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

    /// The model's own name (MJCF `<mujoco model="...">`).
    ///
    /// mjModel has no field for it: MuJoCo writes it as the first entry of the
    /// shared `names` blob, ahead of every object name.
    pub fn model_name(&self) -> Option<&str> {
        let names = self.raw().names;
        if names.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(names) }.to_str().ok()?;
        (!s.is_empty()).then_some(s)
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

/// A simulation state (`mjData`) for one model, freed on drop.
///
/// The exporter only ever uses this to evaluate the model's *initial*
/// configuration — one `mj_forward` at `qpos0` — so the sidecar can record where
/// each geom actually starts. Nothing here steps physics; running a simulation is
/// never this repo's job.
pub struct Data<'m, 'lib> {
    ptr: *mut mjData,
    model: &'m Model<'lib>,
}

impl Drop for Data<'_, '_> {
    fn drop(&mut self) {
        unsafe { (self.model.lib.mj.mj_deleteData)(self.ptr) }
    }
}

impl<'lib> Model<'lib> {
    /// Allocate simulation state and evaluate the model's initial configuration
    /// (`qpos0`), so the `*_x*` Cartesian fields are populated.
    ///
    /// `mj_forward` rather than `mj_step`: we want where the model *starts*, not
    /// where it falls to.
    pub fn forward_at_initial_pose<'m>(&'m self) -> Result<Data<'m, 'lib>, Error> {
        self.make_data(None)
    }

    /// Same, but starting from one of the model's authored keyframes (MJCF
    /// `<key>`). Most menagerie models ship a `home` pose as keyframe 0, which is
    /// a far better starting configuration than `qpos0` for a walking robot.
    pub fn reset_to_keyframe<'m>(&'m self, key: i32) -> Result<Data<'m, 'lib>, Error> {
        self.make_data(Some(key))
    }

    fn make_data<'m>(&'m self, key: Option<i32>) -> Result<Data<'m, 'lib>, Error> {
        let ptr = unsafe { (self.lib.mj.mj_makeData)(self.ptr) };
        if ptr.is_null() {
            return Err(Error::LoadFailed {
                path: PathBuf::from("<mjData>"),
                message: "mj_makeData returned NULL".into(),
            });
        }
        let data = Data { ptr, model: self };
        unsafe {
            match key {
                Some(k) => (self.lib.mj.mj_resetDataKeyframe)(self.ptr, ptr, k),
                None => (self.lib.mj.mj_resetData)(self.ptr, ptr),
            }
            (self.lib.mj.mj_forward)(self.ptr, ptr);
        }
        Ok(data)
    }

    /// The model's integration timestep, in seconds (MJCF `<option timestep>`).
    pub fn timestep(&self) -> f64 {
        self.raw().opt.timestep
    }
}

/// Same pairing rule as [`model_slices`], against `mjData` — the counts still
/// live on the model.
macro_rules! data_slices {
    ($( $(#[$m:meta])* $name:ident : $ty:ty = $count:ident x $stride:literal ),* $(,)?) => {
        $(
            $(#[$m])*
            pub fn $name(&self) -> &[$ty] {
                let raw = unsafe { &*self.ptr };
                unsafe { slice(raw.$name, self.model.$count() * $stride) }
            }
        )*
    };
}

impl Data<'_, '_> {
    /// The raw struct, for fields without an accessor yet.
    pub fn raw(&self) -> &mjData {
        unsafe { &*self.ptr }
    }

    /// Advance the simulation by one `timestep`.
    ///
    /// The only place this repo's tools run physics at all, and they are native
    /// offline tools: no MuJoCo ever executes in the renderer or the editor.
    pub fn step(&mut self) {
        unsafe { (self.model.lib.mj.mj_step)(self.model.ptr, self.ptr) }
    }

    data_slices! {
        /// Body frame world positions.
        xpos: f64 = nbody x 3,
        /// Body frame world orientations, `[w, x, y, z]`.
        xquat: f64 = nbody x 4,
        /// Geom world positions — what the pose stream will later carry.
        geom_xpos: f64 = ngeom x 3,
        /// Geom world orientations as **row-major 3x3 matrices**, not quaternions;
        /// mjData has no `geom_xquat`. See [`Self::geom_world_quat`].
        geom_xmat: f64 = ngeom x 9,
        /// Site world positions — sites stream exactly like geoms, on their own
        /// id space.
        site_xpos: f64 = nsite x 3,
        /// Site world orientations, row-major 3x3. See [`Self::site_world_quat`].
        site_xmat: f64 = nsite x 9,
        /// First wrap point of tendon `i` in [`wrap_xpos`](Self::wrap_xpos).
        ten_wrapadr: i32 = ntendon x 1,
        /// How many wrap points tendon `i` has **this frame** — it changes as the
        /// tendon wraps and unwraps around geometry, which is the whole reason
        /// segment nodes are preallocated and hidden rather than created.
        ten_wrapnum: i32 = ntendon x 1,
        /// Every tendon's wrap points, concatenated. Sliced per tendon by
        /// (`ten_wrapadr`, `ten_wrapnum`); `nwrap x 6` because one wrap object
        /// can contribute two points.
        wrap_xpos: f64 = nwrap x 6,
    /// Flex vertex positions in WORLD space, recomputed every step. This is the
    /// flex stream channel: a deformable's entire visual state.
    flexvert_xpos: f64 = nflexvert x 3,
    }

    /// A site's world orientation as a `[w, x, y, z]` quaternion.
    pub fn site_world_quat(&self, site: usize) -> [f64; 4] {
        mat3_to_quat(&self.site_xmat()[site * 9..site * 9 + 9])
    }

    /// A geom's world orientation as a `[w, x, y, z]` quaternion, converted from
    /// `geom_xmat`.
    ///
    /// The matrix is orthonormal by construction, so this is the standard
    /// branchful trace conversion — picking the largest denominator keeps it
    /// stable when the trace is near -1.
    pub fn geom_world_quat(&self, geom: usize) -> [f64; 4] {
        mat3_to_quat(&self.geom_xmat()[geom * 9..geom * 9 + 9])
    }
}

/// Row-major 3x3 rotation matrix → `[w, x, y, z]` quaternion.
///
/// mjData stores orientations as matrices and has no `*_xquat` for geoms or
/// sites, so this is the only way across. The matrix is orthonormal by
/// construction; picking the largest denominator keeps the conversion stable
/// when the trace is near -1.
fn mat3_to_quat(m: &[f64]) -> [f64; 4] {
    let (m00, m01, m02) = (m[0], m[1], m[2]);
    let (m10, m11, m12) = (m[3], m[4], m[5]);
    let (m20, m21, m22) = (m[6], m[7], m[8]);
    let trace = m00 + m11 + m22;
    if trace > 0.0 {
        let s = 0.5 / (trace + 1.0).sqrt();
        [0.25 / s, (m21 - m12) * s, (m02 - m20) * s, (m10 - m01) * s]
    } else if m00 > m11 && m00 > m22 {
        let s = 2.0 * (1.0 + m00 - m11 - m22).sqrt();
        [(m21 - m12) / s, 0.25 * s, (m01 + m10) / s, (m02 + m20) / s]
    } else if m11 > m22 {
        let s = 2.0 * (1.0 + m11 - m00 - m22).sqrt();
        [(m02 - m20) / s, (m01 + m10) / s, 0.25 * s, (m12 + m21) / s]
    } else {
        let s = 2.0 * (1.0 + m22 - m00 - m11).sqrt();
        [(m10 - m01) / s, (m02 + m20) / s, (m12 + m21) / s, 0.25 * s]
    }
}
