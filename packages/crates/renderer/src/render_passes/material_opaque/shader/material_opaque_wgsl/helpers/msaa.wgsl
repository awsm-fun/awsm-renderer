// ============================================================================
// MSAA Edge Detection - Optimized for Performance
// ============================================================================
//
// Strategy:
// 1. Normal check is done FIRST (cheap dot product, filters ~90% of neighbors)
// 2. Center depth is LAZY-LOADED only when first neighbor passes normal check
// 3. Depth check uses view-space for accuracy (only computed when needed)
// 4. MSAA sample check uses view-space depth (unavoidable for sub-pixel edges)
//
// Cost per pixel:
// - Non-edge (smooth surface): 4 normal loads + 4 dot products + 4 MSAA checks (~4 mat4×vec4)
// - Edge (normal discontinuity): Same as non-edge + early return = ~4 mat4×vec4
// - Edge (depth discontinuity): Above + 1-4 depth comparisons = ~4-8 mat4×vec4
//
// Key optimization: Lazy-loading center depth means most pixels avoid it entirely!
//
// ============================================================================

// MSAA sample count from template (0 if no MSAA)
const MSAA_SAMPLES : u32 = {{ msaa_sample_count }};

// Edge detection thresholds - tune these for your scene
const EDGE_NORMAL_THRESHOLD: f32 = 0.95;          // ~18° angle difference (dot product) - more sensitive
const EDGE_DEPTH_THRESHOLD: f32 = 0.02;           // 2% relative depth difference in view-space - more sensitive
const EDGE_MSAA_DEPTH_THRESHOLD: f32 = 0.02;      // 2% relative difference between MSAA samples (view-space)

// Main entry point - pass in already-calculated values to avoid redundant work
fn depth_edge_mask(
  camera: Camera,
  coords: vec2<i32>,
  pixel_center: vec2<f32>,
  screen_dims_f32: vec2<f32>,
  center_normal: vec3<f32>,
  center_triangle_id: u32
) -> bool {
  return edge_mask_depth_msaa(camera, coords, pixel_center, screen_dims_f32)
      || edge_mask_neighbors(camera, coords, pixel_center, screen_dims_f32, center_normal, center_triangle_id);
}

fn edge_mask_neighbors(
  camera: Camera,
  coords: vec2<i32>,
  pixel_center: vec2<f32>,
  screen_dims_f32: vec2<f32>,
  center_normal: vec3<f32>,
  center_triangle_id: u32
) -> bool {
  // Early exit if center pixel is not covered
  if (center_triangle_id == U32_MAX) {
    return false;
  }

  // Lazy-load center depth only if needed (after normal check fails)
  var center_depth_loaded = false;
  var depth_c: f32;
  var view_depth_c: f32;
  var depth_threshold: f32;

  // Check all 4 neighbors (right, left, down, up) to catch all edges
  let neighbor_offsets = array<vec2<i32>, 4>(
    vec2<i32>(1, 0),   // right
    vec2<i32>(-1, 0),  // left
    vec2<i32>(0, 1),   // down
    vec2<i32>(0, -1)   // up
  );

  let pixel_offsets = array<vec2<f32>, 4>(
    vec2<f32>(1.0, 0.0),
    vec2<f32>(-1.0, 0.0),
    vec2<f32>(0.0, 1.0),
    vec2<f32>(0.0, -1.0)
  );

  for (var i = 0; i < 4; i++) {
    let neighbor_coords = coords + neighbor_offsets[i];
    let neighbor_id = sampleTriangleId(neighbor_coords, 0);

    if (neighbor_id != U32_MAX) {
      let packed_nt_neighbor = textureLoad(normal_tangent_tex, neighbor_coords, 0);
      let tbn_neighbor = unpack_normal_tangent(packed_nt_neighbor);
      let neighbor_normal = tbn_neighbor.N;

      // Check normal discontinuity first (cheapest - just a dot product)
      if (dot(center_normal, neighbor_normal) < EDGE_NORMAL_THRESHOLD) {
        return true;
      }

      // Only check depth if normals are similar (lazy-load center depth on first need)
      if (!center_depth_loaded) {
        depth_c = textureLoad(depth_tex, coords, 0);
        view_depth_c = viewSpaceDepth(camera, depth_c, pixel_center, screen_dims_f32);
        depth_threshold = EDGE_DEPTH_THRESHOLD * abs(view_depth_c);
        center_depth_loaded = true;
      }

      // Check depth discontinuity in view-space
      let neighbor_depth = textureLoad(depth_tex, neighbor_coords, 0);
      let neighbor_pixel_center = pixel_center + pixel_offsets[i];
      let neighbor_view_depth = viewSpaceDepth(camera, neighbor_depth, neighbor_pixel_center, screen_dims_f32);

      if (abs(view_depth_c - neighbor_view_depth) > depth_threshold) {
        return true;
      }
    } else {
      // Neighbor is background - this is an edge
      return true;
    }
  }

  return false;
}

// Detect edges within a pixel by checking MSAA sample depth variation
// This catches sub-pixel edges that neighbor checks would miss
// IMPORTANT: Only returns true for cross-triangle edges to preserve texture detail
fn edge_mask_depth_msaa(
  camera: Camera,
  coords: vec2<i32>,
  pixel_center: vec2<f32>,
  screen_dims_f32: vec2<f32>
) -> bool {
  var sample_count = 0u;
  var dmin =  1e9;
  var dmax = -1e9;

  // Check all MSAA samples for depth variation and triangle IDs
  // View-space depth needed here since samples at same screen position but different depths
  for (var s = 0u; s < {{ msaa_sample_count }}u; s++) {
    if (sampleCoveredU(coords, s)) {
      sample_count++;
      let depth = loadDepthSample(coords, s);
      let view_depth = viewSpaceDepth(camera, depth, pixel_center, screen_dims_f32);
      dmin = min(dmin, view_depth);
      dmax = max(dmax, view_depth);
    }
  }

  // If less than 2 samples covered, no edge within pixel
  if (sample_count < 2u) { return false; }

  // Check if samples have significant depth variation (indicates edge within pixel)
  let depth_range = abs(dmax - dmin);
  let avg_depth = abs((dmax + dmin) * 0.5);
  return depth_range > (EDGE_MSAA_DEPTH_THRESHOLD * avg_depth);
}


fn sampleTriangleId(coords: vec2<i32>, s: i32) -> u32 {
  let v = textureLoad(visibility_data_tex, coords, s);
  return join32(v.x, v.y);
}

fn sampleCovered(coords: vec2<i32>, s: i32) -> bool {
  return sampleTriangleId(coords, s) != U32_MAX;
}

// Version that takes u32 sample index and uses switch for texture load
fn sampleTriangleIdU(coords: vec2<i32>, s: u32) -> u32 {
  var v: vec4<u32>;
  switch(s) {
    case 0u: { v = textureLoad(visibility_data_tex, coords, 0); }
    case 1u: { v = textureLoad(visibility_data_tex, coords, 1); }
    case 2u: { v = textureLoad(visibility_data_tex, coords, 2); }
    case 3u, default: { v = textureLoad(visibility_data_tex, coords, 3); }
  }
  return join32(v.x, v.y);
}

fn sampleCoveredU(coords: vec2<i32>, s: u32) -> bool {
  return sampleTriangleIdU(coords, s) != U32_MAX;
}

fn loadDepthSample(coords: vec2<i32>, s: u32) -> f32 {
  var depth: f32;
  switch(s) {
    case 0u: { depth = textureLoad(depth_tex, coords, 0); }
    case 1u: { depth = textureLoad(depth_tex, coords, 1); }
    case 2u: { depth = textureLoad(depth_tex, coords, 2); }
    case 3u, default: { depth = textureLoad(depth_tex, coords, 3); }
  }
  return depth;
}

// Convert depth buffer value to view-space depth (negative Z in view space)
// This gives us linear depth that we can meaningfully compare across the scene
fn viewSpaceDepth(camera: Camera, depth: f32, pixel_coords: vec2<f32>, screen_dims: vec2<f32>) -> f32 {
  // Reconstruct NDC coordinates (optimized to single expression)
  let ndc_xy = vec2<f32>(
    (pixel_coords.x / screen_dims.x) * 2.0 - 1.0,
    1.0 - (pixel_coords.y / screen_dims.y) * 2.0
  );

  // Reconstruct clip-space position (WebGPU uses depth range [0, 1])
  let clip_pos = vec4<f32>(ndc_xy, depth, 1.0);

  // Transform to view space and apply perspective divide
  let view_pos = camera.inv_proj * clip_pos;
  return view_pos.z / view_pos.w;
}

// Check if we should use MSAA resolve for this pixel
// Returns the number of samples to process (1 for non-edge, MSAA_SAMPLES for edge)
fn msaa_sample_count_for_pixel(
  camera: Camera,
  coords: vec2<i32>,
  pixel_center: vec2<f32>,
  screen_dims_f32: vec2<f32>,
  center_normal: vec3<f32>,
  center_triangle_id: u32
) -> u32 {
  // If not multisampled, always return 1
  if (MSAA_SAMPLES == 0u) {
    return 1u;
  }

  // If sample 0 is skybox, check if any other sample has geometry
  // This handles silhouette edges where sample 0 might be skybox
  if (center_triangle_id == U32_MAX) {
    // Check if any sample hit geometry (starting from sample 1)
    for (var s = 1u; s < {{ msaa_sample_count }}u; s++) {
      if (sampleCoveredU(coords, s)) {
        return MSAA_SAMPLES; // Force MSAA resolve
      }
    }
    return 1u; // All samples are skybox
  }

  // Check if this is an edge pixel
  let is_edge = depth_edge_mask(camera, coords, pixel_center, screen_dims_f32, center_normal, center_triangle_id);

  if (is_edge) {
    return MSAA_SAMPLES;
  } else {
    return 1u;
  }
}
