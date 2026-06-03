// Fragment shader version - reads interpolated vertex colors from fragment input
// Hardware has already interpolated the colors during rasterization

// Get the interpolated vertex color for a given color set
// In the transparent pass, colors are already interpolated by hardware
// and available directly in the fragment input as color_0, color_1, etc.
fn vertex_color(color_info: VertexColorInfo, fragment_input: FragmentInput) -> vec4<f32> {
    {% if let Some(color_sets) = color_sets %}
        // Select the appropriate color set based on color_info.set_index
        {% for i in 0..*color_sets %}
            if color_info.set_index == {{ i }}u {
                return fragment_input.color_{{ i }};
            }
        {% endfor %}
        // Fallback if set_index is out of range
        return vec4<f32>(1.0);
    {% else %}
        // No color sets available
        return vec4<f32>(1.0);
    {% endif %}
}
