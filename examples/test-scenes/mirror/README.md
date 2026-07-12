# mirror — perfect-mirror SSR acceptance scene

Silver mirror floor (metallic 1.0, roughness 0.0 — a metal's reflection is
tinted by base color, so 'mirror' requires a near-WHITE base; a black-base
metal is a physically-correct black mirror). Probes: floating white emissive
sphere (curved-silhouette tangency), red box resting on the floor (contact
line), thin torus (thin-geometry acceptance). Bloom OFF, temporal OFF,
full-res SSR, 128 steps: the mirror is judged bare.

Acceptance (1:1 native crops): floor reflects the sky seamlessly; object
reflections REPLACE the sky reflection (IBL specular dedup); reflections are
geometry-sharp with only ~1px antialiasing at silhouette tangency; the torus
ring is continuous; the box contact has no teeth.
